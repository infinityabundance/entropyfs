//! One io_uring ring with a small, safe submit-and-collect API (Phase 10F,
//! ADR-0021). This is the crate's ONLY file containing `unsafe` — the
//! narrowly isolated Linux ABI boundary the unsafe ledger designates.
//!
//! The rest of the crate (including `store::io::uring::UringIo`) calls the
//! safe [`Uring::submit_and_wait`]; no other module needs `unsafe`.

#![allow(unsafe_code)]

use std::sync::Mutex;

use io_uring::IoUring;
use io_uring::squeue::Entry as Sqe;

/// One completion: the caller's token plus the operation's result
/// (non-negative on success, `-errno` on failure — the io_uring CQE
/// contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completion {
    /// The token passed in the submitted entry's `user_data`.
    pub user_data: u64,
    /// Operation result (`read`/`write` byte count, 0 for fsync, or
    /// `-errno`).
    pub result: i32,
}

/// A mutex-guarded io_uring ring.
///
/// Submission and completion collection are atomic per call, so a caller
/// that submits a batch can rely on receiving exactly that batch's
/// completions before the call returns — the ordering and durability
/// semantics the storage transport needs. One ring is correct for the
/// store: the write path is serialized (`commit_lock` + segment mutex) and
/// batches are the read-path parallelism unit.
pub struct Uring {
    ring: Mutex<IoUring>,
    entries: u32,
}

impl Uring {
    /// Create a ring with `entries` submission slots (rounded up to the
    /// kernel's power-of-two sizing; at least 8).
    pub fn new(entries: u32) -> std::io::Result<Self> {
        let entries = entries.max(8);
        let ring = IoUring::new(entries)?;
        Ok(Self {
            ring: Mutex::new(ring),
            entries,
        })
    }

    /// The ring capacity (max ops per submit batch).
    pub fn capacity(&self) -> u32 {
        self.entries
    }

    /// Submit `ops` and wait until every one of them has completed.
    ///
    /// Every op must carry a distinct `user_data` token; completions are
    /// returned in ARBITRARY order (the kernel is free to complete
    /// out-of-order), correlated by `Completion::user_data`.
    ///
    /// # Safety contract (caller)
    ///
    /// Each entry's referenced memory (read/write buffers, unlink
    /// pathnames) and file descriptor must remain valid until this call
    /// returns. The function does not return until all completions are
    /// collected, so a caller that owns the buffers for the duration of
    /// the call satisfies the io_uring lifetime rule.
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
                // SAFETY: the caller's contract (documented above) — every
                // buffer and fd referenced by this SQE stays valid until
                // all completions are collected below; pushing copies the
                // SQE into the kernel-shared submission ring.
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
