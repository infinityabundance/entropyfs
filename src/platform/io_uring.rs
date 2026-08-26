//! The crate's ONLY file containing `unsafe`: one io_uring ring behind a
//! small, safe submit-and-collect API (Phase 10F, ADR-0021). This is the
//! narrowly isolated Linux ABI boundary the unsafe ledger designates
//! (`docs/security/unsafe-ledger.md`). The rest of the crate — including
//! the storage transport [`crate::store::io::uring::UringIo`] — calls the
//! safe [`Uring::submit_and_wait`]; no other module needs `unsafe`.
//!
//! # PURPOSE
//!
//! Own the crate's single unsafe primitive — the SQE push into the
//! kernel-shared submission ring — and present it as a safe, synchronous
//! "submit all, wait all, collect all" API. The completion queue needs no
//! `unsafe` here: the `io-uring` crate exposes it through a safe
//! `Iterator`.
//!
//! # BOUNDARY
//!
//! KNOWS: the io_uring ABI through the `io-uring` crate (0.7.14, already
//! a transitive dependency of `libublk` — no new dependency tree),
//! `user_data` completion tokens, and the file descriptors and buffers
//! callers hand in. NEVER KNOWS: the record format, segment layout, store
//! orchestration, or durability ordering — those live above
//! [`crate::store::io`]. Ledger policy additionally forbids this code
//! being reachable from persistent-data parsing (parsers are
//! `forbid(unsafe_code)` and cannot call platform code).
//!
//! # MODEL
//!
//! A batch in, exactly one completion per op out. Callers submit
//! `(token, Sqe)` pairs; the kernel completes each op independently and
//! MAY do so in any order, so completions are correlated by the caller's
//! token, never by position. The API does not return until every
//! completion is collected, so callers observe the same blocking
//! semantics as the synchronous syscalls (`pread`/`pwrite`/`fsync`) the
//! reference backend uses — which is what lets `UringIo` reproduce the
//! sync engine's crash-court injection-point sequence exactly.
//!
//! # PERSISTENT AUTHORITY
//!
//! None for the on-disk format: this is a transport, not a format (ADR
//! 0021: a store is equally mountable with either backend). Persistence
//! semantics are the CALLER's choice of ops — a WRITE completion means
//! the kernel accepted the bytes into its page cache (crash-unsafe
//! alone); an FSYNC completion means the data is durable
//! (power-loss-safe). This module never decides what to write or when to
//! fsync; the store's orchestration above does, and the crash courts
//! check that ordering.
//!
//! # CORRECTNESS INVARIANTS
//!
//! - Exactly one completion per submitted op, in arbitrary order.
//! - Every buffer/fd referenced by a submitted SQE remains valid until
//!   the call returns (the buffer-lifetime contract on
//!   [`Uring::submit_and_wait`]).
//! - Distinct `user_data` tokens within a batch; no aliasing between the
//!   buffers of different ops in a batch.
//! - Submit and collect are atomic under the mutex, so one caller's
//!   completions are never interleaved with another's.
//!
//! # CONCURRENCY
//!
//! One `Mutex<IoUring>`; concurrent callers serialize only on the short
//! submit/collect critical section. ONE ring is correct for the store:
//! the write path is serialized above (`commit_lock` + segment mutex),
//! and batches are the read-path parallelism unit. Deliberately out of
//! scope (ADR-0021): registered fixed buffers/files, `SQPOLL`, `IOPOLL`,
//! and async submission from multiple threads.
//!
//! # DURABILITY
//!
//! The ring itself provides none — it is a syscall transport. What
//! survives a crash is decided by which ops the store lets complete
//! before acknowledging, with exactly the sync backend's syscall
//! semantics (page-cache accept vs fdatasync vs fsync vs dir fsync). The
//! crash courts parameterize over both backends and require the same
//! recoverable state per injection point.
//!
//! # RESOURCE BOUNDS
//!
//! Ring depth: `entries` submission slots (u32; clamped to ≥ 8; the
//! kernel rounds up to a power of two). The `ops` slice may exceed the
//! ring depth; it is chunked internally, and buffers must stay valid
//! across all chunks (the whole call). Buffer sizes are chosen by the
//! caller — the store allocates `stored_len` bytes per read from
//! persistent descriptors, bounded by the decode limits the hostile-media
//! court exercises, never by this module.
//!
//! # PERFORMANCE
//!
//! The ring exists because a materialization's dependencies (models,
//! streams, dictionaries, tree nodes) and a commit's durability sequence
//! can be ONE submission instead of a syscall per op. Measured ring
//! economics (Phase 10F, tmpfs): ~2.3 µs per submit-and-wait cycle (the
//! kernel's submit+wait+wake floor) vs ~0.1 µs per `pread`, amortizing
//! to ~0.34 µs/op at a 32-op batch. The 10F-sync/10F-uring court pair
//! (evidence `fuse-court-*-10f-sync/uring`) shows uring trailing sync by
//! 5–27% (reads −7–12%) on tmpfs, so the default stays `sync` (the
//! oracle) until real-device (NVMe, queue-depth) evidence flips it.
//!
//! # FAILURE MODES
//!
//! Expected: `SubmissionQueue::push` failure (no room in the SQ) and
//! `io_uring_enter` failures surface as `io::Error` from
//! [`Uring::submit_and_wait`]; per-op failures arrive as a negative
//! `Completion::result` (`-errno`), which the caller maps to a typed
//! store error. MUST NEVER HAPPEN: returning without exactly one
//! completion per op — the caller would free (or reuse) a buffer the
//! kernel may still read or write: a use-after-free of kernel-shared
//! memory.
//!
//! # HISTORY / EVIDENCE
//!
//! Phase 10F, ADR-0021. The unsafe isolation is one of the repo's
//! custodial items: `SubmissionQueue::push` is the `io-uring` crate's
//! sole unsafe primitive (the CQ is consumed through its safe `Iterator`),
//! and the ledger pins it HERE with exact preconditions (buffer lifetime,
//! fd validity, alignment, no aliasing), enforced by
//! `unsafe_files_match_ledger` (`src/tests/unsafe_ledger.rs`) and the
//! crate-root `#![deny(unsafe_code)]`. Acceptance is crash-court parity:
//! `src/tests/io_backend_parity.rs` runs the crash matrix against BOTH
//! backends and asserts the store directories are canonically
//! byte-identical at every crash point (evidence
//! `fuse-court-*-10f-sync/uring`).

