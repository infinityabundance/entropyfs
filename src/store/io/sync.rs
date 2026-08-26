//! The reference synchronous transport (ADR-0021): the pre-10F engine,
//! preserved byte-for-byte as the crash-consistency oracle. `SyncIo` is
//! what every crash court is measured against; `UringIo` must reproduce
//! its store-directory bytes at every injection point.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::format::version::RECORD_HEADER_SIZE;
use crate::store::StoreError;
use crate::store::io::{IoBackend, IoBackendKind, ReadRequest, open_segment_common};

/// The reference synchronous backend.
pub struct SyncIo {
    dir: PathBuf,
    /// Segment handles (seq -> open file), shared by the read and write
    /// paths. Phase-10E/10E1 discipline: the map mutex is only held to
    /// clone the `Arc`, never across a `pread`/`pwrite`; every I/O op is
    /// offset-based, so concurrent ops never share a seek position and
    /// never serialize on the map.
    segment_fds: Mutex<HashMap<u64, Arc<File>>>,
}

impl SyncIo {
    /// Build the backend over a store directory.
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            segment_fds: Mutex::new(HashMap::new()),
        }
    }

    /// The store directory.
    fn dir(&self) -> &Path {
        &self.dir
    }

    /// Get (or open) the segment file handle.
    fn segment_file(&self, seq: u64) -> Result<Arc<File>, StoreError> {
        let mut fds = self.segment_fds.lock().expect("segment fds poisoned");
        Ok(match fds.entry(seq) {
            std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
            std::collections::hash_map::Entry::Vacant(v) => {
                let file = Arc::new(open_rw(&crate::store::segment::segment_path(
                    self.dir(),
                    seq,
                ))?);
                v.insert(file.clone());
                file
            }
        })
    }
}

/// Open a segment/superblock file read-write, creating it when absent
/// (the pre-10F `SegmentWriter::open` / `write_slot` file mode).
fn open_rw(path: &Path) -> Result<File, StoreError> {
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| StoreError::Io(format!("open {}: {e}", path.display())))?)
}

/// Write the full buffer at an absolute offset (pwrite; loops on short
/// writes — the pre-10F `write_all` equivalent for offset-based I/O).
fn pwrite_full(file: &File, mut offset: u64, mut buf: &[u8]) -> Result<(), StoreError> {
    while !buf.is_empty() {
        let n = rustix::io::pwrite(file, buf, offset).map_err(|e| StoreError::Io(e.to_string()))?;
        if n == 0 {
            return Err(StoreError::Io("short segment write (0 bytes)".into()));
        }
        offset += n as u64;
        buf = &buf[n..];
    }
    Ok(())
}

/// Read the full buffer from an absolute offset (pread; loops on short
/// reads — the pre-10F `read_exact` equivalent for offset-based I/O).
fn pread_full(file: &File, offset: u64, buf: &mut [u8]) -> Result<(), StoreError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        let n = rustix::io::pread(file, &mut buf[filled..], offset + filled as u64)
            .map_err(|e| StoreError::Io(e.to_string()))?;
        if n == 0 {
            return Err(StoreError::Io("short segment read".into()));
        }
        filled += n;
    }
    Ok(())
}

impl IoBackend for SyncIo {
    fn kind(&self) -> IoBackendKind {
        IoBackendKind::Sync
    }

