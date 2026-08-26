//! Append-only segment files (ADR-0008, `docs/format/ondisk-v1.md` §3).
//!
//! # PURPOSE
//!
//! Segments are the store's physical log. Records (inodes, B-tree nodes,
//! roots, data/model payloads, xattrs, mutation-log envelopes) are
//! appended sequentially and never rewritten in place; the superblock
//! references the newest root record, and an append is acknowledged only
//! after the commit path's durability barrier. This module owns the
//! segment lifecycle as the store sees it: file naming, open-time
//! torn-tail recovery, forward scanning with envelope validation,
//! offset-based payload reads, GC deletion, and the newest-ROOT recovery
//! fallback. The bytes themselves move through the store's transport
//! seam, [`crate::store::io::IoBackend`] (Phase 10F, ADR-0021):
//! [`crate::store::io::sync::SyncIo`] is the pre-10F reference path,
//! [`crate::store::io::uring::UringIo`] the performance path. The
//! buffering and durability semantics are exactly the pre-10F engine's.
//!
//! # BOUNDARY
//!
//! This module knows the record envelope layout (`format::record`, ondisk
//! §3) and the segment naming scheme, but it does not interpret payloads
//! (that is the object codecs' job) and it does not manage the superblock
//! (the commit path in `store` does; this module only reconstructs a
//! superblock from the newest ROOT record as a recovery fallback). All
//! file operations are issued through `IoBackend`; this module holds no
//! file descriptors itself.
//!
//! # MODEL
//!
//! A segment is: the 4-byte magic (`ESEG`) + zero or more valid records
//! back-to-back + optionally a torn tail (a partial record left by a
//! crash mid-write). A "clean end" is the first offset at which
//! sequential envelope validation fails (or EOF); recovery scans forward
//! from offset 4 and stops at the clean end, ignoring everything at or
//! beyond it. `record::decode` returns `Ok(None)` for an all-zero region
//! (padding / end of the written region) and `Truncated` when a record
//! header or payload is cut short — both are benign clean ends; any
//! other failure is a mid-file integrity error.
//!
//! # PERSISTENT AUTHORITY
//!
//! The envelope is normative on-disk format (ondisk §3): tag, version,
//! flags, header_len, stored_len, materialized_len (valid when the flag
//! bit is set), content_id, and header/payload CRCs. Validation here
//! implements the recovery semantics: a malformed envelope mid-file is an
//! integrity error, a truncated one at the tail is a torn write.
//! Deleting a segment is permitted only after the new root that
//! supersedes it is durable (GC rule, `docs/architecture/gc.md` §3).
//!
//! # CORRECTNESS INVARIANTS
//!
//! - Records never span segments and are never overwritten in place:
//!   appends start at `durable_end`, which equals the file length after
//!   every flush, and only ever advance.
//! - New appends never follow garbage: reopening a segment truncates any
//!   torn tail to the clean end and makes the truncation durable before
//!   returning.
//! - A record that validates to a clean boundary is never dropped; the
//!   scan record limit is an explicit resource bound, not a validation
//!   rule.
//! - Length units: `durable_end`, offsets, `stored_len` and `total_size`
//!   are PHYSICAL bytes in the segment file; `materialized_len` is the
//!   LOGICAL length of the decoded content (valid only when the flag bit
//!   is set).
//! - A segment is deleted only when it is unreachable from the committed
//!   root and every snapshot root (GC mark precedes delete).
//!
//! # CONCURRENCY
//!
//! [`SegmentWriter`] is single-owner: the store's commit path serializes
//! appends, so there is exactly one writer per segment. Reads (payload
//! reads, scans, recovery) may run concurrently with each other and with
//! appends on the same segment: all file I/O is offset-based
//! (`pread`/`pwrite`), so no operation depends on a shared file
//! position. The segment fd cache lives in the backend (`segment_fds:
//! Mutex<HashMap<u64, Arc<File>>>`, shared by reads and writes); the
//! Phase-10E1 lock-free fix — the 10E design held the map mutex across
//! the whole `pread` loop, serializing concurrent object reads even
//! though `pread` has no shared position; the cache now stores
//! `Arc<File>`, so a reader clones the Arc under the lock and drops the
//! lock before any I/O, and object reads execute concurrently. This
//! module itself has no locks.
//!
//! # DURABILITY
//!
//! Acknowledgement means: buffered records written to the file (`flush`,
//! page cache), `fdatasync`d, and the superblock slot that references the
//! new root made durable — in that order, so segment data is on stable
//! storage BEFORE the superblock flip that makes it reachable. Process
//! crash: page-cache writes survive. Power loss: only fdatasync'd bytes
//! are guaranteed; a torn tail is detected by envelope validation at the
//! next open/scan and truncated or ignored. One historical hazard
//! remains covered: deferred durability writes the inactive superblock
//! slot before the segment data is fsync'd, so a power loss can leave
//! that slot pointing at a root record that never became durable —
//! [`scan_newest_root`] recovers by finding the newest valid ROOT record
//! across all segments (ADR-0008 Phase 6).
//!
//! # RESOURCE BOUNDS
//!
//! Record sizes are envelope-bounded (u16 header_len, u32 stored_len, u64
//! materialized_len) and every length is checked before use; scans take
//! `limit_records` so a pathological segment cannot cause unbounded work;
//! payload reads allocate exactly `stored_len` physical bytes (a
//! persistent, therefore untrusted, size — the caller validates the
//! record context it came from). `scan_segment` and `find_clean_end` read
//! the whole file into memory, bounded by file size — a hostile-media
//! boundary exercised by `docs/security/hostile-media-court.md`.
//!
//! # PERFORMANCE
//!
//! The writer buffers encoded records and flushes the whole buffer in ONE
//! offset-based write at the durable end (`write_at`); the fd cache
//! avoids an open per operation; offset-based `pread` keeps concurrent
//! readers off a shared seek position (10E). The 10F transport made the
//! writer transport-agnostic — the same buffering/durability semantics
//! over `SyncIo` or `UringIo` — and moved payload reads behind
//! `read_many` (one submission / one batch), where concurrent `pread`s
//! are the entire point.
//!
//! # FAILURE MODES
//!
//! Typed [`SegmentError`] only, never a panic on corrupt input (this
//! module is `#![forbid(unsafe_code)]`; every disk byte passes through
//! checked parsing): `Io` (with path), `Malformed` (bad magic/version),
//! `CorruptRecord` (bad CRC/length mid-file, or the scan limit hit),
//! `Overflow` (offset arithmetic), `Missing` (segment not found;
//! declared for callers — no path here currently constructs it: absent
//! files are handled as empty results, and deletion treats NotFound as
//! success). A mid-file validation failure must never be silently
//! skipped, and a clean end must never be mistaken for a corrupt record.
//!
//! # HISTORY / EVIDENCE
//!
//! - Phase 6 (ADR-0008): `scan_newest_root` superblock reconstruction
//!   from the newest valid ROOT record.
//! - Phase 10E: segment read-fd cache + range-traversal read paths (one
//!   read-only fd per segment, offset-based `pread`).
//! - Phase 10E1: lock-free fd-cache reads — the cache now stores
//!   `Arc<File>`; A/B sealed (`fuse-court-*-10e1-before/after`; the court
//!   did not move, but the serialization point is gone with no regression
//!   and the shape is what 10F `read_many` needs) — CHANGELOG v0.6.1.
//! - Phase 10F (ADR-0021): transport-agnostic writer over `IoBackend`;
//!   crash-court parity between `SyncIo` and `UringIo` (canonically
//!   byte-identical store directories at every injection point) is the
//!   acceptance test — CHANGELOG v0.6.2.

