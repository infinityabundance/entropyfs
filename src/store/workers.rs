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
    /// Threads currently parked on the condvar (queue depth).
    waiters: usize,
    /// Peak concurrent waiters observed.
    max_waiters: usize,
}

/// Phase-11D oracle snapshot of the worker budget (diagnostic counters).
#[derive(Debug, Clone, Copy, Default)]
pub struct WorkerOracleSnapshot {
    /// Sum of requested worker slots.
    pub requested: u64,
    /// Sum of granted worker slots.
    pub granted: u64,
    /// Acquires that had to wait for the semaphore.
    pub blocked: u64,
    /// Grant events (worker batches).
    pub batches: u64,
    /// Peak concurrent threads parked on the condvar.
    pub max_queue_depth: u64,
}

/// The process-wide worker budget (a counting semaphore over search/decode
/// worker slots).
pub struct WorkerBudget {
    state: Mutex<BudgetState>,
    cv: Condvar,
    /// Phase-11D oracle counters (diagnostic; never affect scheduling).
    requested: AtomicUsize,
    granted: AtomicUsize,
    blocked: AtomicUsize,
    batches: AtomicUsize,
}

impl WorkerBudget {
    const fn new() -> Self {
        Self {
            state: Mutex::new(BudgetState {
                cap: 0, // lazily set at the first acquisition
                in_flight: 0,
                waiters: 0,
                max_waiters: 0,
            }),
            cv: Condvar::new(),
            requested: AtomicUsize::new(0),
            granted: AtomicUsize::new(0),
            blocked: AtomicUsize::new(0),
            batches: AtomicUsize::new(0),
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
        self.requested
            .fetch_add(want, std::sync::atomic::Ordering::Relaxed);
        if st.in_flight + want > st.cap {
            st.waiters += 1;
            st.max_waiters = st.max_waiters.max(st.waiters);
            self.blocked
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            while st.in_flight + want > st.cap {
                st = self.cv.wait(st).expect("worker budget poisoned");
            }
            st.waiters -= 1;
        }
        st.in_flight += want;
        self.granted
            .fetch_add(want, std::sync::atomic::Ordering::Relaxed);
        self.batches
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

    /// Phase-11D oracle: the budget's cumulative counters.
    pub fn snapshot(&self) -> WorkerOracleSnapshot {
        let st = self.state.lock().expect("worker budget poisoned");
        WorkerOracleSnapshot {
            requested: self.requested.load(std::sync::atomic::Ordering::Relaxed) as u64,
            granted: self.granted.load(std::sync::atomic::Ordering::Relaxed) as u64,
            blocked: self.blocked.load(std::sync::atomic::Ordering::Relaxed) as u64,
            batches: self.batches.load(std::sync::atomic::Ordering::Relaxed) as u64,
            max_queue_depth: st.max_waiters as u64,
        }
    }
}

/// The process-wide budget.
pub static WORKERS: WorkerBudget = WorkerBudget::new();

/// The oracle's worker clock: true thread-CPU time (`CLOCK_THREAD_CPUTIME`)
/// where the kernel provides it, wall time as the fallback. This is what
/// lets the 11D oracle distinguish USEFUL search/decode CPU from semaphore
/// queue wait and spawn/join overhead — `prepare` is otherwise one opaque
/// bucket.
pub struct WorkerClock {
    cpu: Option<u64>,
    wall: std::time::Instant,
}

impl WorkerClock {
    /// Start the clock.
    pub fn start() -> Self {
        Self {
            cpu: thread_cpu_ns(),
            wall: std::time::Instant::now(),
        }
    }

    /// Nanoseconds elapsed on THIS thread (CPU time when available).
    pub fn elapsed_ns(&self) -> u64 {
        match self.cpu {
            Some(c0) => thread_cpu_ns().unwrap_or(c0).saturating_sub(c0),
            None => self.wall.elapsed().as_nanos() as u64,
        }
    }
}

/// `CLOCK_THREAD_CPUTIME_ID` nanoseconds (Linux); `None` elsewhere.
fn thread_cpu_ns() -> Option<u64> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = rustix::time::ClockId::ThreadCPUTime;
        None
    }
    #[cfg(target_os = "linux")]
    {
        let t = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
        Some(t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64)
    }
}

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
