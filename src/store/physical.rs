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
//!
//! # Purpose
//!
//! The store is append-only and the derived `ObjectIndex` is not a census
//! of the disk: it maps each content id to ONE location, so it cannot see
//! older physical copies of a re-appended payload (index-hidden), records
//! that never made it into the index (unindexed), or bytes that are not
//! records at all (torn tails, padding, the 4-byte magic). This module is
//! the independent physical census: it scans every segment file and
//! partitions every byte into one of the mutually exclusive categories in
//! the equation above. GC victim selection and the `status` / `gc
//! --compact` diagnostics are built on this census rather than on the
//! index's view.
//!
//! # Boundary
//!
//! This module knows: segment files, the record envelope format
//! (`segment::scan_segment`), the live mark set (`gc::mark_live`), and the
//! derived object index (used only to decide which physical copy of a
//! content id is canonical). It must never know about logical
//! (uncompressed) bytes, block allocation, or the epoch: it is a pure
//! read-only accounting of committed, on-disk bytes.
//!
//! # Model
//!
//! Hold a segment file as a byte string. The scanner walks it from the
//! 4-byte magic, decodes records until the first offset that fails
//! validation, then every remaining byte is exactly one of: canonical-live,
//! canonical-dead (indexed), index-hidden, unindexed, torn, padding, or
//! format overhead. The identity `file_bytes == Σ categories` is the
//! module's contract and is re-checked by `PhysicalReport::unexplained`
//! (must be 0 for a well-formed store).
//!
//! # Persistent authority
//!
//! None. This module never writes segments, roots, or superblocks. Its
//! output is advisory — it feeds GC's victim selection and diagnostics —
//! and any GC decision made from it still commits through the normal
//! durability protocol.
//!
//! # Correctness invariants
//!
//! - A record is CANONICAL iff the index's single location for its content
//!   id points exactly at this physical copy (same segment seq + offset).
//!   Any other valid record is index-hidden if the id HAS an index entry
//!   elsewhere, and unindexed otherwise.
//! - The categories are mutually exclusive and jointly exhaustive of
//!   `file_bytes`; `unexplained() == 0` must hold after every scan of a
//!   healthy store.
//! - `live` is the reachability mark from `gc::mark_live` — the scanner
//!   classifies; it does not decide reachability.
//! - Every `*_bytes` field is PHYSICAL bytes including the record header
//!   (`HEADER_SIZE + stored_len`); never logical bytes, never
//!   allocated-block counts.
//!
//! # Concurrency
//!
//! Read-only against the store. Index lookups take one `ObjectIndex` shard
//! read lock and copy the `Location`, so no lock is held across a segment
//! read. The caller-supplied `live` set must be a mark consistent with the
//! index snapshot being scanned. A concurrent GC compaction that deletes or
//! rewrites segments mid-scan surfaces as a typed IO error or a mixed
//! point-in-time report; the CLI/evidence call sites run the scan while
//! the store is quiescent.
//!
//! # Durability
//!
//! None: the report is a memory-only diagnostic. It has no
//! acknowledgement, crash, or power-loss semantics of its own.
//!
//! # Resource bounds
//!
//! Per segment the scan is bounded by `config.max_records_per_segment`
//! (exceeding it fails the scan with a `CorruptRecord` error rather than
//! scanning unboundedly) and by the segment size cap the writer enforces.
//! Each file is read fully into memory once (a bounded multiple of the
//! segment size cap) and a second time for the tail sample.
//!
//! # Performance
//!
//! One full sequential read per segment plus one extra read for the tail
//! sample; O(bytes) classification, O(records) index lookups. This is
//! deliberately heavier than index accounting: it trades read cost for the
//! authoritative byte census that GC victim selection needs, because the
//! index's one-location view demonstrably understates garbage (see
//! HISTORY / EVIDENCE).
//!
//! # Failure modes
//!
//! A record envelope that fails validation in the MIDDLE of a file is a
//! `CorruptRecord` error — the scan fails hard, because interior corruption
//! is not a recoverable tail. Only a truncated envelope at the END of the
//! file becomes a torn tail. Missing/unreadable segment files and a
//! malformed magic produce typed errors. What must never happen:
//! `unexplained() != 0` on a healthy store — if the categories cannot
//! account for every byte, the census is wrong.
//!
//! # History / evidence
//!
//! Phase-9H (physical convergence), sealed campaign
//! `evidence/performance/campaign-1787688017-0a03ece/` (revision
//! `0a03ece`). On the real-tree court the post-GC dead bytes (2.66 MB)
//! were all `BtreeNode` records staged by the GC chunk-index REBUILD — the
//! old repeated-COW-insert rebuild physically wrote every intermediate
//! path version — and the index-hidden-dup hypothesis was FALSIFIED there
//! (index-hidden = 0). This scanner was the instrument that proved the
//! divergence: the derived index is a one-location view, so index-derived
//! occupancy understated the dead bytes actually on disk. The fixes that
//! followed (physical victim selection in `gc::physical_ratios`, the
//! `bulk_load` rebuild, `compact_full`) are measured in that campaign:
//! tree-court backing 9,129,988 B → 1,100,161 B; post-GC reconciliation =
//! reachable 1,100,157 B + 0 B dead + 0 B index-hidden + 0 B unindexed + 4
//! B format overhead.

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::path::Path;