#![forbid(unsafe_code)]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::format::codec::CodecError;
use crate::format::record;
use crate::format::version::SEGMENT_MAGIC;
use crate::store::StoreError;
use crate::store::io::IoBackend;

/// Segment error type (typed; never panics on corrupt input).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentError {
    /// I/O failure (path attached for diagnosis).
    Io(String),
    /// Segment file malformed (bad magic/version).
    Malformed,
    /// Record envelope invalid (bad CRC/length).
    CorruptRecord(String),
    /// Sequence number overflow.
    Overflow,
    /// Segment not found.
    Missing,
}

impl std::fmt::Display for SegmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for SegmentError {}

impl From<std::io::Error> for SegmentError {
    fn from(e: std::io::Error) -> Self {
        SegmentError::Io(e.to_string())
    }
}

impl From<StoreError> for SegmentError {
    fn from(e: StoreError) -> Self {
        SegmentError::Io(e.to_string())
    }
}

/// Segment file name for a sequence number: 16-digit zero-padded decimal
/// (ondisk-v1 §1), so lexicographic order equals sequence order.
pub fn segment_file_name(seq: u64) -> String {
    format!("{seq:016}.seg")
}

/// Path of a segment file (under `dir/segments/`).
pub fn segment_path(dir: &Path, seq: u64) -> PathBuf {
    dir.join("segments").join(segment_file_name(seq))
}

