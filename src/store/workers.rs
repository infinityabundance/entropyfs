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
//!
//! # Phase-11E: the persistent fair worker pool (probe-sealed, KEPT)
//!
//! The 11D oracle (`docs/performance/worker-oracle.md`; sealed evidence
//! `evidence/performance/worker-oracle-1787765041-052bc46/`) decomposed the
//! semaphore's opaque `prepare` bucket at 1/2/4/8/16 writers and found:
//!
//! ```text
//! useful search CPU  9.98 / 10.00 / 9.97 / 9.94 / 9.83 s   (constant ±2%)
//! semaphore queue    4.6% -> 91.7% of prepare              (1 -> 16 writers)
//! 16-writer wall     1.14 s  == the SMT-adjusted CPU floor (9.8 s / 8 cores)
//! p50 / p99          5.3 -> 52.4 ms / 9.5 -> 177.6 ms      (1 -> 16 writers)
//! ```
//!
//! The semaphore is a BATCH-granularity scheduler: a request reserves ALL
//! of its workers or none, so T writers run whole batches strictly one at a
//! time and every competing request parks ~50 ms. [`SearchPool`] is the
//! task-level FAIR alternative the 11D decision called for — persistent
//! workers, per-request queues served round-robin, bounded with
//! backpressure at submission.
//!
//! The 11E probe (`src/tests/worker_pool_probe.rs`, sealed evidence
//! `evidence/performance/worker-pool-probe-<run>/`) measured it against the
//! semaphore on the same workload and the pool was KEPT. The mounted-FUSE
//! 11E court (`tools/court-worker-pool-mount.sh`, sealed
//! `evidence/performance/worker-pool-mount-court-<run>/`) then validated it
//! END-TO-END over real FUSE workloads (1/4/8/16 session threads;
//! parallel writes/reads, namespace ops, tree copies, untar, make -j,
//! cargo build, mixed R/W, fsync-heavy, serial controls) and sealed the
//! pool as the MOUNT DEFAULT — pool-16 at 16 FUSE threads: parallel write
//! +14%, latency-battery wall −26%, p95 −39%, p99 −48%, CPU +2.8%,
//! crash/fsck/readback clean. The mount now runs the pool by default with
//! `available_parallelism()` workers; `--no-worker-pool` forces the 11C
//! semaphore (the fallback):
//!
//! ```text
//! 16 writers, pool-16 vs semaphore (sealed release run):
//!   wall      785 ms  vs 1107 ms   (-29%; the batch-transition slack)
//!   p50       47 ms   vs 49 ms
//!   p99       78 ms   vs 241 ms    (-68%; the head-of-line tail)
//!   useful CPU 10259  vs 9988 ms   (+2.7%; the DSFB-mutex visibility /
//!                                   SMT cost of the higher parallelism —
//!                                   the 11F sharding is the follow-up)
//!   p99/p50   1.66    vs 4.90
//!   max slow  18.4x   vs 60.3x
//! 8 writers:  wall -34%, p99 -69%, useful CPU +3.7-6.6% (the same trade)
//! ```
//!
//! Architecture rules (from the 11D brief): typed tasks ONLY
//! ([`WorkerTask::EncodeChunk`]/[`WorkerTask::DecodeExtent`]) — no generic
//! executor, async runtime, futures abstraction, or work-stealing;
//! per-REQUEST queues served round-robin, one task per pick; "execution
//! order may vary; persisted semantic order may not" (results reassemble
//! strictly by ordinal); bounded queue with backpressure AT SUBMISSION;
//! the DSFB observer is deliberately untouched (11D identified it as an
//! independent fix — mixing it in would destroy attribution).

#![forbid(unsafe_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Instant;

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

