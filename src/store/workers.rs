//! Phase-11C: the process-wide worker budget.
//!
//! The Phase-10C candidate search and the batched read decode each spawn
//! `available_parallelism()` scoped threads per request. With T concurrent
//! requests that is T×N threads on an N-core box — the oversubscription
//! the 11B reconciliation measured as the `read_decode`/`prepare` inflation
//! that grows exactly where the write plateau flattens (the 16-thread FUSE
//! court measured the per-chunk search at ~11× its single-request cost).
//!
//! This module caps the TOTAL number of concurrently-running search/decode
//! workers across the whole process at `available_parallelism()` with a
//! SEMAPHORE: a request that cannot acquire its workers WAITS (its thread
//! parks; it burns no CPU) until a running batch finishes, then takes the
//! full machine for its own batch. Consequence: the search/decode CPU is
//! bounded by the machine at every thread count (no T×N thrash), and the
//! wall time converges to the single-request floor — the plateau becomes a
//! flat line. A non-blocking "grant 0 → run inline" fallback was measured
//! and rejected: the inline requests' serial searches competed with the
//! workers' threads, inflating the search CPU ~5× at 16 threads.
//!
//! The CPU is the shared resource; the budget is about the machine's
//! cores, so it is process-global (a single store process serves one
//! filesystem).
//!
//! Deadlock safety: the budget is acquired ONLY on paths that hold no
//! other store lock (the epoch guard is released before prepare/decode —
//! 11B/11C — and the commit lock is never held during them), and it is
//! released before the acquiring phase returns, so no lock-ordering cycle
//! can form.

#![forbid(unsafe_code)]

use std::sync::atomic::AtomicUsize;
use std::sync::{Condvar, Mutex};

/// The budget cap: the machine's available parallelism. Cached (the first
/// call pays `sched_getaffinity`; every call after is a load).
static CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn cap() -> usize {
    *CAP.get_or_init(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .max(1)
    })
}

/// The semaphore state.
struct BudgetState {
    cap: usize,
    in_flight: usize,
}

/// The process-wide worker budget (a counting semaphore over search/decode
/// worker slots).
pub struct WorkerBudget {
    state: Mutex<BudgetState>,
    cv: Condvar,
    /// Diagnostics: total worker batches granted (for the perf diag).
    grants: AtomicUsize,
}

impl WorkerBudget {
    const fn new() -> Self {
        Self {
            state: Mutex::new(BudgetState {
                cap: 0, // lazily set at the first acquisition
                in_flight: 0,
            }),
            cv: Condvar::new(),
            grants: AtomicUsize::new(0),
        }
    }

    /// Acquire `want` worker slots (clamped to the machine's parallelism),
    /// BLOCKING until they are available. The caller must release the
    /// grant (via [`WorkerGrant`]'s drop) once its scoped threads join.
    fn acquire(&self, want: usize) -> WorkerGrant {
        let want = want.min(cap()).max(1);
        let mut st = self.state.lock().expect("worker budget poisoned");
        if st.cap == 0 {
            st.cap = cap();
        }
        while st.in_flight + want > st.cap {
            st = self.cv.wait(st).expect("worker budget poisoned");
        }
        st.in_flight += want;
        self.grants
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        drop(st);
        WorkerGrant { n: want }
    }

    /// Release `n` previously-acquired worker slots.
    fn release(&self, n: usize) {
        if n == 0 {
            return;
        }
        let mut st = self.state.lock().expect("worker budget poisoned");
        st.in_flight -= n;
        drop(st);
        self.cv.notify_all();
    }
}

/// The process-wide budget.
pub static WORKERS: WorkerBudget = WorkerBudget::new();

/// An RAII worker grant: releases the reservation on drop (including the
/// panic path — the scoped threads' join propagates a worker panic before
/// the grant drops, so the slots are always returned).
#[must_use]
pub struct WorkerGrant {
    n: usize,
}

impl WorkerGrant {
    /// The granted worker count.
    pub fn n(&self) -> usize {
        self.n
    }
}

impl Drop for WorkerGrant {
    fn drop(&mut self) {
        WORKERS.release(self.n);
    }
}

/// Acquire up to `want` workers (blocking); the grant is released when it
/// drops. Returns a grant of at least 1 (a caller may run its work inline
/// with 1 slot, or spawn the granted threads).
pub fn grant(want: usize) -> WorkerGrant {
    WORKERS.acquire(want)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_serializes_oversized_requests() {
        let a = grant(usize::MAX);
        assert!(a.n() >= 1);
        // A second full-size request must wait: prove it with a probe
        // thread that cannot acquire while `a` is held.
        let held = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let held2 = std::sync::Arc::clone(&held);
        let probe = std::thread::spawn(move || {
            // Start the acquisition on a separate thread; it must NOT
            // complete while the budget is held by `a`.
            let b = grant(usize::MAX);
            held2.store(b.n() > 0, std::sync::atomic::Ordering::Relaxed);
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!held.load(std::sync::atomic::Ordering::Relaxed));
        drop(a);
        probe.join().unwrap();
        assert!(held.load(std::sync::atomic::Ordering::Relaxed));
    }
}