/// The current segment's write side: buffers encoded records, tracks the
/// write position, and flushes durably on commit.
///
/// # Role and invariants
///
/// Exactly one writer exists per segment (the store's commit path
/// serializes appends); `seq` is the segment sequence. `durable_end` is
/// the physical byte offset of the next append (== file length after
/// every flush); `buffer` holds logical (unflushed) bytes; `record_count`
/// counts records appended in this writer's lifetime. Appends never
/// overwrite: records are written at `durable_end` and the end only ever
/// advances.
///
/// # Transport (Phase 10F, ADR-0021)
///
/// The writer is transport-agnostic: it holds no file descriptor and
/// issues every file operation through the store's [`IoBackend`] —
/// [`crate::store::io::sync::SyncIo`] (reference path) or
/// [`crate::store::io::uring::UringIo`] (performance path) — with the
/// exact pre-10F buffering and durability semantics. Segment fds live in
/// the backend's cache (`segment_fds: Mutex<HashMap<u64, Arc<File>>>`),
/// shared by reads and writes; all I/O is offset-based (`pwrite`/`pread`)
/// so no operation depends on a shared seek position (the Phase-10E1
/// lock-free fix — see the module docs).
///
/// # Units
///
/// `durable_end` and every offset here are PHYSICAL bytes in the segment
/// file; `buffer`/`buffered_len` are logical bytes not yet written;
/// record lengths are stored (physical) vs materialized (logical) — see
/// [`ScanRecord`].
pub struct SegmentWriter {
    seq: u64,
    io: Arc<dyn IoBackend>,
    /// Buffered bytes not yet written to the file.
    buffer: Vec<u8>,
    /// The durable end of the file (== file length).
    durable_end: u64,
    /// Records appended in this writer's lifetime.
    record_count: u64,
}

impl SegmentWriter {
    /// Open (create if needed) the segment file for appending; returns a
    /// writer positioned at the segment's durable end.
    ///
    /// On an existing file, any torn tail (records that do not validate to
    /// a clean boundary) is truncated so new appends never follow garbage
    /// (`docs/recovery/crash-consistency.md` §6). The backend performs the
    /// magic write / torn-tail truncation with the exact pre-10F
    /// durability semantics.
    ///
    /// # Stages
    ///
    /// 1. `IoBackend::open_segment(seq)`: fresh file ⇒ the 4-byte magic
    ///    (`ESEG`) is written and made durable (`sync_all`); existing
    ///    file ⇒ the torn tail is truncated to the clean end and the
    ///    truncation made durable (`fdatasync`). Returns the durable end
    ///    offset.
    /// 2. Position the writer there (`max(4)` — appends always follow the
    ///    magic; a fresh segment starts with `durable_end == 4`).
    ///
    /// This is also the rollover entry point: opening a NEW `seq` while
    /// the caller seals the previous segment begins the next segment
    /// (the old file is left intact for recovery and GC).
    pub fn open(io: &Arc<dyn IoBackend>, seq: u64) -> Result<Self, SegmentError> {
        let durable_end = io.open_segment(seq)?;
        Ok(Self {
            seq,
            io: io.clone(),
            buffer: Vec::new(),
            durable_end: durable_end.max(4),
            record_count: 0,
        })
    }

