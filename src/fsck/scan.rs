//! fsck scan phase: independent raw scan of the persistent structures.
//!
//! This walks the superblock file and every segment file directly (never
//! through the mounted `Store` API), rebuilding the derived object index
//! exactly as mount would, and recording every anomaly as an issue
//! (`docs/recovery/fsck.md`).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::core::extent::ChunkId;
use crate::core::materialize::{DecoderContext, MaterializeError};
use crate::core::representation::{RansCodec, Representation, UniverseId};
use crate::format::codec::CodecError;
use crate::format::superblock::Superblock;
use crate::format::version::{RecordTag, SUPERBLOCK_SLOT_A_OFFSET, SUPERBLOCK_SLOT_B_OFFSET};
use crate::store::index::{BTreeError, ObjectProvider};
use crate::store::object::{Location, ObjectIndex};
use crate::store::root::{Root, SuperblockPair};
use crate::store::segment::{self, SegmentError};

use super::{Category, FsckIssue, FsckOptions, Severity};

/// The fsck scan context: raw superblock + segment view plus the derived
/// object index. Implements `ObjectProvider` and `DecoderContext` so the
/// verify phase can walk trees and materialize descriptors without the
/// mounted store.
pub struct FsckCtx {
    /// Store directory.
    pub dir: PathBuf,
    /// Superblock file path.
    pub superblock_path: PathBuf,
    /// Both decoded slots (valid ones only).
    pub slots: Vec<Superblock>,
    /// The chosen active superblock (highest valid generation).
    pub active: Superblock,
    /// The decoded active root (when valid).
    pub root: Option<Root>,
    /// Derived object index rebuilt from segments.
    pub object_index: ObjectIndex,
    /// Records scanned per tag.
    pub records_by_tag: HashMap<RecordTag, u64>,
    /// Issues collected so far.
    pub issues: Vec<FsckIssue>,
    /// Options.
    pub options: FsckOptions,
    /// Total records scanned.
    pub records_scanned: u64,
    /// Number of segments scanned.
    pub segments_scanned: u64,
    /// Torn tails found (segment, truncated_from, truncated_to).
    pub torn_tails: Vec<(u64, u64, u64)>,
    /// Max records allowed per segment (defense against pathological
    /// segments).
    pub max_records_per_segment: u64,
    /// Physical bytes of live-and-unreachable accounting (filled by graph).
    pub leaked_bytes: u64,
    /// Conflicting content ids whose payloads differ (corruption).
    pub conflicting_duplicates: Vec<ChunkId>,
    /// Inodes verified (semantic phase).
    pub inodes_verified: u64,
    /// Extents verified (semantic phase).
    pub extents_verified: u64,
    /// Chunk descriptors verified (semantic phase).
    pub chunk_descriptors_verified: u64,
}

impl FsckCtx {
    /// Run the scan phase: read both superblock slots, pick the active
    /// generation, enumerate and scan every segment, rebuild the derived
    /// index, and decode the active root.
    pub fn scan(dir: &Path, options: &FsckOptions) -> Result<Self, String> {
        let superblock_path = dir.join("superblock");
        let pair =
            SuperblockPair::read(&superblock_path).map_err(|e| format!("superblock read: {e}"))?;
        let mut issues = Vec::new();
        let slots: Vec<Superblock> = [pair.a.clone(), pair.b.clone()]
            .into_iter()
            .flatten()
            .collect();
        if slots.len() < 2 {
            issues.push(FsckIssue::new(
                Severity::Warning,
                Category::Superblock,
                format!("only {} of 2 superblock slots decode", slots.len()),
            ));
        }
        let active = match pair.choose() {
            Ok(sb) => sb,
            Err(e) => {
                return Err(format!(
                    "no valid superblock: {e} (unrepairable without a known-good slot)"
                ));
            }
        };
        let mut ctx = FsckCtx {
            dir: dir.to_path_buf(),
            superblock_path,
            slots,
            active,
            root: None,
            object_index: ObjectIndex::new(),
            records_by_tag: HashMap::new(),
            issues,
            options: options.clone(),
            records_scanned: 0,
            segments_scanned: 0,
            torn_tails: Vec::new(),
            max_records_per_segment: options.max_records_per_segment,
            leaked_bytes: 0,
            conflicting_duplicates: Vec::new(),
            inodes_verified: 0,
            extents_verified: 0,
            chunk_descriptors_verified: 0,
        };
        ctx.scan_segments()?;
        ctx.load_root()?;
        Ok(ctx)
    }

