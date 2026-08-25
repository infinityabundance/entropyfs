//! Physical segment reconciliation (Phase-9H).
//!
//! The derived `ObjectIndex` maps each `ChunkId` to ONE canonical
//! location, so index-based accounting (`gc::live_ratios`,
//! `gc::unreachable_bytes`) can diverge from what is actually on disk:
//!
//! - a record whose payload was appended again elsewhere is *index-hidden*
//!   (the index points at the newer copy; the older physical copy still
//!   occupies bytes);
//! - records in deleted/replaced B-tree paths stay indexed but unreachable;
//! - torn tails, zero padding, and the 4-byte magic are never indexed.
//!
//! This module scans every segment file independently of the index and
//! reconciles the physical bytes exactly:
//!
//! ```text
//! file_bytes
//!   = live_bytes            (canonical record, live set)
//!   + dead_indexed_bytes    (canonical record, not live)
//!   + index_hidden_bytes    (valid record shadowed by a newer location)
//!   + unindexed_bytes       (valid record with no index entry at all)
//!   + torn_bytes            (tail that fails envelope validation)
//!   + zero_padding_bytes    (all-zero tail)
//!   + format_overhead       (magic + any unclassified bytes)
//! ```
//!
//! `gc` victim selection uses this scan so the denominator is the actual
//! physical occupancy, not the index's view of it.

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::path::Path;

use crate::core::extent::ChunkId;
use crate::store::segment::{self, ScanRecord};
use crate::store::{Store, StoreError};

/// Per-segment physical reconciliation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SegmentPhysical {
    /// Segment sequence number.
    pub seq: u64,
    /// Segment file size (bytes on disk).
    pub file_bytes: u64,
    /// Records whose index location == this physical record.
    pub canonical_records: u64,
    /// Canonical records whose id is in the live set.
    pub live_bytes: u64,
    /// Canonical records whose id is NOT in the live set.
    pub dead_indexed_bytes: u64,
    /// Valid records shadowed by a newer location for the same id.
    pub index_hidden_bytes: u64,
    /// Valid records with no index entry at all.
    pub unindexed_bytes: u64,
    /// Tail bytes that fail envelope validation (torn write).
    pub torn_bytes: u64,
    /// All-zero tail bytes.
    pub zero_padding_bytes: u64,
    /// Everything else (magic, unclassified).
    pub format_overhead_bytes: u64,
}

impl SegmentPhysical {
    /// Bytes that are fully reclaimable: dead canonical, index-hidden,
    /// unindexed, torn, and padding (the live records must be copied out
    /// first; that copy cost is the compaction price).
    pub fn reclaimable_bytes(&self) -> u64 {
        self.dead_indexed_bytes
            .saturating_add(self.index_hidden_bytes)
            .saturating_add(self.unindexed_bytes)
            .saturating_add(self.torn_bytes)
            .saturating_add(self.zero_padding_bytes)
    }

    /// The physical live ratio used for victim selection: canonical live
    /// bytes over ALL physically present record bytes (index-hidden and
    /// unindexed records lower it even though the index cannot see them).
    pub fn physical_live_ratio(&self) -> f64 {
        let total = self
            .live_bytes
            .saturating_add(self.dead_indexed_bytes)
            .saturating_add(self.index_hidden_bytes)
            .saturating_add(self.unindexed_bytes);
        if total == 0 {
            return 1.0; // empty segments are reclaimable as a whole
        }
        self.live_bytes as f64 / total as f64
    }

    /// The index-based ratio (the old `live_ratios` view): live canonical
    /// over canonical only. Index-hidden/unindexed bytes are invisible.
    pub fn index_live_ratio(&self) -> f64 {
        let total = self.live_bytes.saturating_add(self.dead_indexed_bytes);
        if total == 0 {
            return 1.0;
        }
        self.live_bytes as f64 / total as f64
    }
}