    /// Sequence number of the segment this writer appends to.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Append one already-encoded record to the write buffer (in-memory;
    /// not yet visible to readers). `record_count` increments here; the
    /// ACK path is `flush` + `fdatasync` (module DURABILITY).
    pub fn append(&mut self, bytes: Vec<u8>) {
        self.buffer.extend_from_slice(&bytes);
        self.record_count += 1;
    }

    /// Number of buffered (logical, not-yet-written) bytes.
    pub fn buffered_len(&self) -> u64 {
        self.buffer.len() as u64
    }

    /// Flush buffered bytes to the file (page cache only — NOT yet
    /// durable): one offset-based write at the durable end (pwrite
    /// through the backend; the pre-10F `seek → write_all` equivalent).
    ///
    /// # Stages (the append path)
    ///
    /// 1. Take the whole buffer, so a later `append` can never observe a
    ///    partial flush — the writer either has the bytes or not.
    /// 2. `IoBackend::write_at(seq, durable_end, bytes)`: one pwrite at
    ///    the current end. Appends are strictly sequential — writes never
    ///    overlap and never overwrite.
    /// 3. Advance `durable_end` by the written byte count; it is now the
    ///    new file length, and the next flush continues from here.
    ///
    /// Durability is established later by [`SegmentWriter::fdatasync`]
    /// (and the superblock flip in the commit path) — module DURABILITY.
    pub fn flush(&mut self) -> Result<(), SegmentError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let buf = std::mem::take(&mut self.buffer);
        self.io.write_at(self.seq, self.durable_end, &buf)?;
        self.durable_end += buf.len() as u64;
        Ok(())
    }

    /// Make all flushed data durable (`fdatasync` through the backend).
    ///
    /// This is the durability point for the segment's data: after it
    /// returns, the records written by the last `flush` survive power
    /// loss. The commit path calls this BEFORE the superblock flip that
    /// makes the records reachable (module DURABILITY ordering).
    pub fn fdatasync(&self) -> Result<(), SegmentError> {
        self.io.fdatasync_segment(self.seq)?;
        Ok(())
    }

    /// Current durable end offset: the physical byte offset of the next
    /// append (== file length after every flush).
    pub fn durable_end(&self) -> u64 {
        self.durable_end
    }
}

/// A fully owned record from a segment scan (payload copied out of the
/// scanned bytes).
///
/// # Units
///
/// `stored_len` is PHYSICAL bytes present in the envelope (the payload's
/// on-disk size); `materialized_len` is the LOGICAL length of the decoded
/// content (`None` unless the flags' materialized-length bit is set);
/// `offset` is the record's physical start offset within the segment
/// file; [`ScanRecord::total_size`] = `HEADER_SIZE + stored_len` — the
/// physical bytes the record occupies, and the scan's step size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRecord {
    /// Record tag.
    pub tag: crate::format::version::RecordTag,
    /// Flags.
    pub flags: u16,
    /// Stored payload length.
    pub stored_len: u32,
    /// Materialized length.
    pub materialized_len: Option<u64>,
    /// Content id.
    pub content_id: crate::core::extent::ChunkId,
    /// Payload bytes (owned).
    pub payload: Vec<u8>,
    /// Record start offset within the segment.
    pub offset: u64,
}

impl ScanRecord {
    /// Physical on-disk size of the record: envelope header + stored
    /// payload bytes.
    pub fn total_size(&self) -> u64 {
        record::HEADER_SIZE + self.stored_len as u64
    }
}

