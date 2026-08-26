//! Append-only segment files (ADR-0008, `docs/format/ondisk-v1.md` §3).
//!
//! Records are appended sequentially; durability is established with
//! `fdatasync` before a superblock flip. Recovery scans segments forward;
//! a torn tail (partial write) is detected by envelope validation and
//! ignored.
//!
//! Phase 10F (ADR-0021): the writer is transport-agnostic — it buffers
//! encoded records and issues the file operations through the store's
//! [`crate::store::io::IoBackend`] (`SyncIo` reference path / `UringIo`
//! performance path). The buffering and durability semantics are exactly
//! the pre-10F engine's.

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

/// Store error type (typed; never panics on corrupt input).
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

/// Segment file name for a sequence number.
pub fn segment_file_name(seq: u64) -> String {
    format!("{seq:016}.seg")
}

/// Path of a segment file.
pub fn segment_path(dir: &Path, seq: u64) -> PathBuf {
    dir.join("segments").join(segment_file_name(seq))
}

/// The current segment: appends records, tracks the write position, and
/// flushes durably on commit. The backing file operations go through the
/// store's [`IoBackend`] (Phase 10F).
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
    /// Open (create if needed) the segment file for appending.
    ///
    /// On an existing file, any torn tail (records that do not validate to
    /// a clean boundary) is truncated so new appends never follow garbage
    /// (`docs/recovery/crash-consistency.md` §6). The backend performs the
    /// magic write / torn-tail truncation with the exact pre-10F
    /// durability semantics.
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

    /// Sequence number.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Append an encoded record to the buffer.
    pub fn append(&mut self, bytes: Vec<u8>) {
        self.buffer.extend_from_slice(&bytes);
        self.record_count += 1;
    }

    /// Number of buffered bytes.
    pub fn buffered_len(&self) -> u64 {
        self.buffer.len() as u64
    }

    /// Flush buffered bytes to the file (not yet durable): one offset-based
    /// write at the durable end (pwrite through the backend; the pre-10F
    /// `seek → write_all` equivalent).
    pub fn flush(&mut self) -> Result<(), SegmentError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let buf = std::mem::take(&mut self.buffer);
        self.io.write_at(self.seq, self.durable_end, &buf)?;
        self.durable_end += buf.len() as u64;
        Ok(())
    }

    /// Make all flushed data durable (`fdatasync`).
    pub fn fdatasync(&self) -> Result<(), SegmentError> {
        self.io.fdatasync_segment(self.seq)?;
        Ok(())
    }

    /// Current durable end offset.
    pub fn durable_end(&self) -> u64 {
        self.durable_end
    }
}

/// A fully owned record from a segment scan (payload copied out).
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
    /// Total on-disk size.
    pub fn total_size(&self) -> u64 {
        record::HEADER_SIZE + self.stored_len as u64
    }
}

/// Find the last clean record boundary in segment bytes (the offset at
/// which sequential record validation first fails, or EOF). The pure
/// form lives in `store::io` (shared by both backends); this file-based
/// reader is retained for compatibility with path-based scans.
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

/// Delete a segment file (only after the new root is durable — GC rule).
pub fn delete_segment(dir: &Path, seq: u64) -> Result<(), SegmentError> {
    let path = segment_path(dir, seq);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SegmentError::Io(e.to_string())),
    }
}

/// List segment sequence numbers present in the store.
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