// =========================================================================
// Phase-11E: the persistent fair worker-pool PROBE
// =========================================================================
//
// WHY THIS EXISTS
//
//   The 11D oracle decomposed the 11C semaphore's opaque `prepare` bucket
//   (see the module doc above): useful search CPU is constant across writer
//   counts, throughput sits at the CPU floor, and the ONLY available win is
//   the latency distribution. The semaphore serializes whole BATCHES, so a
//   request parks ~50 ms while other batches run; a task-level fair pool
//   can interleave chunks from every writer and cap the tail.
//
// WHAT THIS IS
//
//   A persistent pool of typed worker threads. Tasks are one of two
//   EntropyFS operations (never arbitrary closures):
//
//     EncodeChunk { request_id, ordinal, chunk data, params }  (search)
//     DecodeExtent { request_id, ordinal, descriptor, ctx }    (read)
//
//   Requests hold per-request task queues; workers serve the ACTIVE
//   REQUESTS round-robin, one task per pick:
//
//     requests: A -> [A0 A1 ..]  B -> [B0 B1 ..]  C -> [C0 C1 ..]
//     workers pick: A0 B0 C0 D0 / A1 B1 C1 D1 / A2 B2 C2 D2 ..
//
//   A request's results are reassembled STRICTLY BY ORDINAL by the caller:
//   execution order may vary, persisted semantic order may not. Worker
//   scheduling therefore can never become decoding authority.
//
// BOUNDED QUEUE / BACKPRESSURE
//
//   Submitted-but-unfinished tasks are bounded at `capacity = queue_factor
//   x workers`; submission BLOCKS when the bound is reached. 16 FUSE
//   writers must not enqueue 16x16 = 256 tasks and ack as if the work were
//   free — submission itself is the backpressure point.
//
// WHY THE TASKS CARRY Arc<Store>
//
//   The search needs the store (dedup lookups, DSFB, bases, validation,
//   perf). The pool's worker threads outlive the submit frame, so each
//   task owns an Arc<Store> clone. The pool itself holds only Weak<Store>
//   (set by `SearchPool::bind`): the pool never keeps the store alive
//   beyond the tasks that reference it.
//
// CONCURRENCY / DEADLOCK RULES
//
//   Two locks: the shared QUEUES lock (active ring + per-request pending
//   queues + backpressure accounting) and each request's RESULTS lock
//   (result slots + completion counter + the requester's condvar). Lock
//   orderings, all acyclic:
//
//     worker   pick:     queues -> (release) -> state.first_service
//     worker   deliver:  state.results -> state.finished_at
//     submitter submit:  backpressure wait (released) -> queues
//     submitter join:    state.results -> state.finished_at/first_service
//
//   Workers hold NO store lock while parked and take the queues lock only
//   to pop a task — the same rule the 11C semaphore follows, so no
//   lock-order cycle with the store's locks can form.
//
// LIFECYCLE
//
//   LIFECYCLE
//
//   POOL defaults to ON for the FUSE daemon (the mounted-FUSE 11E court
//   sealed it as the mount default; the daemon sizes it at
//   available_parallelism() and `--no-worker-pool` restores the 11C
//   semaphore), and the probe test enables it, binds a store, runs a
//   sweep, and disables it. The 11E probe sealed the adoption decision:
//   pool-16 was KEPT (the gates below; the semaphore stays as
//   the fallback).
// =========================================================================

/// One typed unit of search/decode work (Phase-11E probe).
///
/// The task owns EVERYTHING its worker needs: the store (an `Arc` clone —
/// the worker thread outlives the submit frame), the chunk/extent payload,
/// and the encode parameters. No borrowing across threads.
///
/// `pub(crate)`: the task types are store-internal (the public surface is
/// the store API); the enum's fields carry `pub(crate)` types (`Composed`).
#[allow(clippy::large_enum_variant)]
pub(crate) enum WorkerTask {
    /// Encode one composed chunk through the guided candidate search
    /// (Phase-10C phase 2). `ordinal` is the chunk's position within the
    /// request's batch; the caller reassembles by it.
    EncodeChunk {
        /// Chunk position within the request; results reassemble by this.
        ordinal: usize,
        /// The store the search reads through (dedup, DSFB, bases, perf).
        store: Arc<crate::store::Store>,
        /// Target inode (search context only — never persisted here).
        ino: u64,
        /// The composed chunk: final bytes, content id, prev version,
        /// in-batch dictionary, synthetic batch view.
        composed: super::Composed,
        /// Encode resource limits.
        limits: crate::core::limits::Limits,
        /// Family gates for this encode.
        options: crate::optimizer::policy::OptimizeOptions,
        /// Foreground search policy (probe runs use the store's own).
        fg: crate::optimizer::foreground::ForegroundPolicy,
    },
    /// Materialize one extent into its byte window (the decode half of a
    /// batched read). The task carries the descriptor plus the prefetched
    /// object and nested-descriptor maps, so decode needs no epoch guard
    /// and no store I/O beyond the fallback path (Phase-11C two-phase
    /// read structure preserved).
    DecodeExtent {
        /// Extent position within the request's batch.
        ordinal: usize,
        /// The store (fallback object resolution + perf).
        store: Arc<crate::store::Store>,
        /// Logical start offset of the extent.
        start: u64,
        /// The extent's representation descriptor.
        desc: crate::core::representation::Representation,
        /// Prefetched objects (read window); shared across the batch.
        objects: Arc<HashMap<crate::core::extent::ChunkId, Vec<u8>>>,
        /// Nested descriptors resolved by the prepare half (Phase-11C).
        descriptors: Arc<HashMap<crate::core::extent::ChunkId, Vec<u8>>>,
        /// Decode resource limits.
        limits: crate::core::limits::Limits,
    },
}

