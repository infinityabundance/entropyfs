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
//! # PURPOSE
//!
//! Provide the store's performance transport: every segment mutation,
//! durability barrier, superblock op, payload read and GC unlink issued
//! through one io_uring ring, with call semantics indistinguishable from
//! [`super::sync::SyncIo`] — each [`IoBackend`] method completes its
//! durability work before returning, so the store's orchestration above
//! the seam is unchanged.
//!
//! # BOUNDARY
//!
//! KNOWS: the store directory layout, segment file naming and open mode,
//! the record header size (payloads begin at `offset + RECORD_HEADER_SIZE`),
//! and the `IoBackend` contract. NEVER KNOWS: record format semantics,
//! transaction/epoch orchestration, or recovery — those live above the
//! seam. And never `unsafe`: every ring interaction goes through the
//! platform module's safe API, with the buffer-lifetime contract
//! satisfied by owning each buffer in the issuing frame until the
//! completion is consumed.
//!
//! # MODEL
//!
//! A `UringIo` behaves identically to `SyncIo` at the call boundary; the
//! difference is only *how* the syscalls are issued. Writes: one
//! submission per batch (the whole mutation-log append or GC copy pass).
//! Reads: `read_many` fetches a materialization's model/stream/
//! dictionary/tree-node dependencies in ONE submission queue with
//! parallel decode; the extent scan and the transaction prune walk batch
//! per tree level. Durability: the ordered op sequence (segment
//! fdatasync, dir sync, superblock slot write + fsync) is a small
//! sequence of ordered ops on the same ring.
//!
//! # PERSISTENT AUTHORITY
//!
//! Yes — this is part of the persistent-data surface: it writes segment
//! bytes, truncates torn tails, writes and fsyncs the superblock pair,
//! and unlinks segments during GC. Its acceptance test is crash-court
//! parity: at every injection point the store directory must be
//! canonically byte-identical to the `SyncIo` state (inode wall-clock
//! times canonicalized), and recovery must produce the same admissible
//! state. A `UringIo` that produced a different recoverable state at any
//! crash point is wrong by definition.
//!
//! # CORRECTNESS INVARIANTS
//!
//! - The same durability ordering as `SyncIo` per call; every op
//!   completes (page-cache accept or durable) before returning.
//! - Short writes loop: advance the offset and re-submit the remainder
//!   (the kernel may legally complete a pwrite short); 0 bytes is an
//!   error, not a silent success.
//! - Short reads loop via a per-request `filled` count over the same
//!   owned buffer; `read_many` returns results in request order.
//! - Delete evicts the cached fd handle BEFORE the unlink (no reader
//!   mid-op on a vanished file); NotFound is tolerated (idempotent
//!   delete).
//! - All segment ops are offset-based; the fd map mutex is held only to
//!   clone the `Arc<File>` (10E1 discipline), never across an op.
//!
//! # CONCURRENCY
//!
//! One mutex-guarded ring (the platform module); the write path is
//! serialized above (`commit_lock` + segment mutex), so concurrent
//! callers serialize only on the short submit/collect critical section.
//! Batches are the read-path parallelism unit — `read_many` is where
//! concurrent preads actually overlap.
//!
//! # DURABILITY
//!
//! Identical acknowledgement semantics to `SyncIo`: `write_at` = kernel
//! page-cache accept; `fdatasync_segment` = record durable;
//! `sync_segment_file` = full sync (fresh-magic durability);
//! `sync_segments_dir` = segment directory entry durable;
//! `fsync_superblock` = commit durable. The ring is a transport; the
//! durability ordering is this module's per-call sequence, identical to
//! the sync engine's — which is exactly what the crash courts diff.
//!
//! # RESOURCE BOUNDS
//!
//! `read_many` allocates `stored_len` bytes per request (`u64` → `usize`);
//! an overflowing `offset + RECORD_HEADER_SIZE` is a typed per-request
//! `Limit` error that never poisons the batch. `stored_len` derives from
//! persistent descriptors and is bounded by the decode limits the
//! hostile-media court exercises (upstream of this seam). Ring depth is
//! the CLI `--io-uring-entries N` (submission slots, u32).
//!
//! # PERFORMANCE
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
//!
//! Measured ring economics (Phase 10F, tmpfs): ~2.3 µs per submit-and-
//! wait cycle (the kernel's submit+wait+wake floor) vs ~0.1 µs per
//! `pread`, amortizing to ~0.34 µs/op at a 32-op batch — batching closes
//! most of the read gap. The sealed 10F-sync/10F-uring court pair
//! (evidence `fuse-court-*-10f-sync/uring`) shows uring trailing sync by
//! 5–27% (reads −7–12%) on tmpfs; the residual write gap is the ring
//! floor on sub-µs tmpfs I/O. The default stays `sync` (the oracle)
//! until real-device (NVMe, queue-depth) evidence flips it; this seam is
//! where that evidence will land.
//!
//! # FAILURE MODES
//!
//! [`UringIo::new`] fails loudly when the kernel cannot provide a ring
//! (the mount refuses, with a hint to use `--io-backend sync`) — never
//! silent degradation. Negative CQE results become typed `StoreError`s
//! via `cqe_err` (`-errno`). In `read_many`, a request whose segment
//! cannot be opened or whose read fails errors its own slot; a ring-level
//! failure fails every in-flight request of the batch. NotFound on
//! unlink is not an error. Lock poisoning (`expect`) is a programming
//! error, not a runtime path.
//!
//! # HISTORY / EVIDENCE
//!
//! Phase 10F, ADR-0021. The crash courts and a full-workload sequence
//! run against BOTH backends; `src/tests/io_backend_parity.rs` asserts
//! the store directories are canonically byte-identical at every crash
//! point, and the in-crate crash/durability courts are parameterized
//! over both backends (evidence `fuse-court-*-10f-sync/uring`). The 10F
//! write-path hunt also surfaced an O(tree)-per-tiny-batch bug in the
//! checkpoint's chunk-index patch — fixed in the store (empty-batch
//! short-circuit + empty-slice skip), a 50×+ win on BOTH backends — the
//! batching shape of this transport is part of why those measurements
//! improved.

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

