//! The io_uring storage transport (Phase 10F, ADR-0021): the performance
//! path. The same record format, the same durability ordering, the same
//! crash-state per injection point as [`super::sync::SyncIo`] — with the
//! syscalls issued through one io_uring ring.
//!
//! All io_uring interaction goes through [`crate::platform::io_uring::Uring`]
//! — the crate's single `unsafe` file, which owns the SQE push with a
//! documented buffer-lifetime contract. This module is safe Rust
//! (`#![forbid(unsafe_code)]`), like every store module.
//!
//! # Kernel floor
//!
//! READ/WRITE (5.6), FSYNC (5.1), UNLINKAT (5.11). Open-time torn-tail
//! truncation uses `File::set_len` (std) rather than `IORING_OP_FTRUNCATE`
//! (6.9) — it is a recovery-time operation with no crash-point semantics,
//! and routing it through the ring would needlessly raise the kernel
//! floor. Segment open/stat/scan reads are likewise std (offline paths).
//! The hot path — appends, fdatasync, dir sync, superblock write/fsync,
//! payload reads (`read_many`) and GC unlinks — is entirely on the ring.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use io_uring::opcode;
use io_uring::squeue::Entry as Sqe;
use io_uring::types::{Fd, FsyncFlags};

use crate::format::version::RECORD_HEADER_SIZE;
use crate::platform::io_uring::Uring;
use crate::store::StoreError;
use crate::store::io::{IoBackend, IoBackendKind, ReadRequest, open_segment_common};
use crate::store::segment::segment_file_name;

/// The io_uring transport: one mutex-guarded ring. The store serializes
/// the write path (`commit_lock` + segment mutex), and batches are the
/// parallelism unit for reads, so a single ring is correct; concurrent
/// callers serialize only on the (short) submit/collect critical section.
pub struct UringIo {
    dir: PathBuf,
    /// The ring (platform module: the crate's single unsafe file).
    ring: Uring,
    /// Segment handles (seq -> open file), shared by the read and write
    /// paths (offset-based ops only). Evicted on delete.
    segment_fds: Mutex<HashMap<u64, Arc<File>>>,
}