impl WorkerTask {
    /// Execute the task on the calling (pool) thread.
    ///
    /// Returns `(ordinal, thread_cpu_ns, result)` — the CPU time is the
    /// 11D Gate-C measurement (per-worker `CLOCK_THREAD_CPUTIME`), summed
    /// per request so the pool path reports the same `worker_useful_cpu`
    /// row as the semaphore path.
    fn execute(self) -> (usize, u64, Result<WorkerOutcome, crate::store::StoreError>) {
        match self {
            WorkerTask::EncodeChunk {
                ordinal,
                store,
                ino,
                composed,
                limits,
                options,
                fg,
            } => {
                let t0 = WorkerClock::start();
                let r = super::encode_prepared_chunk(&store, &composed, ino, limits, options, fg);
                store.perf().record("worker_tasks", 0);
                let cpu = t0.elapsed_ns();
                (ordinal, cpu, Ok(WorkerOutcome::Encode(r)))
            }
            WorkerTask::DecodeExtent {
                ordinal,
                store,
                start,
                desc,
                objects,
                descriptors,
                limits,
            } => {
                let t0 = WorkerClock::start();
                let ctx = crate::store::epoch::PrefetchContext::new(
                    &store,
                    &objects,
                    Some(&descriptors),
                    None,
                );
                let mut chunk = vec![0u8; desc.len() as usize];
                let mut budget = limits.max_decode_work;
                let r = crate::core::materialize::materialize(
                    &desc,
                    &ctx,
                    &limits,
                    0,
                    &mut budget,
                    &mut chunk,
                )
                .map_err(|e| crate::store::StoreError::Descriptor(e.to_string()))
                .map(|()| WorkerOutcome::Decode((start, chunk)));
                store.perf().record("worker_tasks", 0);
                let cpu = t0.elapsed_ns();
                (ordinal, cpu, r)
            }
        }
    }
}

/// The typed result of one task (Phase-11E).
#[derive(Debug, Clone)]
pub struct WorkerResult {
    /// The owning request.
    pub request_id: u64,
    /// Position within the request; the caller reassembles by this.
    pub ordinal: usize,
    /// The task outcome.
    pub result: Result<WorkerOutcome, crate::store::StoreError>,
}

/// The outcome of one executed task (Phase-11E). The variants carry very
/// different payload sizes (an encode holds the full search outcome);
/// boxing would add an indirection to every task — the size difference
/// is accepted and documented (internal enum, not persisted).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum WorkerOutcome {
    /// An encode: the chunk's search outcome (flatten updates, guided
    /// outcome, post-flatten prev version) — exactly `ChunkResult`.
    Encode(super::ChunkResult),
    /// A decode: the materialized extent `(logical start, bytes)`.
    Decode((u64, Vec<u8>)),
}

/// Per-request completion + fairness metrics (Phase-11E).
///
/// Units: nanoseconds (wall). `cpu_ns` is the SUM of the request's task
/// thread-CPU across the pool workers (a CPU sum, not a wall slice — it
/// may exceed the request's wall; see the 11D oracle's Gate-C note).
#[derive(Debug, Clone, Copy)]
pub struct RequestMetrics {
    /// Submit -> first task service start (the pool analog of the
    /// semaphore's queue wait; Gate A).
    pub queue_wait_ns: u64,
    /// Submit -> last task completion (the pool round-trip; Gate B's
    /// wall analog — the pool's `worker_scope_wall`).
    pub span_ns: u64,
    /// Sum of the request's task thread-CPU (Gate C).
    pub cpu_ns: u64,
    /// Task count.
    pub tasks: usize,
}

/// A submitted request: waits for its tasks and reassembles by ordinal.
#[must_use = "the request's results are the reassembly; join must be called"]
pub struct PoolSubmit {
    state: Arc<RequestState>,
}

impl PoolSubmit {
    /// Block until every task of this request completed, then return the
    /// results IN ORDINAL ORDER plus the request's metrics.
    ///
    /// This is the pool's determinism point: the workers may have executed
    /// the tasks in any order, but the caller consumes them strictly by
    /// ordinal — persisted semantic order never depends on scheduling.
    pub fn join(self) -> (Vec<WorkerResult>, RequestMetrics) {
        let state = &self.state;
        let mut g = state.results.lock().expect("pool request results poisoned");
        while state.done.load(Ordering::Acquire) < state.total {
            g = state.cv.wait(g).expect("pool request results poisoned");
        }
        let out: Vec<WorkerResult> = g
            .iter()
            .map(|s| s.as_ref().expect("completed slot must be present").clone())
            .collect();
        let finished = *state
            .finished_at
            .lock()
            .expect("pool request finished_at poisoned");
        let first = *state
            .first_service
            .lock()
            .expect("pool request first_service poisoned");
        let metrics = RequestMetrics {
            queue_wait_ns: first
                .map(|t| t.duration_since(state.submitted_at).as_nanos() as u64)
                .unwrap_or(0),
            span_ns: finished
                .map(|t| t.duration_since(state.submitted_at).as_nanos() as u64)
                .unwrap_or(0),
            cpu_ns: state.cpu_ns.load(Ordering::Relaxed),
            tasks: state.total,
        };
        drop(g);
        (out, metrics)
    }
}