    /// Whether any error-severity issue was found.
    pub fn has_errors(&self) -> bool {
        self.issues.iter().any(|i| i.severity == Severity::Error)
    }

    /// Resource limits derived from the options (for materialization).
    pub fn limits(&self) -> crate::core::limits::Limits {
        crate::core::limits::Limits {
            max_fanout: self.options.max_fanout,
            max_descriptor_bytes: self.options.max_descriptor_bytes,
            max_inline_bytes: self.options.max_inline_bytes,
            max_palette: self.options.max_palette,
            max_period: self.options.max_period,
            max_chunk_size: self.options.max_chunk_size,
            ..Default::default()
        }
    }

    fn scan_segments(&mut self) -> Result<(), String> {
        let segments =
            segment::list_segments(&self.dir).map_err(|e| format!("segment list: {e}"))?;
        if segments.is_empty() {
            self.issues.push(FsckIssue::new(
                Severity::Error,
                Category::Segment,
                "no segment files present".to_string(),
            ));
        }
        // Track payloads per content id to detect conflicting duplicates.
        let mut payload_seen: HashMap<ChunkId, Vec<u8>> = HashMap::new();
        for seq in &segments {
            self.segments_scanned += 1;
            let path = segment::segment_path(&self.dir, *seq);
            let (records, clean_end) = segment::scan_segment(&path, self.max_records_per_segment)
                .map_err(|e| format!("segment {seq}: {e}"))?;
            let file_len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if clean_end < file_len {
                self.torn_tails.push((*seq, clean_end, file_len));
                self.issues.push(FsckIssue::new(
                    Severity::Warning,
                    Category::Segment,
                    format!(
                        "segment {seq}: torn tail of {} bytes after offset {}",
                        file_len - clean_end,
                        clean_end
                    ),
                ));
            }
            if *seq > self.active.segment_seq {
                self.issues.push(FsckIssue::new(
                    Severity::Warning,
                    Category::Segment,
                    format!(
                        "segment {seq} exists beyond the active segment seq {} (uncommitted GC/extraneous)",
                        self.active.segment_seq
                    ),
                ));
            }
            for rec in records {
                self.records_scanned += 1;
                *self.records_by_tag.entry(rec.tag).or_insert(0) += 1;
                if let Some(prev) = payload_seen.get(&rec.content_id) {
                    if prev != &rec.payload {
                        self.conflicting_duplicates.push(rec.content_id);
                        self.issues.push(FsckIssue::new(
                            Severity::Error,
                            Category::Record,
                            format!(
                                "content id {} has conflicting payloads (hash collision or corruption)",
                                rec.content_id
                            ),
                        ));
                    }
                } else {
                    payload_seen.insert(rec.content_id, rec.payload.clone());
                }
                self.object_index.insert(
                    rec.content_id,
                    Location {
                        segment_seq: *seq,
                        offset: rec.offset,
                        stored_len: rec.stored_len as u64,
                        materialized_len: rec.materialized_len,
                        tag: rec.tag,
                    },
                );
            }
        }
        Ok(())
    }

    fn load_root(&mut self) -> Result<(), String> {
        let sb = self.active.clone();
        let slot_root = self
            .object_index
            .get(&sb.root_object_id)
            .map(|loc| {
                segment::read_payload(&self.dir, loc.segment_seq, loc.offset, loc.stored_len)
            })
            .transpose()
            .map_err(|e: SegmentError| format!("root payload read: {e}"))?;
        if let Some(bytes) = slot_root {
            if let Ok(root) = crate::integrity::root::verify_root(&sb, &bytes) {
                if root.generation == sb.generation {
                    self.root = Some(root);
                    return Ok(());
                }
            }
        }
        // Deferred-durability fallback (Phase 6): the slot's root may have
        // been destroyed by a power loss before its fsync. Recover the
        // newest valid ROOT record from the segments and report the
        // fallback.
        match segment::scan_newest_root(&self.dir, self.options.max_records_per_segment) {
            Ok(Some((sb, root))) => {
                self.issues.push(FsckIssue::new(
                    Severity::Warning,
                    Category::Root,
                    format!(
                        "active superblock root missing/invalid; recovered newest root record (generation {})",
                        root.generation
                    ),
                ));
                self.active = sb;
                self.root = Some(root);
                Ok(())
            }
            Ok(None) => {
                self.issues.push(FsckIssue::new(
                    Severity::Error,
                    Category::Root,
                    "no valid root: slot root missing and no root record in segments".to_string(),
                ));
                Ok(())
            }
            Err(e) => Err(format!("segment root scan: {e}")),
        }
    }
}