/// Whole-store reconciliation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicalReport {
    /// Per-segment table, ascending sequence.
    pub segments: Vec<SegmentPhysical>,
    /// Total file bytes across all segments.
    pub file_bytes: u64,
    /// Sum of canonical live record bytes.
    pub live_bytes: u64,
    /// Sum of canonical dead (unreachable) record bytes.
    pub dead_indexed_bytes: u64,
    /// Sum of index-hidden record bytes.
    pub index_hidden_bytes: u64,
    /// Sum of unindexed record bytes.
    pub unindexed_bytes: u64,
    /// Sum of torn bytes.
    pub torn_bytes: u64,
    /// Sum of zero padding.
    pub zero_padding_bytes: u64,
    /// Sum of format overhead.
    pub format_overhead_bytes: u64,
}

impl PhysicalReport {
    /// The index's accounting of the same store (live canonical + dead
    /// canonical), for the before/after diagnostic.
    pub fn index_accounted_bytes(&self) -> u64 {
        self.live_bytes.saturating_add(self.dead_indexed_bytes)
    }

    /// Reconciliation residual: file bytes not explained by any category.
    /// Must be zero for a well-formed store.
    pub fn unexplained(&self) -> u64 {
        self.file_bytes.saturating_sub(
            self.live_bytes
                .saturating_add(self.dead_indexed_bytes)
                .saturating_add(self.index_hidden_bytes)
                .saturating_add(self.unindexed_bytes)
                .saturating_add(self.torn_bytes)
                .saturating_add(self.zero_padding_bytes)
                .saturating_add(self.format_overhead_bytes),
        )
    }
}

fn classify(
    rec: &ScanRecord,
    seq: u64,
    live: &HashSet<ChunkId>,
    index: &crate::store::object::ObjectIndex,
) -> (bool, bool) {
    // (is_canonical, is_live): the record is canonical when the index's
    // single location for its content id points exactly at this physical
    // copy.
    match index.get(&rec.content_id) {
        Some(loc) if loc.segment_seq == seq && loc.offset == rec.offset => {
            (true, live.contains(&rec.content_id))
        }
        _ => (false, false),
    }
}

/// Extension helper: the scanner needs the record's segment; records carry
/// only their offset, so classification happens per segment.
fn scan_segment_classified(
    path: &Path,
    seq: u64,
    limit_records: u64,
    live: &HashSet<ChunkId>,
    index: &crate::store::object::ObjectIndex,
) -> Result<SegmentPhysical, StoreError> {
    let file_len = std::fs::metadata(path)
        .map(|m| m.len())
        .map_err(|e| StoreError::Io(e.to_string()))?;
    let (records, clean_end) = segment::scan_segment(path, limit_records)?;
    let mut out = SegmentPhysical {
        seq,
        file_bytes: file_len,
        ..Default::default()
    };
    out.canonical_records = 0;
    let mut valid_bytes = 0u64;
    for rec in &records {
        let total = rec.total_size();
        valid_bytes = valid_bytes.saturating_add(total);
        let (canonical, is_live) = classify(rec, seq, live, index);
        if canonical {
            out.canonical_records += 1;
            if is_live {
                out.live_bytes = out.live_bytes.saturating_add(total);
            } else {
                out.dead_indexed_bytes = out.dead_indexed_bytes.saturating_add(total);
            }
        } else if index.get(&rec.content_id).is_some() {
            out.index_hidden_bytes = out.index_hidden_bytes.saturating_add(total);
        } else {
            out.unindexed_bytes = out.unindexed_bytes.saturating_add(total);
        }
    }
    let tail_bytes = file_len.saturating_sub(clean_end);
    if tail_bytes > 0 {
        // Distinguish an all-zero tail (padding) from a torn partial
        // record by sampling the tail.
        let bytes = std::fs::read(path).map_err(|e| StoreError::Io(e.to_string()))?;
        let tail = &bytes[clean_end as usize..];
        if tail.iter().all(|&b| b == 0) {
            out.zero_padding_bytes = tail_bytes;
        } else {
            out.torn_bytes = tail_bytes;
        }
    }
    out.format_overhead_bytes = file_len
        .saturating_sub(valid_bytes)
        .saturating_sub(tail_bytes);
    Ok(out)
}