/// Pool-level diagnostics for the probe's explicit fairness measurement.
#[derive(Debug, Clone, Copy)]
pub struct PoolDiagnostics {
    /// Peak concurrently submitted-but-unfinished tasks (queue depth).
    pub peak_in_flight: usize,
    /// Backpressure capacity (`queue_factor x workers`).
    pub capacity: usize,
    /// Worker thread count.
    pub workers: usize,
    /// Maximum observed consecutive tasks picked from ONE request by a
    /// single worker (fairness witness: stays 1 whenever >= 2 requests are
    /// active; grows only when a request monopolizes the ring).
    pub max_consecutive_same_request: usize,
}

/// Per-request state: result slots by ordinal, completion signaling, and
/// the fairness metrics. NOT guarded by the queues lock (the submitter
/// waits on it without blocking the workers' ring).
struct RequestState {
    id: u64,
    total: usize,
    /// Result slots indexed by ordinal (all `Some` once `done == total`).
    results: Mutex<Vec<Option<WorkerResult>>>,
    /// Completed task count (workers increment; the submitter waits on it).
    done: AtomicUsize,
    /// Wakes the submitter when `done == total`.
    cv: Condvar,
    submitted_at: Instant,
    /// Set by the first worker that picks a task of this request.
    first_service: Mutex<Option<Instant>>,
    /// Set by the worker that delivers the LAST result.
    finished_at: Mutex<Option<Instant>>,
    /// Sum of task thread-CPU (Gate C).
    cpu_ns: AtomicU64,
}

impl RequestState {
    fn new(id: u64, total: usize) -> Self {
        Self {
            id,
            total,
            results: Mutex::new((0..total).map(|_| None).collect()),
            done: AtomicUsize::new(0),
            cv: Condvar::new(),
            submitted_at: Instant::now(),
            first_service: Mutex::new(None),
            finished_at: Mutex::new(None),
            cpu_ns: AtomicU64::new(0),
        }
    }

    /// Record the first time a worker picked a task of this request
    /// (queue-wait metric). Called under the queues lock at pick time.
    fn mark_first_service(&self) {
        let mut f = self
            .first_service
            .lock()
            .expect("pool request first_service poisoned");
        if f.is_none() {
            *f = Some(Instant::now());
        }
    }

    /// Deliver one task result (called by the worker AFTER it released
    /// the queues lock).
    fn deliver(
        &self,
        ordinal: usize,
        result: Result<WorkerOutcome, crate::store::StoreError>,
        cpu_ns: u64,
    ) {
        let mut g = self.results.lock().expect("pool request results poisoned");
        let slot = g
            .get_mut(ordinal)
            .expect("ordinal within the request's result slots");
        debug_assert!(slot.is_none(), "task executed twice: ordinal {ordinal}");
        *slot = Some(WorkerResult {
            request_id: self.id,
            ordinal,
            result,
        });
        self.cpu_ns.fetch_add(cpu_ns, Ordering::Relaxed);
        if self.done.fetch_add(1, Ordering::AcqRel) + 1 == self.total {
            *self
                .finished_at
                .lock()
                .expect("pool request finished_at poisoned") = Some(Instant::now());
            self.cv.notify_all();
        }
        drop(g);
    }
}

/// The active-request ring + per-request pending queues, one lock.
struct PoolQueues {
    /// Request ids with at least one pending task, in submission order.
    active: Vec<u64>,
    /// All live requests (pending + in-execution), by id.
    requests: HashMap<u64, RequestEntry>,
}

impl PoolQueues {
    fn new() -> Self {
        Self {
            active: Vec::new(),
            requests: HashMap::new(),
        }
    }
}

/// One live request: its pending task queue + its completion state.
struct RequestEntry {
    pending: VecDeque<WorkerTask>,
    state: Arc<RequestState>,
}

/// The pool's shared state, owned by every worker and the submit API.
struct PoolShared {
    /// The bound store (Weak: the pool never keeps it alive).
    store: Mutex<Option<Weak<crate::store::Store>>>,
    queues: Mutex<PoolQueues>,
    /// Backpressure accounting: submitted-but-unfinished tasks.
    in_flight: AtomicUsize,
    /// Peak observed in_flight (the queue-depth measurement).
    peak_in_flight: AtomicUsize,
    /// Backpressure bound: `queue_factor x workers`.
    capacity: usize,
    /// Worker exit flag (drain-then-exit when the active ring empties).
    shutdown: std::sync::atomic::AtomicBool,
    /// Wakes parked workers (new task) and backpressure waiters (space).
    wake: Condvar,
    /// The backpressure condvar's associated mutex (the wait lock).
    backpressure: Mutex<()>,
    /// Fairness witness: max consecutive picks of one request by one worker.
    max_consecutive: AtomicUsize,
    /// Monotonic request id source.
    next_request_id: AtomicU64,
}

impl PoolShared {
    fn new(workers: usize, queue_factor: usize) -> Self {
        Self {
            store: Mutex::new(None),
            queues: Mutex::new(PoolQueues::new()),
            in_flight: AtomicUsize::new(0),
            peak_in_flight: AtomicUsize::new(0),
            capacity: workers.saturating_mul(queue_factor).max(1),
            shutdown: std::sync::atomic::AtomicBool::new(false),
            wake: Condvar::new(),
            backpressure: Mutex::new(()),
            max_consecutive: AtomicUsize::new(0),
            next_request_id: AtomicU64::new(1),
        }
    }