impl UringIo {
    /// Build the backend over a store directory. Fails loudly when the
    /// kernel cannot provide an io_uring ring (the mount refuses, rather
    /// than silently degrading).
    pub fn new(dir: &Path, entries: u32) -> Result<Self, StoreError> {
        let ring = Uring::new(entries).map_err(|e| {
            StoreError::Io(format!(
                "io_uring setup failed (entries {entries}): {e}; \
                 use --io-backend sync on kernels without io_uring"
            ))
        })?;
        Ok(Self {
            dir: dir.to_path_buf(),
            ring,
            segment_fds: Mutex::new(HashMap::new()),
        })
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

    /// Submit one SQE and wait for its completion. Returns the CQE result.
    ///
    /// Buffer lifetime: the caller's owned buffer (if any) lives in the
    /// caller's frame until this function returns; the fd is a cached
    /// `Arc<File>` that outlives the call.
    fn run_one(&self, entry: Sqe) -> Result<i32, StoreError> {
        let completions = self
            .ring
            .submit_and_wait(&[(1, entry)])
            .map_err(|e| StoreError::Io(format!("uring: {e}")))?;
        Ok(completions[0].result)
    }

    /// The fdatasync-equivalent Fsync op.
    fn fsync_op(&self, seq: u64, datasync: bool) -> Result<i32, StoreError> {
        let file = self.segment_file(seq)?;
        let op = opcode::Fsync::new(Fd(file.as_raw_fd()))
            .flags(if datasync {
                FsyncFlags::DATASYNC
            } else {
                FsyncFlags::empty()
            })
            .build();
        self.run_one(op)
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

/// Map a negative CQE result to a StoreError.
fn cqe_err(res: i32, what: &str) -> StoreError {
    StoreError::Io(format!("{what}: {}", -res))
}

impl IoBackend for UringIo {
    fn kind(&self) -> IoBackendKind {
        IoBackendKind::Uring
    }

    fn name(&self) -> &'static str {
        "uring"
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
        // Own the payload: the SQE references this Vec's memory, and the
        // platform contract requires it to live until the completion is
        // consumed — the same call.
        let mut owned = bytes.to_vec();
        let file = self.segment_file(seq)?;
        let mut off = offset;
        loop {
            let res = self.run_one(
                opcode::Write::new(Fd(file.as_raw_fd()), owned.as_ptr(), owned.len() as u32)
                    .offset(off)
                    .build(),
            )?;
            if res < 0 {
                return Err(cqe_err(res, "pwrite"));
            }
            let n = res as usize;
            if n == owned.len() {
                return Ok(());
            }
            if n == 0 {
                return Err(StoreError::Io("pwrite: 0 bytes written".into()));
            }
            // Short write: advance and re-submit the remainder (the kernel
            // may legally complete a pwrite short).
            off += n as u64;
            owned.drain(..n);
        }
    }

    fn truncate_segment(&self, seq: u64, len: u64) -> Result<(), StoreError> {
        // Open-time torn-tail truncation: `File::set_len` (std) rather
        // than IORING_OP_FTRUNCATE (kernel 6.9+) — a recovery-time op
        // with no crash-point semantics; see the module docs.
        let file = self.segment_file(seq)?;
        file.set_len(len).map_err(|e| StoreError::Io(e.to_string()))
    }

    fn sync_segment_file(&self, seq: u64) -> Result<(), StoreError> {
        let res = self.fsync_op(seq, false)?;
        if res < 0 {
            return Err(cqe_err(res, "fsync segment"));
        }
        Ok(())
    }

    fn fdatasync_segment(&self, seq: u64) -> Result<(), StoreError> {
        let res = self.fsync_op(seq, true)?;
        if res < 0 {
            return Err(cqe_err(res, "fdatasync segment"));
        }
        Ok(())
    }

    fn sync_segments_dir(&self) -> Result<(), StoreError> {
        let dir = File::open(self.dir().join("segments"))
            .map_err(|e| StoreError::Io(format!("open segments dir: {e}")))?;
        let res = self.run_one(opcode::Fsync::new(Fd(dir.as_raw_fd())).build())?;
        if res < 0 {
            return Err(cqe_err(res, "fsync segments dir"));
        }
        Ok(())
    }

    fn delete_segment(&self, seq: u64) -> Result<(), StoreError> {
        // Evict the cached handle before the unlink so no reader can be
        // mid-op on a vanished file; the unlink itself is a ring op.
        self.segment_fds
            .lock()
            .expect("segment fds poisoned")
            .remove(&seq);
        let name = segment_file_name(seq);
        let cname = CString::new(name.as_str())
            .map_err(|_| StoreError::Io("segment name contains NUL".into()))?;
        let dir = File::open(self.dir().join("segments"))
            .map_err(|e| StoreError::Io(format!("open segments dir: {e}")))?;
        // The platform contract: `cname` is owned by this frame and lives
        // until the completion is consumed; `dir` outlives the call.
        let res = self
            .ring
            .submit_and_wait(&[(
                1,
                opcode::UnlinkAt::new(Fd(dir.as_raw_fd()), cname.as_ptr()).build(),
            )])
            .map_err(|e| StoreError::Io(format!("uring: {e}")))?[0]
            .result;
        if res < 0 {
            // NotFound is not an error (idempotent delete).
            let err = std::io::Error::from_raw_os_error(-res);
            if err.kind() != std::io::ErrorKind::NotFound {
                return Err(cqe_err(res, "unlink segment"));
            }
        }
        Ok(())
    }

    fn read_payload(&self, seq: u64, offset: u64, stored_len: u64) -> Result<Vec<u8>, StoreError> {
        let reqs = [ReadRequest {
            segment_seq: seq,
            offset,
            stored_len,
        }];
        let mut out = self.read_many(&reqs);
        out.pop().expect("one request, one result")
    }

    fn read_many(&self, reqs: &[ReadRequest]) -> Vec<Result<Vec<u8>, StoreError>> {
        if reqs.is_empty() {
            return Vec::new();
        }
        // Per-request state: destination buffers are owned here and live
        // until every completion is consumed (the platform contract).
        let mut bufs: Vec<Vec<u8>> = Vec::with_capacity(reqs.len());
        let mut offs: Vec<u64> = Vec::with_capacity(reqs.len());
        for r in reqs {
            let start = match r.offset.checked_add(RECORD_HEADER_SIZE) {
                Some(s) => s,
                None => {
                    // Overflowing offset: synthesize a per-request error
                    // without poisoning the batch.
                    let mut errs: Vec<Result<Vec<u8>, StoreError>> = Vec::new();
                    errs.resize(
                        reqs.len(),
                        Err(StoreError::Limit("payload offset overflow".into())),
                    );
                    return errs;
                }
            };
            bufs.push(vec![0u8; r.stored_len as usize]);
            offs.push(start);
        }
        // Resolve fds up front; a request whose segment cannot be opened
        // fails on its own slot.
        let mut fds: Vec<Option<Arc<File>>> = Vec::with_capacity(reqs.len());
        let mut results: Vec<Option<Result<Vec<u8>, StoreError>>> = Vec::with_capacity(reqs.len());
        let mut inflight: Vec<usize> = Vec::new();
        for (i, r) in reqs.iter().enumerate() {
            match self.segment_file(r.segment_seq) {
                Ok(f) => {
                    fds.push(Some(f));
                    results.push(None);
                    inflight.push(i);
                }
                Err(e) => {
                    fds.push(None);
                    results.push(Some(Err(e)));
                }
            }
        }
        let mut filled: Vec<usize> = vec![0usize; reqs.len()];
        while !inflight.is_empty() {
            // One submission batch per iteration; short reads loop back
            // into `inflight` with an advanced offset.
            let ops: Vec<(u64, Sqe)> = inflight
                .iter()
                .map(|&i| {
                    let file = fds[i].as_ref().expect("inflight fd resolved");
                    let buf = &mut bufs[i];
                    let off = offs[i] + filled[i] as u64;
                    let want = buf.len() - filled[i];
                    let entry = opcode::Read::new(
                        Fd(file.as_raw_fd()),
                        buf[filled[i]..].as_mut_ptr(),
                        want as u32,
                    )
                    .offset(off)
                    .build();
                    (i as u64 + 1, entry)
                })
                .collect();
            let completions = match self
                .ring
                .submit_and_wait(&ops)
                .map_err(|e| StoreError::Io(format!("uring: {e}")))
            {
                Ok(c) => c,
                Err(e) => {
                    // Ring-level failure: fail every in-flight request.
                    for &i in &inflight {
                        results[i] = Some(Err(e.clone()));
                    }
                    break;
                }
            };
            let mut next_inflight: Vec<usize> = Vec::new();
            for c in completions {
                let i = (c.user_data - 1) as usize;
                if c.result < 0 {
                    results[i] = Some(Err(cqe_err(c.result, "pread")));
                    continue;
                }
                let n = c.result as usize;
                let want = bufs[i].len() - filled[i];
                if n >= want {
                    results[i] = Some(Ok(std::mem::take(&mut bufs[i])));
                } else if n == 0 {
                    results[i] = Some(Err(StoreError::Io("pread: 0 bytes".into())));
                } else {
                    filled[i] += n;
                    next_inflight.push(i);
                }
            }
            inflight = next_inflight;
        }
        results
            .into_iter()
            .map(|r| r.expect("every request resolved"))
            .collect()
    }

    fn write_superblock_slot(&self, offset: u64, slot: &[u8]) -> Result<(), StoreError> {
        let file = open_rw(&self.dir().join("superblock"))?;
        let owned = slot.to_vec();
        let res = self.run_one(
            opcode::Write::new(Fd(file.as_raw_fd()), owned.as_ptr(), owned.len() as u32)
                .offset(offset)
                .build(),
        )?;
        if res < 0 {
            return Err(cqe_err(res, "pwrite superblock"));
        }
        if res as usize != slot.len() {
            return Err(StoreError::Io("pwrite superblock: short write".into()));
        }
        Ok(())
    }

    fn fsync_superblock(&self) -> Result<(), StoreError> {
        let file = File::open(self.dir().join("superblock"))
            .map_err(|e| StoreError::Io(format!("open superblock: {e}")))?;
        let res = self.run_one(opcode::Fsync::new(Fd(file.as_raw_fd())).build())?;
        if res < 0 {
            return Err(cqe_err(res, "fsync superblock"));
        }
        Ok(())
    }
}