/// Find the last clean record boundary in segment bytes (the offset at
/// which sequential record validation first fails, or EOF). The pure
/// form lives in `store::io` (shared by both backends); this file-based
/// reader is retained for compatibility with path-based scans.
///
/// "Clean" means every byte before the boundary validated as complete
/// record envelopes (magic + CRCs); everything from the boundary onward
/// is a torn tail or padding and is never interpreted. Reads the whole
/// file into memory (bounded by file size).
#[allow(dead_code)]
fn find_clean_end(file: &mut std::fs::File) -> Result<u64, SegmentError> {
    use std::io::Read;
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    crate::store::io::find_clean_end_bytes(&bytes)
}

/// Scan a segment file sequentially, validating every record envelope.
///
/// Returns owned records plus the first offset at which validation fails
/// (torn tail). `limit_records` bounds the scan (defense against
/// pathological segments).
///
/// # Envelope semantics (torn-tail detection)
///
/// Each record is decoded and integrity-checked at its offset:
///
/// - valid record ⇒ advance by `total_size()` (envelope + payload);
/// - `Ok(None)` ⇒ all-zero padding / end of the written region: the
///   clean end;
/// - `Truncated` ⇒ not enough bytes for the header/payload: a TORN TAIL
///   — the benign crash artifact; the scan stops and the tail is ignored
///   (recovery semantics);
/// - any other decode error (bad tag/version/length, header or payload
///   CRC failure, content-id mismatch) ⇒ `SegmentError::CorruptRecord`:
///   corruption MID-file, an integrity failure that must never be
///   silently skipped.
///
/// # Stages
///
/// 1. Read the whole file. A file shorter than the 4-byte magic has
///    nothing valid to scan (empty result at its length); a wrong magic
///    is `Malformed`.
/// 2. Walk records from offset 4 (after the magic) with the envelope
///    rules above, enforcing `limit_records` (hitting it aborts with
///    `CorruptRecord("record limit exceeded")` — an explicit resource
///    bound, not a validation rule).
/// 3. Return `(records, first-invalid offset)`. Offsets are physical
///    bytes in the file; lengths per [`ScanRecord`].
pub fn scan_segment(
    path: &Path,
    limit_records: u64,
) -> Result<(Vec<ScanRecord>, u64), SegmentError> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut bytes = Vec::with_capacity(len as usize);
    file.read_to_end(&mut bytes)?;
    if bytes.len() < 4 {
        // Truncated magic: nothing valid to scan.
        return Ok((Vec::new(), bytes.len() as u64));
    }
    if bytes[..4] != SEGMENT_MAGIC {
        return Err(SegmentError::Malformed);
    }
    let mut records = Vec::new();
    let mut offset = 4u64;
    while offset < bytes.len() as u64 {
        if records.len() as u64 >= limit_records {
            return Err(SegmentError::CorruptRecord("record limit exceeded".into()));
        }
        match record::decode(&bytes, offset) {
            Ok(Some(rec)) => {
                let total = rec.total_size();
                records.push(ScanRecord {
                    tag: rec.tag,
                    flags: rec.flags,
                    stored_len: rec.stored_len,
                    materialized_len: rec.materialized_len,
                    content_id: rec.content_id,
                    payload: rec.payload.to_vec(),
                    offset: rec.offset,
                });
                offset = offset.checked_add(total).ok_or(SegmentError::Overflow)?;
            }
            Ok(None) => break,                   // zero padding / clean end
            Err(CodecError::Truncated) => break, // torn tail
            Err(e) => {
                return Err(SegmentError::CorruptRecord(format!(
                    "at offset {offset}: {e:?}"
                )));
            }
        }
    }
    Ok((records, offset))
}