use crate::core::extent::ChunkId;
use crate::store::segment::{self, ScanRecord};
use crate::store::{Store, StoreError};

/// Per-segment physical reconciliation: every byte of ONE segment file
/// classified into the mutually exclusive categories below.
///
/// # Units and accounting classes
///
/// Every `*_bytes` field is PHYSICAL bytes on disk (record totals include
/// the record header — `ScanRecord::total_size()`, i.e. header + stored
/// payload). The fields are jointly exhaustive: for a well-formed segment,
///
/// ```text
/// file_bytes == live_bytes + dead_indexed_bytes + index_hidden_bytes
///              + unindexed_bytes + torn_bytes + zero_padding_bytes
///              + format_overhead_bytes
/// ```
///
/// (`PhysicalReport::unexplained` asserts the whole-store version.)
/// "Allocated" disk blocks and "logical" (uncompressed) bytes are NOT
/// measured: `file_bytes` is the file length from `stat`.
///
/// The live/dead distinction is reachability: `live` is the mark set from
/// `gc::mark_live` (roots = current root + every snapshot), applied to the
/// record's content id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SegmentPhysical {
    /// Segment sequence number (monotonic; identifies the segment file).
    pub seq: u64,
    /// Physical file length in bytes (`stat` size; includes the 4-byte
    /// magic, every record, and any tail).
    pub file_bytes: u64,
    /// Count of records (not bytes) whose index location == this physical
    /// record.
    pub canonical_records: u64,
    /// Physical bytes of canonical records whose content id is in the
    /// live (root-reachable) set.
    pub live_bytes: u64,
    /// Physical bytes of canonical records whose content id is NOT in the
    /// live set — indexed but unreachable; reclaimable.
    pub dead_indexed_bytes: u64,
    /// Physical bytes of valid records shadowed by a newer location for
    /// the same content id (the index records one location; the older
    /// physical copy still occupies bytes); reclaimable.
    pub index_hidden_bytes: u64,
    /// Physical bytes of valid records with no index entry at all;
    /// reclaimable.
    pub unindexed_bytes: u64,
    /// Physical tail bytes that fail envelope validation (a truncated
    /// record at the end of the file).
    pub torn_bytes: u64,
    /// Physical all-zero tail bytes.
    pub zero_padding_bytes: u64,
    /// Physical bytes that are neither record nor tail: the 4-byte segment
    /// magic plus any unclassified interior bytes. NOT reclaimable by
    /// compaction — every segment file carries its magic (the 9H campaign's
    /// post-compact residual is exactly 4 B).
    pub format_overhead_bytes: u64,
}