    /// Submit one request's tasks. Blocks (backpressure) until the
    /// in-flight bound admits them; the request becomes visible to the
    /// workers only once ALL its tasks are queued (no partial visibility).
    ///
    /// BACKPRESSURE RULE: a request is admitted when nothing is in flight
    /// EVEN IF it exceeds the capacity — an idle pool must never refuse
    /// work, and a single request is its own lower bound (a 64-extent read
    /// decode is one request). The bound governs CONCURRENT queued work:
    /// while other requests' tasks are in flight, a new request waits
    /// until `in_flight + total <= capacity`. The Phase-11E probe found
    /// the naive `in_flight + total > capacity` wait deadlocks when an
    /// oversized request meets an idle pool: `in_flight` is 0 and can
    /// never drop further, so the wait never admits it.
    fn submit(&self, request_id: u64, tasks: Vec<WorkerTask>) -> PoolSubmit {
        let total = tasks.len();
        assert!(total > 0, "pool submit with no tasks");
        let mut b = self
            .backpressure
            .lock()
            .expect("pool backpressure poisoned");
        while self.in_flight.load(Ordering::Acquire) != 0
            && self.in_flight.load(Ordering::Acquire).saturating_add(total) > self.capacity
        {
            b = self.wake.wait(b).expect("pool backpressure poisoned");
        }
        drop(b);
        let state = Arc::new(RequestState::new(request_id, total));
        let entry = RequestEntry {
            pending: tasks.into(),
            state: Arc::clone(&state),
        };
        let mut q = self.queues.lock().expect("pool queues poisoned");
        assert!(
            !q.requests.contains_key(&request_id),
            "duplicate pool request id {request_id}"
        );
        q.requests.insert(request_id, entry);
        q.active.push(request_id);
        self.in_flight.fetch_add(total, Ordering::AcqRel);
        self.peak_in_flight
            .fetch_max(self.in_flight.load(Ordering::Relaxed), Ordering::Relaxed);
        drop(q);
        self.wake.notify_all();
        PoolSubmit { state }
    }
}

/// The persistent worker pool (Phase-11E probe; see the module doc).
///
/// # Lifecycle
///
/// `POOL` defaults to disabled. The FUSE daemon opts in per mount
/// (`--worker-pool N`; the pool is the mount default since the mounted-
/// FUSE 11E court sealed it, and `--no-worker-pool` restores the
/// semaphore), and the probe test
/// enables it, binds a store, runs a sweep, and disables it. The workers
/// park on their condvar while idle and are joined at disable/unmount.
pub struct SearchPool {
    runtime: Mutex<Option<PoolRuntime>>,
}

/// One enabled pool instance: the shared state + its worker threads.
struct PoolRuntime {
    shared: Arc<PoolShared>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl PoolRuntime {
    /// Set the shutdown flag, wake everyone, and join the workers. The
    /// workers drain pending tasks before exiting (the probe always calls
    /// disable after its requests joined, so the drain is empty).
    fn shutdown_and_join(self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.wake.notify_all();
        for h in self.workers {
            let _ = h.join();
        }
    }
}

impl SearchPool {
    const fn new() -> Self {
        Self {
            runtime: Mutex::new(None),
        }
    }

    /// Whether the pool path is live (the store's search/decode sites
    /// branch on this; false keeps the 11C semaphore path).
    pub fn enabled(&self) -> bool {
        self.runtime
            .lock()
            .expect("pool runtime poisoned")
            .is_some()
    }

    /// Enable the pool with `workers` threads and a backpressure capacity
    /// of `queue_factor x workers`. Shuts down any previous instance
    /// first (the probe reconfigures between runs).
    pub fn enable(&self, workers: usize, queue_factor: usize) {
        assert!(workers >= 1, "pool needs at least one worker");
        let mut rt = self.runtime.lock().expect("pool runtime poisoned");
        if let Some(old) = rt.take() {
            old.shutdown_and_join();
        }
        let shared = Arc::new(PoolShared::new(workers, queue_factor));
        let mut handles = Vec::with_capacity(workers);
        for w in 0..workers {
            let s = Arc::clone(&shared);
            handles.push(
                std::thread::Builder::new()
                    .name("entropyfs-pool".into())
                    .spawn(move || pool_worker_main(s, w))
                    .expect("spawn pool worker"),
            );
        }
        *rt = Some(PoolRuntime {
            shared,
            workers: handles,
        });
    }

    /// Shut the pool down (probe teardown / between-run reconfiguration).
    pub fn disable(&self) {
        let mut rt = self.runtime.lock().expect("pool runtime poisoned");
        if let Some(old) = rt.take() {
            old.shutdown_and_join();
        }
    }