/// Read a record's payload from a segment file by absolute offset.
///
/// The payload begins at `offset + RECORD_HEADER_SIZE` (the envelope
/// header precedes it); exactly `stored_len` PHYSICAL bytes are read via
/// `seek` + `read_exact`. `stored_len` is persistent data (it came from a
/// record envelope or the chunk index's location entry), so it is an
/// allocation bound that must be validated by the caller's record
/// context. Path-based variant (fsck, store open, recovery fallback);
/// the runtime read path goes through
/// [`IoBackend::read_payload`]/`read_many`, which add the fd cache and
/// batching.
pub fn read_payload(
    dir: &Path,
    seq: u64,
    offset: u64,
    stored_len: u64,
) -> Result<Vec<u8>, SegmentError> {
    let path = segment_path(dir, seq);
    let mut file = File::open(path)?;
    let start = offset
        .checked_add(record::HEADER_SIZE)
        .ok_or(SegmentError::Overflow)?;
    file.seek(SeekFrom::Start(start))?;
    let mut payload = vec![0u8; stored_len as usize];
    file.read_exact(&mut payload)?;
    Ok(payload)
}

/// Delete a segment file (only after the new root is durable — GC rule,
/// `docs/architecture/gc.md` §3). Idempotent: an already-absent file is
/// success (`NotFound` ⇒ `Ok`). Path-based helper (test use); GC deletion
/// at runtime goes through [`IoBackend::delete_segment`], which
/// additionally evicts the fd-cache entry.
pub fn delete_segment(dir: &Path, seq: u64) -> Result<(), SegmentError> {
    let path = segment_path(dir, seq);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SegmentError::Io(e.to_string())),
    }
}