/// Scan every segment file and reconcile the physical bytes (Phase-9H).
/// `live` is the mark set from `gc::mark_live`.
pub fn scan_physical(store: &Store, live: &HashSet<ChunkId>) -> Result<PhysicalReport, StoreError> {
    let index = store.object_index();
    let mut report = PhysicalReport::default();
    for seq in segment::list_segments(store.dir())? {
        let seg = scan_segment_classified(
            &segment::segment_path(store.dir(), seq),
            seq,
            store.config().max_records_per_segment,
            live,
            index,
        )?;
        report.file_bytes = report.file_bytes.saturating_add(seg.file_bytes);
        report.live_bytes = report.live_bytes.saturating_add(seg.live_bytes);
        report.dead_indexed_bytes = report
            .dead_indexed_bytes
            .saturating_add(seg.dead_indexed_bytes);
        report.index_hidden_bytes = report
            .index_hidden_bytes
            .saturating_add(seg.index_hidden_bytes);
        report.unindexed_bytes = report.unindexed_bytes.saturating_add(seg.unindexed_bytes);
        report.torn_bytes = report.torn_bytes.saturating_add(seg.torn_bytes);
        report.zero_padding_bytes = report
            .zero_padding_bytes
            .saturating_add(seg.zero_padding_bytes);
        report.format_overhead_bytes = report
            .format_overhead_bytes
            .saturating_add(seg.format_overhead_bytes);
        report.segments.push(seg);
    }
    Ok(report)
}

/// Convenience: mark + scan in one call.
pub fn physical_report(store: &Store) -> Result<PhysicalReport, StoreError> {
    let live = crate::store::gc::mark_live(store)?;
    scan_physical(store, &live)
}

/// Render the reconciliation as a human-readable report.
pub fn render(report: &PhysicalReport) -> String {
    let mut out = String::new();
    let pct = |b: u64| {
        if report.file_bytes == 0 {
            return 0.0;
        }
        100.0 * b as f64 / report.file_bytes as f64
    };
    out.push_str(&format!(
        "physical reconciliation: {} B files\n",
        report.file_bytes
    ));
    out.push_str(&format!(
        "  live canonical      {:>12} B ({:5.1}%)\n",
        report.live_bytes,
        pct(report.live_bytes)
    ));
    out.push_str(&format!(
        "  dead indexed        {:>12} B ({:5.1}%)  [reclaimable]\n",
        report.dead_indexed_bytes,
        pct(report.dead_indexed_bytes)
    ));
    out.push_str(&format!(
        "  index-hidden        {:>12} B ({:5.1}%)  [reclaimable]\n",
        report.index_hidden_bytes,
        pct(report.index_hidden_bytes)
    ));
    out.push_str(&format!(
        "  unindexed           {:>12} B ({:5.1}%)  [reclaimable]\n",
        report.unindexed_bytes,
        pct(report.unindexed_bytes)
    ));
    out.push_str(&format!(
        "  torn                {:>12} B ({:5.1}%)\n",
        report.torn_bytes,
        pct(report.torn_bytes)
    ));
    out.push_str(&format!(
        "  zero padding        {:>12} B ({:5.1}%)\n",
        report.zero_padding_bytes,
        pct(report.zero_padding_bytes)
    ));
    out.push_str(&format!(
        "  format overhead     {:>12} B ({:5.1}%)\n",
        report.format_overhead_bytes,
        pct(report.format_overhead_bytes)
    ));
    out.push_str(&format!(
        "  unexplained         {:>12} B  [must be 0]\n",
        report.unexplained()
    ));
    out
}