    /// Bind the pool to a store (Weak: the pool never keeps it alive; the
    /// tasks' own `Arc` clones do). Must be called while enabled.
    pub fn bind(&self, store: &Arc<crate::store::Store>) {
        let rt = self.runtime.lock().expect("pool runtime poisoned");
        let rt = rt.as_ref().expect("pool must be enabled before bind");
        *rt.shared.store.lock().expect("pool store poisoned") = Some(Arc::downgrade(store));
    }

    /// The bound store as an `Arc` clone (per-request; cheap).
    /// `None` when unbound or the store was dropped (probe invariant:
    /// bind before the sweep, hold the store through it).
    pub fn store_arc(&self) -> Option<Arc<crate::store::Store>> {
        let rt = self.runtime.lock().expect("pool runtime poisoned");
        let rt = rt.as_ref()?;
        let g = rt.shared.store.lock().ok()?;
        g.as_ref()?.upgrade()
    }

    /// Allocate a monotonic request id (task stamping).
    pub fn alloc_request_id(&self) -> u64 {
        let rt = self.runtime.lock().expect("pool runtime poisoned");
        rt.as_ref()
            .expect("pool must be enabled to allocate request ids")
            .shared
            .next_request_id
            .fetch_add(1, Ordering::Relaxed)
    }

    /// The pool's `submit` is store-internal (the task types are
    /// `pub(crate)`); external callers use the store API.
    pub(crate) fn submit(&self, request_id: u64, tasks: Vec<WorkerTask>) -> PoolSubmit {
        let rt = self.runtime.lock().expect("pool runtime poisoned");
        let rt = rt.as_ref().expect("pool must be enabled before submit");
        rt.shared.submit(request_id, tasks)
    }

    /// The probe's fairness diagnostics.
    pub fn diagnostics(&self) -> PoolDiagnostics {
        let rt = self.runtime.lock().expect("pool runtime poisoned");
        match rt.as_ref() {
            Some(rt) => PoolDiagnostics {
                peak_in_flight: rt.shared.peak_in_flight.load(Ordering::Relaxed),
                capacity: rt.shared.capacity,
                workers: rt.workers.len(),
                max_consecutive_same_request: rt.shared.max_consecutive.load(Ordering::Relaxed),
            },
            None => PoolDiagnostics {
                peak_in_flight: 0,
                capacity: 0,
                workers: 0,
                max_consecutive_same_request: 0,
            },
        }
    }

    /// Phase 12C-1-2: the pool's LIVE pressure — the fraction of the
    /// backpressure capacity currently consumed (`in_flight / capacity`,
    /// clamped to [0, 1], 0 when the pool is disabled).
    ///
    /// This is the storage engine's own queue-pressure signal for the
    /// foreground deferral gate: a deep in-flight set means the pool is
    /// saturated, so adding MORE expensive search work now would only
    /// deepen the queue and stretch every write's latency — the 12C-1-2
    /// policy defers such work to the background optimizer instead. The
    /// read is one lock-free atomic load + a division; the runtime mutex
    /// is held only to find the shared state (the same acquisition the
    /// other pool accessors take).
    ///
    /// The brief's full scalar is `max(worker_utilization,
    /// normalized_queue_depth, normalized_queue_wait)`; this first
    /// implementation uses the queue-depth term (`in_flight / capacity`),
    /// which the pool already accounts exactly and lock-free. The
    /// queue-wait EWMA is the documented follow-on refinement.
    pub fn pressure(&self) -> f64 {
        let rt = self.runtime.lock().expect("pool runtime poisoned");
        match rt.as_ref() {
            Some(rt) => {
                let inflight = rt.shared.in_flight.load(Ordering::Relaxed);
                let cap = rt.shared.capacity.max(1);
                (inflight as f64 / cap as f64).min(1.0)
            }
            None => 0.0,
        }
    }
}

/// The process-wide pool (Phase-11E probe; disabled by default).
pub static POOL: SearchPool = SearchPool::new();

/// One worker's main loop: pick (round-robin, one task per pick), execute
/// outside every lock, deliver, repeat. Parks only when the active ring is
/// empty; exits when the pool shuts down AND the ring drained.
///
/// `worker_index` seeds the worker's private ring cursor: worker *i* starts
/// at ring position *i*, so the first wave of picks lands one task per
/// active request (A0 B0 C0 D0 …) instead of all workers stampeding the
/// first request. The probe found the SHARED-cursor alternative silently
/// pins each request to one worker when `workers == active_requests`: the
/// cursor wraps every W picks, so a worker's consecutive picks land on the
/// same ring position (max-consecutive grew to W; the round-robin had
/// degenerated into request-level batching). The per-worker cursor advances
/// by ONE per pick, so a worker's consecutive picks are always different
/// requests while >= 2 are active.
fn pool_worker_main(shared: Arc<PoolShared>, worker_index: usize) {
    let mut cursor = worker_index;
    let mut last_request: Option<u64> = None;
    let mut consecutive = 0usize;
    loop {
        let picked: Option<(WorkerTask, Arc<RequestState>)> = {
            let mut q = shared.queues.lock().expect("pool queues poisoned");
            loop {
                if shared.shutdown.load(Ordering::Acquire) && q.active.is_empty() {
                    return;
                }
                if let Some((task, state)) = pick_one(
                    &mut q,
                    &mut cursor,
                    &shared,
                    &mut last_request,
                    &mut consecutive,
                ) {
                    break Some((task, state));
                }
                q = shared.wake.wait(q).expect("pool queues poisoned");
            }
        };
        let (task, state) = picked.expect("picked a task");
        let (ordinal, cpu, result) = task.execute();
        state.deliver(ordinal, result, cpu);
        if state.done.load(Ordering::Acquire) == state.total {
            // Last delivery: drop the request from the map (its ring slot
            // was already removed when the pending queue drained).
            let mut q = shared.queues.lock().expect("pool queues poisoned");
            q.requests.remove(&state.id);
            drop(q);
        }
        // Backpressure accounting: one task left the in-flight set. If this
        // freed the last slot, wake a blocked submitter.
        if shared.in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
            shared.wake.notify_all();
        }
    }
}