impl SegmentPhysical {
    /// Reclaimable PHYSICAL bytes: dead canonical, index-hidden, unindexed,
    /// torn, and padding. All exclusive categories; units as in the struct.
    /// The live records must be copied out first — that copy cost is the
    /// compaction price. `format_overhead_bytes` is deliberately excluded:
    /// the 4-byte magic survives in every fresh segment, so it is not
    /// reclaimed by compacting this one.
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
    ///
    /// The denominator is record-bearing bytes only: torn, padding, and
    /// format bytes are excluded because they are reclaimable without any
    /// copy-out — the ratio measures the fraction of the segment that must
    /// be copied to reclaim it, which is what GC compares against
    /// `gc_target_ratio`. Empty of record bytes => 1.0 (nothing to copy,
    /// but the whole segment is still reclaimable as-is).
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
    /// over canonical only. Index-hidden/unindexed bytes are invisible,
    /// so this ratio can overstate liveness exactly where the physical
    /// scan does not — kept for the before/after diagnostic comparison.
    pub fn index_live_ratio(&self) -> f64 {
        let total = self.live_bytes.saturating_add(self.dead_indexed_bytes);
        if total == 0 {
            return 1.0;
        }
        self.live_bytes as f64 / total as f64
    }
}

/// Whole-store reconciliation: the per-segment table plus the summed
/// categories.
///
/// Same units and accounting classes as [`SegmentPhysical`]: every byte
/// field is PHYSICAL bytes, the categories are mutually exclusive and
/// jointly exhaustive, and `unexplained()` must be 0 for a healthy store.
/// The sums are inclusive of their per-segment parts (each is the Σ of the
/// homonymous `SegmentPhysical` field).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicalReport {
    /// Per-segment table, ascending sequence (`segment::list_segments`
    /// order).
    pub segments: Vec<SegmentPhysical>,
    /// Total physical file bytes across all segments (Σ file lengths).
    pub file_bytes: u64,
    /// Σ canonical live (root-reachable) record bytes.
    pub live_bytes: u64,
    /// Σ canonical dead (unreachable-but-indexed) record bytes.
    pub dead_indexed_bytes: u64,
    /// Σ index-hidden record bytes.
    pub index_hidden_bytes: u64,
    /// Σ unindexed record bytes.
    pub unindexed_bytes: u64,
    /// Σ torn bytes.
    pub torn_bytes: u64,
    /// Σ zero padding.
    pub zero_padding_bytes: u64,
    /// Σ format overhead (magic + unclassified).
    pub format_overhead_bytes: u64,
}

impl PhysicalReport {
    /// The index's accounting of the same store (live canonical + dead
    /// canonical), for the before/after diagnostic. Units: PHYSICAL record
    /// bytes. This is the most the derived index can ever see — one
    /// location per content id — so index-hidden and unindexed bytes are
    /// invisible to it; that gap is exactly what Phase-9H measured.
    pub fn index_accounted_bytes(&self) -> u64 {
        self.live_bytes.saturating_add(self.dead_indexed_bytes)
    }