/// List segment sequence numbers present in the store (`dir/segments/`),
/// in ascending sequence order. Names that do not parse as `NNNN.seg`
/// are ignored.
pub fn list_segments(dir: &Path) -> Result<Vec<u64>, SegmentError> {
    let seg_dir = dir.join("segments");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&seg_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(stem) = name.strip_suffix(".seg") {
            if let Ok(seq) = stem.parse::<u64>() {
                out.push(seq);
            }
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// Recovery fallback: the newest valid ROOT record across all segments,
/// reconstructed into a superblock (ADR-0008 Phase 6). Used when the
/// superblock slots reference root records that a power loss destroyed
/// (deferred durability writes the inactive slot before the segment data
/// is fsync'd). Returns `(superblock, root)`.
///
/// # Stages
///
/// 1. List segments in sequence order.
/// 2. Scan each (bounded by `max_records_per_segment`), keeping only
///    ROOT records that decode to a valid root.
/// 3. Reconstruct a superblock from each root (feature bits are cleared
///    — later commits re-flag them) and keep the highest-`generation`
///    one.
///
/// `generation` is the ordering authority: a newer committed root always
/// carries a higher generation than any older one, so the winner is the
/// newest commit that actually became durable.
pub fn scan_newest_root(
    dir: &Path,
    max_records_per_segment: u64,
) -> Result<
    Option<(
        crate::format::superblock::Superblock,
        crate::store::root::Root,
    )>,
    SegmentError,
> {
    let mut best: Option<(
        crate::format::superblock::Superblock,
        crate::store::root::Root,
    )> = None;
    for seq in list_segments(dir)? {
        let path = segment_path(dir, seq);
        let (records, _) = scan_segment(&path, max_records_per_segment)?;
        for rec in records {
            if rec.tag != crate::format::version::RecordTag::Root {
                continue;
            }
            let Ok(root) = crate::store::root::Root::decode(&rec.payload) else {
                continue;
            };
            let sb = crate::format::superblock::Superblock {
                uuid: root.uuid,
                generation: root.generation,
                root_object_id: root.id(),
                segment_seq: root.segment_seq,
                incompat: 0, // feature bits are re-flagged by later commits
                ..Default::default()
            };
            let replace = match &best {
                None => true,
                Some((b, _)) => root.generation > b.generation,
            };
            if replace {
                best = Some((sb, root));
            }
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::record::{FLAG_HAS_MATERIALIZED_LEN, encode};
    use crate::format::version::RecordTag;
    use crate::store::io::sync::SyncIo;
    use std::io::Write;
    use tempfile::TempDir;

    /// Test helper: a backend-driven writer over a temp store dir.
    fn writer_for(tmp: &TempDir, seq: u64) -> SegmentWriter {
        std::fs::create_dir_all(tmp.path().join("segments")).unwrap();
        let io: Arc<dyn IoBackend> = Arc::new(SyncIo::new(tmp.path()));
        SegmentWriter::open(&io, seq).unwrap()
    }

    fn make_records() -> Vec<Vec<u8>> {
        (0..8u32)
            .map(|i| {
                let payload = vec![i as u8; 32 + i as usize];
                encode(
                    RecordTag::Data,
                    FLAG_HAS_MATERIALIZED_LEN,
                    Some(payload.len() as u64),
                    &payload,
                )
            })
            .collect()
    }

    #[test]
    fn append_flush_sync_scan() {
        let tmp = TempDir::new().unwrap();
        let mut w = writer_for(&tmp, 0);
        for bytes in make_records() {
            w.append(bytes);
        }
        w.flush().unwrap();
        w.fdatasync().unwrap();
        let (records, end) = scan_segment(&segment_path(tmp.path(), 0), 1000).unwrap();
        assert_eq!(records.len(), 8);
        assert_eq!(end, w.durable_end());
        // payload roundtrip via read_payload
        for rec in &records {
            let payload = read_payload(tmp.path(), 0, rec.offset, rec.stored_len as u64).unwrap();
            assert_eq!(payload, rec.payload);
        }
    }

    #[test]
    fn torn_tail_ignored() {
        let tmp = TempDir::new().unwrap();
        let mut w = writer_for(&tmp, 0);
        for bytes in make_records() {
            w.append(bytes);
        }
        w.flush().unwrap();
        w.fdatasync().unwrap();
        // Simulate a torn write: truncate the file mid-record.
        let path = segment_path(tmp.path(), 0);
        let full_len = std::fs::metadata(&path).unwrap().len();
        let torn = full_len - 7;
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(torn).unwrap();
        drop(f);
        let (records, _end) = scan_segment(&path, 1000).unwrap();
        // Records fully before the cut remain; the torn tail is dropped.
        assert!(records.len() < 8);
        // Reopening truncates the torn tail so new appends follow clean
        // records.
        let mut w2 = writer_for(&tmp, 0);
        assert_eq!(w2.durable_end(), scan_segment(&path, 1000).unwrap().1);
        // Appending after the torn tail must yield a fully valid segment.
        let extra = encode(RecordTag::Data, 0, None, b"post-torn record");
        w2.append(extra);
        w2.flush().unwrap();
        w2.fdatasync().unwrap();
        drop(w2);
        let (records2, _) = scan_segment(&path, 1000).unwrap();
        assert!(records2.len() > records.len());
    }

    #[test]
    fn corrupt_middle_detected() {
        let tmp = TempDir::new().unwrap();
        let mut w = writer_for(&tmp, 0);
        for bytes in make_records() {
            w.append(bytes);
        }
        w.flush().unwrap();
        w.fdatasync().unwrap();
        // Flip a byte inside the first record's payload.
        let path = segment_path(tmp.path(), 0);
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        f.seek(SeekFrom::Start(record::HEADER_SIZE + 5)).unwrap();
        let mut b = [0u8; 1];
        f.read_exact(&mut b).unwrap();
        b[0] ^= 0xFF;
        f.seek(SeekFrom::Start(record::HEADER_SIZE + 5)).unwrap();
        f.write_all(&b).unwrap();
        drop(f);
        let res = scan_segment(&path, 1000);
        assert!(matches!(res, Err(SegmentError::CorruptRecord(_))));
    }

    #[test]
    fn list_and_delete() {
        let tmp = TempDir::new().unwrap();
        let mut w0 = writer_for(&tmp, 0);
        w0.flush().unwrap();
        drop(w0);
        let mut w1 = writer_for(&tmp, 1);
        w1.flush().unwrap();
        drop(w1);
        assert_eq!(list_segments(tmp.path()).unwrap(), vec![0, 1]);
        delete_segment(tmp.path(), 0).unwrap();
        assert_eq!(list_segments(tmp.path()).unwrap(), vec![1]);
    }
}