/// Pick ONE task for a worker: round-robin over the active requests, one
/// task per pick (a request cannot monopolize a worker while another
/// request is active). Removes a request from the ring when its queue
/// drains. Must hold the queues lock.
///
/// `cursor` is the worker's PRIVATE ring position (seeded with its worker
/// index; advanced by one per pick): consecutive picks by one worker land
/// on different requests while >= 2 are active — the probe-found shared
/// cursor silently pinned each request to one worker (see
/// [`pool_worker_main`]).
fn pick_one(
    q: &mut PoolQueues,
    cursor: &mut usize,
    shared: &PoolShared,
    last_request: &mut Option<u64>,
    consecutive: &mut usize,
) -> Option<(WorkerTask, Arc<RequestState>)> {
    if q.active.is_empty() {
        return None;
    }
    let idx = *cursor % q.active.len();
    let rid = q.active[idx];
    *cursor = cursor.wrapping_add(1);
    let entry = q
        .requests
        .get_mut(&rid)
        .expect("active request id must be in the request map");
    match entry.pending.pop_front() {
        Some(task) => {
            // Fairness witness: consecutive picks of the SAME request by
            // this worker (1 whenever >= 2 requests are active).
            if *last_request == Some(rid) {
                *consecutive += 1;
            } else {
                *consecutive = 1;
            }
            *last_request = Some(rid);
            shared
                .max_consecutive
                .fetch_max(*consecutive, Ordering::Relaxed);
            entry.state.mark_first_service();
            if entry.pending.is_empty() {
                q.active.remove(idx);
                if *cursor > idx {
                    *cursor -= 1;
                }
            }
            Some((task, Arc::clone(&entry.state)))
        }
        None => {
            // Defensive: the ring slot drained concurrently (another
            // worker took the last task). Drop it and retry.
            q.active.remove(idx);
            if *cursor > idx {
                *cursor -= 1;
            }
            None
        }
    }
}

#[cfg(test)]
/// The worker-pool unit tests (the global probe POOL is shared and
/// serialized — see the module doc).
pub mod tests {
    use super::*;

    /// Serializes tests that configure the GLOBAL probe `POOL` (the
    /// mechanism test here and the gate probe in
    /// `src/tests/worker_pool_probe` — cargo runs tests in parallel, and
    /// the pool is process-global).
    pub static POOL_LOCK: Mutex<()> = Mutex::new(());

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