    /// Reconciliation residual: physical file bytes not explained by any
    /// category. Units: PHYSICAL bytes. Must be zero for a well-formed
    /// store — this is the module's contract identity
    /// (`file_bytes == Σ categories`) re-checked at report level.
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

/// Classify one scanned record against the index and the live set.
///
/// Returns `(is_canonical, is_live)`. A record is canonical when the
/// index's single location for its content id points exactly at this
/// physical copy (same segment seq + offset); `is_live` is membership of
/// the content id in the reachability mark. Non-canonical records are
/// further classified by the caller: index-hidden if the id has an entry
/// elsewhere, unindexed if it has no entry at all.
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
///
/// Scans ONE segment file and classifies every byte into the
/// [`SegmentPhysical`] categories. `seq` is the segment's sequence number
/// (used for the canonicality comparison against the index),
/// `limit_records` bounds the envelope scan
/// (`config.max_records_per_segment`), `live` is the reachability mark,
/// and `index` is the derived object index.
fn scan_segment_classified(
    path: &Path,
    seq: u64,
    limit_records: u64,
    live: &HashSet<ChunkId>,
    index: &crate::store::object::ObjectIndex,
) -> Result<SegmentPhysical, StoreError> {
    // ---------------------------------------------------------------------
    // Stage 1: File length + sequential envelope scan.
    //
    // `scan_segment` walks records from the 4-byte magic and returns the
    // records plus `clean_end`, the first offset at which validation
    // stopped (zero padding / truncated tail / EOF). A mid-file envelope
    // error or an over-limit record count fails the scan hard.
    // ---------------------------------------------------------------------
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
    // ---------------------------------------------------------------------
    // Stage 2: Classify every scanned record.
    //
    // `valid_bytes` accumulates the PHYSICAL record totals (header + stored
    // payload) so format overhead can be derived in Stage 4. Each record
    // lands in exactly one category: canonical-live, canonical-dead
    // (indexed), index-hidden, or unindexed.
    // ---------------------------------------------------------------------
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
    // ---------------------------------------------------------------------
    // Stage 3: Classify the tail.
    //
    // Everything from `clean_end` to EOF is a non-record tail. Distinguish
    // an all-zero tail (padding) from a torn partial record by sampling
    // the tail bytes.
    // ---------------------------------------------------------------------
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
    // ---------------------------------------------------------------------
    // Stage 4: Derive format overhead.
    //
    // Every physical byte that is neither a validated record nor a tail:
    // the 4-byte segment magic and any unclassified interior bytes. This
    // term is what keeps the `file_bytes == Σ categories` identity exact.
    // ---------------------------------------------------------------------
    out.format_overhead_bytes = file_len
        .saturating_sub(valid_bytes)
        .saturating_sub(tail_bytes);
    Ok(out)
}

/// Scan every segment file and reconcile the physical bytes (Phase-9H).
/// `live` is the mark set from `gc::mark_live`.
///
/// This is the census GC victim selection is built on: `gc::collect` picks
/// victims from these scanned ratios (`gc::physical_ratios`) rather than
/// from the derived index, because the index is a one-location view that
/// cannot see index-hidden or unindexed garbage — the divergence Phase-9H
/// measured (evidence campaign `1787688017-0a03ece`; see the module doc).
pub fn scan_physical(store: &Store, live: &HashSet<ChunkId>) -> Result<PhysicalReport, StoreError> {
    let index = store.object_index();
    let mut report = PhysicalReport::default();
    // ---------------------------------------------------------------------
    // Stage 1: Scan and classify every segment file, in sequence order.
    //
    // Each segment is reconciled independently of the index; only the
    // canonical/live/hidden/unindexed decision consults it. A corrupt
    // segment fails the whole report rather than producing partial
    // accounting. The per-segment record bound (`max_records_per_segment`)
    // applies inside `scan_segment_classified`.
    // ---------------------------------------------------------------------
    for seq in segment::list_segments(store.dir())? {
        let seg = scan_segment_classified(
            &segment::segment_path(store.dir(), seq),
            seq,
            store.config().max_records_per_segment,
            live,
            index,
        )?;
        // -----------------------------------------------------------------
        // Stage 2: Aggregate the per-segment categories into the store
        // report.
        //
        // The report's sums are the exclusive partition of `file_bytes`
        // across segments; `PhysicalReport::unexplained` re-checks the
        // `file_bytes == Σ categories` identity on the totals.
        // -----------------------------------------------------------------
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

/// Convenience: mark + scan in one call. `live` is derived here, so the
/// report is self-contained (reachability + physical census from one
/// point-in-time view of the store).
pub fn physical_report(store: &Store) -> Result<PhysicalReport, StoreError> {
    let live = crate::store::gc::mark_live(store)?;
    scan_physical(store, &live)
}

/// Render the reconciliation as a human-readable report. All `* B` figures
/// are PHYSICAL bytes (the campaign's units: backing bytes, not allocated
/// blocks and not logical bytes); percentages are shares of
/// `report.file_bytes`.
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