    fn name(&self) -> &'static str {
        "sync"
    }

    fn open_segment(&self, seq: u64) -> Result<u64, StoreError> {
        open_segment_common(self, seq)
    }

    fn segment_len(&self, seq: u64) -> Result<u64, StoreError> {
        let path = crate::store::segment::segment_path(self.dir(), seq);
        match std::fs::metadata(&path) {
            Ok(m) => Ok(m.len()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(StoreError::Io(format!("stat {}: {e}", path.display()))),
        }
    }

    fn read_segment_file(&self, seq: u64) -> Result<Vec<u8>, StoreError> {
        let path = crate::store::segment::segment_path(self.dir(), seq);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(StoreError::Io(format!("read {}: {e}", path.display()))),
        }
    }

    fn write_at(&self, seq: u64, offset: u64, bytes: &[u8]) -> Result<(), StoreError> {
        let file = self.segment_file(seq)?;
        pwrite_full(&file, offset, bytes)
    }

    fn truncate_segment(&self, seq: u64, len: u64) -> Result<(), StoreError> {
        let file = self.segment_file(seq)?;
        file.set_len(len).map_err(|e| StoreError::Io(e.to_string()))
    }

    fn sync_segment_file(&self, seq: u64) -> Result<(), StoreError> {
        let file = self.segment_file(seq)?;
        file.sync_all().map_err(|e| StoreError::Io(e.to_string()))
    }

    fn fdatasync_segment(&self, seq: u64) -> Result<(), StoreError> {
        let file = self.segment_file(seq)?;
        file.sync_data().map_err(|e| StoreError::Io(e.to_string()))
    }

    fn sync_segments_dir(&self) -> Result<(), StoreError> {
        let dir = File::open(self.dir().join("segments"))
            .map_err(|e| StoreError::Io(format!("open segments dir: {e}")))?;
        dir.sync_all().map_err(|e| StoreError::Io(e.to_string()))
    }

    fn delete_segment(&self, seq: u64) -> Result<(), StoreError> {
        let path = crate::store::segment::segment_path(self.dir(), seq);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(StoreError::Io(format!("remove {}: {e}", path.display())));
            }
        }
        self.segment_fds
            .lock()
            .expect("segment fds poisoned")
            .remove(&seq);
        Ok(())
    }

    fn read_payload(&self, seq: u64, offset: u64, stored_len: u64) -> Result<Vec<u8>, StoreError> {
        let file = self.segment_file(seq)?;
        let start = offset
            .checked_add(RECORD_HEADER_SIZE)
            .ok_or(StoreError::Limit("payload offset overflow".into()))?;
        let mut buf = vec![0u8; stored_len as usize];
        pread_full(&file, start, &mut buf)?;
        Ok(buf)
    }

    fn read_many(&self, reqs: &[ReadRequest]) -> Vec<Result<Vec<u8>, StoreError>> {
        // The reference path: sequential preads, exactly the pre-10F
        // single-read behavior.
        reqs.iter()
            .map(|r| self.read_payload(r.segment_seq, r.offset, r.stored_len))
            .collect()
    }

    fn write_superblock_slot(&self, offset: u64, slot: &[u8]) -> Result<(), StoreError> {
        let file = open_rw(&self.dir().join("superblock"))?;
        pwrite_full(&file, offset, slot)
    }

    fn fsync_superblock(&self) -> Result<(), StoreError> {
        let file = File::open(self.dir().join("superblock"))
            .map_err(|e| StoreError::Io(format!("open superblock: {e}")))?;
        file.sync_all().map_err(|e| StoreError::Io(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::record::{FLAG_HAS_MATERIALIZED_LEN, encode};
    use crate::format::version::RecordTag;
    use crate::store::io::find_clean_end_bytes;
    use crate::store::segment::{scan_segment, segment_path};

    #[test]
    fn open_write_read_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("segments")).unwrap();
        let io = SyncIo::new(tmp.path());
        // Fresh open writes the magic.
        let end = io.open_segment(0).unwrap();
        assert_eq!(end, 4);
        // Append-encoded records, exactly like SegmentWriter::append then
        // flush: one pwrite of the buffered bytes at durable_end.
        let mut payloads = Vec::new();
        let mut bytes = Vec::new();
        for i in 0..8u32 {
            let p = vec![i as u8; 32 + i as usize];
            let e = encode(
                RecordTag::Data,
                FLAG_HAS_MATERIALIZED_LEN,
                Some(p.len() as u64),
                &p,
            );
            payloads.push(p);
            bytes.extend_from_slice(&e);
        }
        io.write_at(0, end, &bytes).unwrap();
        io.fdatasync_segment(0).unwrap();
        // Scan back and read each payload via read_payload.
        let path = segment_path(tmp.path(), 0);
        let (records, _) = scan_segment(&path, 1000).unwrap();
        assert_eq!(records.len(), 8);
        for (rec, want) in records.iter().zip(&payloads) {
            let got = io
                .read_payload(0, rec.offset, rec.stored_len as u64)
                .unwrap();
            assert_eq!(&got, want);
        }
        // read_many returns results in request order.
        let reqs: Vec<ReadRequest> = records
            .iter()
            .map(|r| ReadRequest {
                segment_seq: 0,
                offset: r.offset,
                stored_len: r.stored_len as u64,
            })
            .collect();
        let many = io.read_many(&reqs);
        for (got, want) in many.iter().zip(&payloads) {
            assert_eq!(got.as_ref().unwrap(), want);
        }
    }

    #[test]
    fn torn_tail_truncated_at_open() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("segments")).unwrap();
        let io = SyncIo::new(tmp.path());
        let arc: std::sync::Arc<dyn crate::store::io::IoBackend> = std::sync::Arc::new(io);
        let mut w = crate::store::segment::SegmentWriter::open(&arc, 0).unwrap();
        for i in 0..8u32 {
            let p = vec![i as u8; 32];
            w.append(encode(
                RecordTag::Data,
                FLAG_HAS_MATERIALIZED_LEN,
                Some(32),
                &p,
            ));
        }
        w.flush().unwrap();
        w.fdatasync().unwrap();
        drop(w);
        let path = segment_path(tmp.path(), 0);
        let full = std::fs::metadata(&path).unwrap().len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(full - 7)
            .unwrap();
        let end = arc.open_segment(0).unwrap();
        assert_eq!(
            end,
            find_clean_end_bytes(&std::fs::read(&path).unwrap()).unwrap()
        );
        // The torn tail is gone; a new append lands cleanly.
        arc.write_at(0, end, &encode(RecordTag::Data, 0, None, b"x"))
            .unwrap();
        arc.fdatasync_segment(0).unwrap();
        let (records, _) = scan_segment(&path, 1000).unwrap();
        assert_eq!(records.len(), 8);
    }

    #[test]
    fn superblock_slot_write_and_fsync() {
        let tmp = tempfile::TempDir::new().unwrap();
        let io = SyncIo::new(tmp.path());
        let sb = crate::format::superblock::Superblock {
            generation: 1,
            ..Default::default()
        };
        let slot = sb.encode();
        io.write_superblock_slot(0, &slot).unwrap();
        io.fsync_superblock().unwrap();
        let pair =
            crate::store::root::SuperblockPair::read(&tmp.path().join("superblock")).unwrap();
        assert_eq!(pair.choose().unwrap().generation, 1);
    }

    #[test]
    fn delete_evicts_handle() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("segments")).unwrap();
        let io = SyncIo::new(tmp.path());
        io.open_segment(3).unwrap();
        io.write_at(3, 4, b"data").unwrap();
        io.delete_segment(3).unwrap();
        assert_eq!(io.segment_len(3).unwrap(), 0);
        assert!(io.read_segment_file(3).unwrap().is_empty());
    }
}