    #[test]
    fn pool_reassembles_ordinals_and_honors_backpressure() {
        // The pool's determinism contract: workers may execute tasks in any
        // order, but `join` returns them strictly by ordinal, and the
        // in-flight bound (backpressure) is never exceeded. Exercises the
        // machinery with trivial ZERO decodes (pure CPU, no store I/O) so
        // the test is deterministic and fast.
        let _guard = POOL_LOCK.lock().expect("pool test lock poisoned");
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(
            crate::store::Store::create(dir.path(), &Default::default(), [0x11; 16]).unwrap(),
        );
        POOL.enable(4, 8);
        POOL.bind(&store);
        let objects = Arc::new(HashMap::new());
        let descriptors = Arc::new(HashMap::new());
        let limits = *store.limits();
        let mut submits = Vec::new();
        for _ in 0..3u64 {
            let rid = POOL.alloc_request_id();
            let tasks = (0..5usize)
                .map(|i| WorkerTask::DecodeExtent {
                    ordinal: i,
                    store: Arc::clone(&store),
                    start: (i as u64) * 64,
                    desc: crate::core::representation::Representation::Zero { len: 64 },
                    objects: Arc::clone(&objects),
                    descriptors: Arc::clone(&descriptors),
                    limits,
                })
                .collect();
            submits.push(POOL.submit(rid, tasks));
        }
        for (r, s) in submits.into_iter().enumerate() {
            let (results, m) = s.join();
            assert_eq!(results.len(), 5, "request {r}: all ordinals present");
            for (i, wr) in results.iter().enumerate() {
                assert_eq!(wr.ordinal, i, "request {r}: ordinal order preserved");
                match &wr.result {
                    Ok(WorkerOutcome::Decode((start, chunk))) => {
                        assert_eq!(*start, (i as u64) * 64);
                        assert_eq!(chunk.len(), 64);
                        assert!(chunk.iter().all(|&b| b == 0));
                    }
                    other => panic!("request {r}: unexpected outcome {other:?}"),
                }
            }
            assert_eq!(m.tasks, 5);
            assert!(m.span_ns > 0);
        }
        let d = POOL.diagnostics();
        assert!(
            d.peak_in_flight <= d.capacity,
            "backpressure bound held (peak {} <= capacity {})",
            d.peak_in_flight,
            d.capacity
        );
        assert_eq!(d.workers, 4);

        // Regression (probe-found): an OVERSIZED request must be admitted
        // by an IDLE pool — `in_flight` is 0 and can never drop further, so
        // a strict `in_flight + total > capacity` wait would deadlock (the
        // 64-extent read-back decode vs pool-4's capacity 32). Admit 40
        // tasks against capacity 32 at idle.
        let rid = POOL.alloc_request_id();
        let tasks = (0..40usize)
            .map(|i| WorkerTask::DecodeExtent {
                ordinal: i,
                store: Arc::clone(&store),
                start: 0,
                desc: crate::core::representation::Representation::Zero { len: 16 },
                objects: Arc::clone(&objects),
                descriptors: Arc::clone(&descriptors),
                limits,
            })
            .collect();
        let (results, _) = POOL.submit(rid, tasks).join();
        assert_eq!(results.len(), 40);
        for (i, wr) in results.iter().enumerate() {
            assert_eq!(wr.ordinal, i);
        }

        POOL.disable();
        assert!(!POOL.enabled());
    }

    #[test]
    fn pool_pressure_reflects_in_flight_work() {
        // Phase 12C-1-2: the pool's pressure scalar (in_flight/capacity)
        // is the foreground deferral gate's live signal — it must rise
        // with in-flight work, normalize to [0, 1], and read 0 when the
        // pool is idle. Also verifies the store's pressure wiring: the
        // override wins, else the pool's signal when the store uses the
        // pool, else 0.
        let _guard = POOL_LOCK.lock().expect("pool test lock poisoned");
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(
            crate::store::Store::create(dir.path(), &Default::default(), [0x22; 16]).unwrap(),
        );
        POOL.enable(2, 8);
        POOL.bind(&store);
        assert_eq!(POOL.pressure(), 0.0, "idle pool has zero pressure");

        // The store wiring: without the pool flag, pressure is 0; with
        // the flag, it reads the pool; the override always wins.
        assert_eq!(store.foreground_pressure(), 0.0);
        store.enable_worker_pool();
        assert_eq!(store.foreground_pressure(), POOL.pressure());
        store.set_pressure_override(Some(0.5));
        assert_eq!(store.foreground_pressure(), 0.5);
        store.set_pressure_override(None);
        assert_eq!(store.foreground_pressure(), POOL.pressure());
        store.set_pressure_override(Some(1.5)); // clamped
        assert_eq!(store.foreground_pressure(), 1.0);
        store.set_pressure_override(None);

        // 16 medium decodes (64 KiB RAW) against 2 workers: the drain
        // takes long enough that in-flight work is observable. The read
        // is a bounded spin (a slow CI machine cannot miss a >0 window
        // entirely) — an observation, never a timing gate.
        let payload = vec![0x5au8; 65536];
        let cid = crate::core::extent::ChunkId::of(&payload);
        let mut objects = HashMap::new();
        objects.insert(cid, payload);
        let objects = Arc::new(objects);
        let descriptors = Arc::new(HashMap::new());
        let limits = *store.limits();
        let rid = POOL.alloc_request_id();
        let tasks = (0..16usize)
            .map(|i| WorkerTask::DecodeExtent {
                ordinal: i,
                store: Arc::clone(&store),
                start: 0,
                desc: crate::core::representation::Representation::Raw {
                    obj: cid,
                    len: 65536,
                },
                objects: Arc::clone(&objects),
                descriptors: Arc::clone(&descriptors),
                limits,
            })
            .collect();
        let submit = POOL.submit(rid, tasks);
        let mut observed = 0.0f64;
        for _ in 0..100_000 {
            let p = POOL.pressure();
            assert!(p <= 1.0, "pressure is normalized to [0, 1]");
            observed = observed.max(p);
            if p > 0.0 {
                break;
            }
            std::thread::yield_now();
        }
        let (results, m) = submit.join();
        assert_eq!(results.len(), 16);
        assert_eq!(m.tasks, 16);
        assert!(observed > 0.0, "pressure rose with in-flight work");

        POOL.disable();
        assert_eq!(POOL.pressure(), 0.0, "disabled pool has zero pressure");
    }
}