#![allow(unsafe_code)]

use std::sync::Mutex;

use io_uring::IoUring;
use io_uring::squeue::Entry as Sqe;

/// One completion: the caller's token plus the operation's result — the
/// io_uring CQE contract. A batch in, exactly one completion per op out;
/// completions are correlated by `user_data` because the kernel may
/// complete ops in arbitrary order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completion {
    /// The token passed in the submitted entry's `user_data`. Distinct
    /// within a submission batch.
    pub user_data: u64,
    /// Operation result, in bytes for READ/WRITE (bytes transferred),
    /// 0 for FSYNC/UNLINKAT, or `-errno` on failure.
    pub result: i32,
}

/// A mutex-guarded io_uring ring: the crate's single `unsafe`-owning
/// type (see the module docs for the ledger story).
///
/// ROLE: the one place SQEs are pushed into the kernel-shared submission
/// ring and completions are collected, behind a safe synchronous API.
///
/// INVARIANTS: submission and completion collection are atomic per call,
/// so a caller that submits a batch receives exactly that batch's
/// completions before the call returns — the ordering semantics the
/// storage transport needs; exactly one completion per submitted op;
/// every op's buffers/fd stay valid until the call returns (the safety
/// contract on [`Uring::submit_and_wait`]).
///
/// One ring is correct for the store: the write path is serialized
/// (`commit_lock` + segment mutex) and batches are the read-path
/// parallelism unit.
pub struct Uring {
    ring: Mutex<IoUring>,
    /// Ring depth in ops (submission slots; the kernel rounds up to a
    /// power of two; ≥ 8). The max ops per submit batch.
    entries: u32,
}

impl Uring {
    /// Create a ring with `entries` submission slots — the ring depth, in
    /// ops: the max ops per submit batch. The kernel rounds up to a
    /// power of two; the platform floor is 8.
    pub fn new(entries: u32) -> std::io::Result<Self> {
        let entries = entries.max(8);
        let ring = IoUring::new(entries)?;
        Ok(Self {
            ring: Mutex::new(ring),
            entries,
        })
    }

    /// The ring capacity (max ops per submit batch), in ops.
    pub fn capacity(&self) -> u32 {
        self.entries
    }