impl ObjectProvider for FsckCtx {
    fn get(&self, id: &ChunkId) -> Result<Option<Vec<u8>>, BTreeError> {
        self.object_index
            .get(id)
            .map(|loc| {
                segment::read_payload(&self.dir, loc.segment_seq, loc.offset, loc.stored_len)
            })
            .transpose()
            .map_err(|e: SegmentError| BTreeError::Provider(e.to_string()))
    }

    fn put(&mut self, _id: ChunkId, _bytes: Vec<u8>) {
        // fsck never mutates trees.
        unreachable!("FsckCtx::put must not be called")
    }
}

impl DecoderContext for FsckCtx {
    fn fetch_object(&self, id: &ChunkId) -> Result<Vec<u8>, MaterializeError> {
        ObjectProvider::get(self, id)
            .map_err(|e| MaterializeError::InvalidDescriptor(e.to_string()))?
            .ok_or(MaterializeError::MissingObject(*id))
    }

    fn fetch_descriptor(&self, id: &ChunkId) -> Result<Representation, MaterializeError> {
        let bytes = crate::store::index::get(
            self.root
                .as_ref()
                .map(|r| r.chunk_index_root)
                .unwrap_or(ChunkId::ZERO),
            id.as_bytes(),
            crate::store::BTREE_ORDER,
            self.max_records_per_segment as u32,
            self,
        )
        .map_err(|e| MaterializeError::InvalidDescriptor(e.to_string()))?
        .ok_or(MaterializeError::MissingChunk(*id))?;
        crate::format::descriptor::decode(
            &bytes,
            self.options.max_descriptor_bytes,
            self.options.max_inline_bytes,
            self.options.max_palette,
            self.options.max_period,
            self.options.max_chunk_size,
        )
        .map_err(|e| MaterializeError::InvalidDescriptor(e.to_string()))
    }

    fn decode_rans(
        &self,
        model: &[u8],
        encoded: &[u8],
        scale_bits: u8,
        codec: RansCodec,
        out_len: u64,
    ) -> Result<Vec<u8>, MaterializeError> {
        let parsed = crate::rans::metadata::decode_model(model, 2048)
            .map_err(|e| MaterializeError::RansDecode(e.to_string()))?;
        if parsed.scale_bits != scale_bits || parsed.codec != codec {
            return Err(MaterializeError::RansDecode("model tag mismatch".into()));
        }
        crate::rans::residual::decode_stream(&parsed, encoded, out_len)
            .map_err(|e| MaterializeError::RansDecode(e.to_string()))
    }

    fn universe_bytes(
        &self,
        universe: UniverseId,
        seed: [u8; 16],
        coordinate: u64,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>, MaterializeError> {
        match universe {
            UniverseId::UniformXofV1 => Ok(
                crate::entropy::universe::UniformXofV1::materialize_range(seed, coordinate, range),
            ),
        }
    }
}

/// Read a single superblock slot for report display.
pub fn read_slot_raw(path: &Path, offset: u64) -> Result<Option<Superblock>, CodecError> {
    use std::io::Read;
    use std::io::Seek;
    use std::io::SeekFrom;
    let mut file = std::fs::File::open(path).map_err(|_| CodecError::Malformed)?;
    let mut buf = [0u8; 512];
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| CodecError::Malformed)?;
    let mut read = 0usize;
    while read < buf.len() {
        let n = file
            .read(&mut buf[read..])
            .map_err(|_| CodecError::Malformed)?;
        if n == 0 {
            break;
        }
        read += n;
    }
    if read == 0 {
        return Ok(None);
    }
    if read < buf.len() {
        return Err(CodecError::Malformed);
    }
    Superblock::decode(&buf).map(Some)
}

/// Slot offsets for report display.
pub fn slot_offsets() -> (u64, u64) {
    (SUPERBLOCK_SLOT_A_OFFSET, SUPERBLOCK_SLOT_B_OFFSET)
}