/// The io_uring transport: one mutex-guarded ring (ring depth in ops,
/// from the CLI `--io-uring-entries N`). The store serializes the write
/// path (`commit_lock` + segment mutex), and batches are the parallelism
/// unit for reads, so a single ring is correct; concurrent callers
/// serialize only on the (short) submit/collect critical section.
///
/// INVARIANTS: every segment op is offset-based (no shared seek
/// position); the fd map mutex is held only to clone the `Arc<File>`
/// (10E1 discipline), never across an op; delete evicts the handle
/// before the unlink; every buffer handed to the ring is owned by this
/// struct's call frame until its completion is consumed (the platform
/// contract).
pub struct UringIo {
    dir: PathBuf,
    /// The ring (platform module: the crate's single unsafe file).
    ring: Uring,
    /// Segment handles (seq -> open file), shared by the read and write
    /// paths (offset-based ops only). Evicted on delete.
    segment_fds: Mutex<HashMap<u64, Arc<File>>>,
}

impl UringIo {
    /// Build the backend over a store directory with a ring of `entries`
    /// submission slots (ring depth, in ops; clamped to ≥ 8 by the
    /// platform module). Fails loudly when the kernel cannot provide an
    /// io_uring ring (the mount refuses, rather than silently degrading).
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

    /// Submit one SQE and wait for its completion. Returns the CQE result
    /// (bytes transferred, or `-errno`).
    ///
    /// Buffer lifetime: the caller's owned buffer (if any) lives in the
    /// caller's frame until this function returns; the fd is a cached
    /// `Arc<File>` that outlives the call. That is the platform
    /// contract's whole content — `Uring::submit_and_wait` does not
    /// return until the completion is consumed, so owning the buffer for
    /// the duration of the call satisfies it by construction (see the
    /// platform module docs).
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
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| StoreError::Io(format!("open {}: {e}", path.display())))
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
        // Own the payload. The platform contract (module docs of
        // `platform::io_uring`): the kernel reads the SQE's referenced
        // buffer asynchronously after the push, and the only safe release
        // point is the completion's consumption — which `submit_and_wait`
        // guarantees happens before it returns. `bytes` is borrowed (the
        // caller may drop it after this call returns), so the payload is
        // copied into this frame's Vec and THIS function is the
        // responsible owner. Short writes advance the offset and
        // re-submit the drained remainder of the same owned Vec; the Vec
        // is not freed until the final completion is consumed.
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
        // until the completion is consumed (`submit_and_wait` returns
        // only after collection); `dir` outlives the call. The handle
        // eviction above precedes the unlink so no cached reader can be
        // mid-op on a vanished file, and NotFound is tolerated because
        // delete is idempotent.
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
        // -----------------------------------------------------------------
        // Stage 1: materialize per-request state — destination buffers
        // (`stored_len` bytes each, owned here), absolute payload offsets
        // (record data begins at `offset + RECORD_HEADER_SIZE`), and the
        // per-request result slots.
        // -----------------------------------------------------------------
        // Per-request state: destination buffers are owned here and live
        // until every completion is consumed — the platform contract.
        // `submit_and_wait` returns only after collection, so the `bufs`
        // Vec (re-submitted across short-read iterations) is released at
        // the safe point.
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
        // -----------------------------------------------------------------
        // Stage 2: resolve fds up front; a request whose segment cannot
        // be opened fails on its own slot, and the rest of the batch
        // proceeds.
        // -----------------------------------------------------------------
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
        // -----------------------------------------------------------------
        // Stage 3: submission loop — one batch per iteration; short reads
        // loop back into `inflight` with an advanced offset. Tokens are
        // `index + 1` (0 is unused), correlated via `Completion::user_data`
        // in ARBITRARY completion order.
        // -----------------------------------------------------------------
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