    /// Submit `ops` and wait until every one of them has completed.
    ///
    /// Every op must carry a distinct `user_data` token; completions are
    /// returned in ARBITRARY order (the kernel is free to complete
    /// out-of-order), correlated by `Completion::user_data`.
    ///
    /// # Safety contract (caller) — the buffer-lifetime contract
    ///
    /// WHICH memory must outlive the submission: every buffer or pathname
    /// the SQE references — a read destination, a write payload, an
    /// unlink pathname — plus the file descriptor. `push` only COPIES the
    /// `Entry` struct into the kernel-shared submission ring; what the
    /// kernel dereferences later, asynchronously, are the pointers inside
    /// it. Those must name live memory until the kernel has finished.
    ///
    /// WHO is responsible: the caller. This function does not return
    /// until exactly one completion per submitted op has been collected,
    /// and a completion's consumption is the kernel's last touch of that
    /// op's buffers. So a caller that owns every referenced buffer in its
    /// own frame for the duration of the call satisfies the io_uring
    /// lifetime rule BY CONSTRUCTION — releasing (or mutating) a buffer
    /// before the call returns is a use-after-free of kernel-shared
    /// memory, i.e. undefined behavior.
    ///
    /// WHAT happens on completion: the kernel has finished reading
    /// (write ops) or writing (read ops) the buffer; `Completion::result`
    /// reports the transferred byte count (or `-errno`), and the caller
    /// may then free or reuse the buffer. For multi-chunk submissions the
    /// buffers must remain valid across ALL chunks — the contract is
    /// "until the call returns", not "until the chunk's completions are
    /// collected".
    ///
    /// Submissions larger than the ring capacity are chunked internally;
    /// the caller still receives exactly one completion per submitted op.
    pub fn submit_and_wait(&self, ops: &[(u64, Sqe)]) -> std::io::Result<Vec<Completion>> {
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        let mut ring = self.ring.lock().expect("uring poisoned");
        let want = ops.len();
        let chunk = self.entries as usize;
        let mut out: Vec<Completion> = Vec::with_capacity(want);
        let mut submitted = 0usize;
        while submitted < want {
            let take = (want - submitted).min(chunk);
            let mut sq = ring.submission();
            for (token, e) in &ops[submitted..submitted + take] {
                // SAFETY: the caller's contract (documented on
                // `submit_and_wait`) — every buffer and fd referenced by
                // this SQE stays valid until all completions are
                // collected below; `push` only COPIES the Entry into the
                // kernel-shared submission ring, and the kernel's
                // dereference of the referenced memory happens before the
                // matching completion is drained from the CQ below. For
                // chunked batches the loop keeps the caller's buffers
                // alive across every chunk (the call has not returned).
                unsafe {
                    sq.push(&e.clone().user_data(*token))
                        .map_err(|e| std::io::Error::other(format!("sq push: {e:?}")))?;
                }
            }
            drop(sq);
            while out.len() < submitted + take {
                ring.submitter()
                    .submit_and_wait(submitted + take - out.len())?;
                let mut cq = ring.completion();
                cq.sync();
                // `CompletionQueue` is a safe iterator over the CQ ring;
                // the queue was synced after a successful submit_and_wait,
                // so every `next` yields a real completion.
                for cqe in cq.by_ref() {
                    out.push(Completion {
                        user_data: cqe.user_data(),
                        result: cqe.result(),
                    });
                }
            }
            submitted += take;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use io_uring::opcode;

    #[test]
    fn nop_completes() {
        let ring = Uring::new(8).unwrap();
        let completions = ring
            .submit_and_wait(&[(7, opcode::Nop::new().build())])
            .unwrap();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].user_data, 7);
        assert_eq!(completions[0].result, 0);
    }

    #[test]
    fn batch_out_of_order_ok() {
        let ring = Uring::new(8).unwrap();
        let ops: Vec<(u64, Sqe)> = (0..32).map(|i| (i, opcode::Nop::new().build())).collect();
        let completions = ring.submit_and_wait(&ops).unwrap();
        assert_eq!(completions.len(), 32);
        // Every token present exactly once (order may vary).
        let mut tokens: Vec<u64> = completions.iter().map(|c| c.user_data).collect();
        tokens.sort_unstable();
        assert_eq!(tokens, (0..32).collect::<Vec<u64>>());
        assert!(completions.iter().all(|c| c.result == 0));
    }

    #[test]
    fn write_read_roundtrip() {
        use io_uring::types::Fd;
        use std::fs::OpenOptions;
        use std::os::fd::AsRawFd;
        let ring = Uring::new(8).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("f");
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        // Write then read back via the ring.
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let mut buf = payload.clone();
        let w = ring
            .submit_and_wait(&[(
                1,
                opcode::Write::new(Fd(file.as_raw_fd()), buf.as_ptr(), buf.len() as u32)
                    .offset(0)
                    .build(),
            )])
            .unwrap();
        assert_eq!(w[0].result, 4096);
        let mut out = vec![0u8; 4096];
        let r = ring
            .submit_and_wait(&[(
                2,
                opcode::Read::new(Fd(file.as_raw_fd()), out.as_mut_ptr(), out.len() as u32)
                    .offset(0)
                    .build(),
            )])
            .unwrap();
        assert_eq!(r[0].result, 4096);
        assert_eq!(out, payload);
        // Buffer lifetime: payload/out live until both completions are
        // collected above — satisfied.
        let _ = &mut buf;
    }
}
