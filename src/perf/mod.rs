//! Phase-10A/11B performance instrumentation (diagnostic, not sealed
//! evidence).
//!
//! Three collectors:
//!
//! - [`Timings`]: named write-path phase timers (read-modify-write,
//!   hashing, candidate search by family, §32 validation, commit-lock
//!   wait, B-tree mutation, transaction pruning, segment append/flush,
//!   superblock publication). Each phase keeps a bounded sample ring so
//!   p50/p95/p99 can be reported alongside the cumulative total — the
//!   court's answer to "where does every millisecond go".
//! - [`FuseStats`]: per-opcode request counters with latency percentiles,
//!   a write-request size histogram (does the kernel really deliver 1 MiB
//!   writes?), and the maximum request concurrency observed.
//!
//! Phase 11B adds the **request reconciliation**: every request opens an
//! envelope ([`Timings::request`]) and the store partitions its body into
//! exclusive leaf phases ([`Timings::time_request`]). The identity
//!
//! ```text
//! request latency = Σ exclusive phases + residual
//! ```
//!
//! is the performance equivalent of Phase 9H's physical byte
//! reconciliation: [`Timings::reconcile`] aggregates the closed requests
//! and [`Timings::render_reconciled`] renders the stacked accounting table
//! with an explicit residual (FUSE/scheduler/other) and an overlap flag
//! when a phase was nested inside another (a double-counted partition).
//!
//! All collectors are cheap (one `Instant` pair + one mutex push per
//! sample) and strictly diagnostic: they never affect correctness or
//! persistence.
//!
//! # PURPOSE
//!
//! Instrumentation is EVIDENCE, not decoration: the 11B/11C/11D courts
//! assert their claims from these numbers (`docs/performance/
//! reconciliation.md`, `docs/performance/worker-oracle.md`), so the units
//! discipline below is part of the contract. The collectors answer "where
//! does every request microsecond go" (11B), "which lever moved which
//! term" (11C), and "is `prepare` CPU or queue" (11D).
//!
//! # BOUNDARY
//!
//! Diagnostic-only: no collector affects correctness, persistence, or
//! data flow; samples are advisory. The module knows phase names and
//! durations only — never store state — and must never be used as a
//! synchronization or ordering mechanism.
//!
//! # MODEL — THE UNITS DISCIPLINE
//!
//! Every sample is exactly one of four unit classes, and the classes are
//! NOT interchangeable:
//!
//! - **wall time** (monotonic `Instant`): the phase table, the request
//!   envelopes (`RequestResult::total_ns`, `RequestAcc::t0.elapsed()`),
//!   and the `FuseStats` op latencies. Wall is the only unit that
//!   partitions a request's real time.
//! - **CPU sum**: `worker_useful_cpu` — per-worker `CLOCK_THREAD_CPUTIME`
//!   (wall fallback where the kernel lacks it), summed across parallel
//!   workers. It is recorded through the SAME global phase table
//!   ([`Timings::record`]) but is NOT a wall slice: it may exceed 100%
//!   of the enclosing wall row, it is never attached to a request
//!   envelope, and the wall reconciliation excludes it by design (Phase
//!   11D Gate C, `docs/performance/worker-oracle.md`; the thread-CPU vs
//!   wall distinction lives at the clock site in `src/store/workers.rs`
//!   and must be preserved there).
//! - **per-request**: envelope rows ([`Timings::time_request`]) are
//!   exclusive leaf partitions of ONE request.
//! - **cumulative**: `Phase::nanos_total` and the reconciled
//!   [`Reconciliation`] are sums over all samples / all closed requests.
//!
//! # THE REQUEST ENVELOPE (Phase 11B)
//!
//! The FUSE handler opens the envelope ([`Timings::request`]); the store
//! entry points re-open it for direct callers (benchmarks, recovery) and
//! nested re-opens are pass-throughs, so the total is the full request
//! including FUSE overhead while direct callers still get a
//! reconciliation. The exclusive partition rows of the FUSE write path
//! (`docs/performance/reconciliation.md` §1): `inode_lock_wait`,
//! `epoch_lock_wait`, `read_scan`/`read_deps`/`read_prefetch`/
//! `read_decode`, `prepare`, `stage`, `commit_lock_wait`, `append`,
//! `flush`, `epoch_wait`, and the checkpoint's `cp_*` rows; the fsync
//! path partitions into `barrier_*` plus `cp_*` rows. A row must never
//! wrap a call that itself emits rows; internal helper reads run inside
//! [`Timings::detach`].
//!
//! # CORRECTNESS INVARIANTS
//!
//! - The reconciliation identity `request latency == Σ phases + residual`
//!   holds per request and in aggregate; the courts assert it at every
//!   thread count.
//! - A negative residual (or the aggregate `overlap` flag) is an
//!   INSTRUMENTATION BUG — a nested row — not a runtime condition; the
//!   courts fail on it.
//! - CPU-sum rows never join the envelope partition; only wall segments
//!   partition a request.
//!
//! # CONCURRENCY
//!
//! Three independent mutexes (`phases`, `requests`, `completed`) plus
//! atomics; they are never held simultaneously (`RequestGuard::drop`
//! releases `requests` before taking `completed`), so there is no lock
//! order to deadlock. `CURRENT_REQUEST`/`DETACH_DEPTH` are thread-local:
//! envelopes are per-thread, which is exactly right because the store's
//! request work is single-threaded per request.
//!
//! # DURABILITY
//!
//! None. The collectors hold no persistence authority; the daemon's
//! `--stats-file` dump archives the rendered tables for the courts.
//!
//! # RESOURCE BOUNDS
//!
//! `MAX_SAMPLES` (4096) caps each phase's and each opcode's latency ring,
//! bounding memory and the percentile sort. The `completed` ledger grows
//! with closed requests until [`Timings::clear`] (per-run isolation). One
//! mutex push per sample; one `Instant` pair per timed region.
//!
//! # PERFORMANCE
//!
//! Cheap by design so the courts can run it under load without perturbing
//! the measurement (the 11B/11C residual floors — ≤ 4% / ≤ 3.2% — are
//! evidence that the overhead is small).
//!
//! # FAILURE MODES
//!
//! - Poisoned mutexes panic (deliberate: a broken timer must not silently
//!   corrupt evidence).
//! - Negative residual / `overlap`: a caller wrapped a row-emitting call
//!   inside [`Timings::time_request`] instead of [`Timings::detach`].
//! - Must never happen: instrumentation changing data flow, or a CPU-sum
//!   row treated as a wall partition.
//!
//! # HISTORY / EVIDENCE
//!
//! - Phase 10A introduced the phase table; 11B the request envelope.
//! - `evidence/performance/recon-court-1787757073-e5b0592/` — sealed 11B
//!   mounted court: identity holds (no overlap, residual ≤ 4.0%) at
//!   1/2/4/8/16 threads; found the epoch-mutex convoy behind the write
//!   plateau.
//! - `evidence/performance/recon-court-1787762195-49f1a55/` — sealed 11C
//!   court: identity holds (residual ≤ 3.2%), epoch locks 4.3% → 0.2% at
//!   16 threads, `read_decode` 1.6% → 0.7%, `commit_lock_wait` 34.7% →
//!   16.4%.
//! - `evidence/performance/worker-oracle-1787765041-052bc46/` — sealed
//!   11D oracle: `prepare` decomposes into `worker_queue_wait` (Gate A) +
//!   `worker_scope_wall` (Gate B) + `worker_useful_cpu` (Gate C); search
//!   CPU constant 9.8–10.0 s at every thread count, queue wait 4.6% →
//!   91.7% of `prepare`; identity holds (residual ≤ 0.9%).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// Sample-ring bound per phase and per opcode (keeps the memory bounded
/// and the percentile sort cheap: 4096 samples per ring).
const MAX_SAMPLES: usize = 4096;

// The request this thread is currently inside (if any).
//
// Phase 11B: exclusive phases recorded while a thread is inside a request
// attach to that request's envelope, so the reconciliation can be computed
// per request even though the phase timers are scattered across the store.
// Set on envelope open, cleared on guard drop (the drop is the close /
// linearization point). Per-thread by design: one request, one thread.
thread_local! {
    static CURRENT_REQUEST: std::cell::Cell<Option<u64>> = const {
        std::cell::Cell::new(None)
    };
    /// Nesting depth of [`Timings::detach`]: while > 0, exclusive phases
    /// record globally but do NOT attach to the request (an internal helper
    /// read inside a `prepare` row is preparation work, not a top-level
    /// read).
    static DETACH_DEPTH: std::cell::Cell<u32> = const {
        std::cell::Cell::new(0)
    };
}

/// One timed phase: cumulative wall nanoseconds plus a bounded per-sample
/// latency ring for percentiles.
///
/// # Units
///
/// - `nanos_total`: cumulative WALL nanoseconds over ALL samples (u128;
///   the phase table's cumulative class).
/// - `samples`: per-sample WALL nanoseconds, FIFO-bounded at
///   [`MAX_SAMPLES`] entries (ring eviction; the percentile class).
/// - `count`: number of samples (samples may exceed the ring size).
///
/// Invariant: `nanos_total` is the sum of every recorded sample (not
/// just the ring's).
#[derive(Debug, Default)]
struct Phase {
    /// Cumulative wall nanoseconds (all samples).
    nanos_total: u128,
    /// Number of samples.
    count: u64,
    /// Bounded wall-nanosecond latency ring (percentiles).
    samples: Vec<u64>,
}

/// Named write-path phase timings.
///
/// # Role
///
/// The global phase table (named phases keyed by `&'static str`,
/// cumulative + percentiles), plus the Phase-11B request ledger: in-flight
/// envelopes, closed results, and the monotonic envelope id counter.
///
/// # Invariants
///
/// - Envelope ids come from `next_request` (Relaxed is fine: ids only
///   need uniqueness, never ordering across threads).
/// - The three mutexes are never held simultaneously (see module doc).
/// - Envelopes are opened and closed on the SAME thread (the ledger is
///   keyed by id but the thread-local enforces single-thread ownership).
#[derive(Debug, Default)]
pub struct Timings {
    phases: Mutex<HashMap<&'static str, Phase>>,
    /// In-flight request envelopes (keyed by id; closed by [`RequestGuard`]
    /// drop).
    requests: Mutex<HashMap<u64, RequestAcc>>,
    /// Closed requests, in close order.
    completed: Mutex<Vec<RequestResult>>,
    /// Monotonic request id.
    next_request: std::sync::atomic::AtomicU64,
}

/// One in-flight request's exclusive-phase accumulator (Phase 11B).
///
/// # Units
///
/// `t0` is the WALL start `Instant`; `phases` maps exclusive phase name →
/// accumulated WALL nanoseconds inside this request only (the
/// per-request class).
#[derive(Debug)]
struct RequestAcc {
    name: &'static str,
    t0: Instant,
    phases: HashMap<&'static str, u64>,
}

/// A closed request: total latency, exclusive phase durations, residual.
///
/// # Units
///
/// - `total_ns`: WALL nanoseconds (`t0.elapsed()` at close).
/// - `phases`: exclusive phase durations in WALL nanoseconds, sorted
///   descending (the per-request partition rows).
/// - `residual_ns`: `total − Σ phases` as i128 — negative means a phase
///   was nested inside another partition row (double counting, an
///   instrumentation bug the courts fail on).
#[derive(Debug, Clone)]
pub struct RequestResult {
    /// The request name.
    pub name: &'static str,
    /// Total wall latency of the request (ns).
    pub total_ns: u64,
    /// Exclusive phase durations within the request (ns), descending.
    pub phases: Vec<(&'static str, u64)>,
    /// `total − Σ phases` (ns). Negative ⇒ a phase was nested inside
    /// another partition row (double counting).
    pub residual_ns: i128,
}

/// One reconciled phase row: the aggregate over all completed requests.
///
/// # Units
///
/// `total_ms` is WALL milliseconds (phase ns / 1e6) summed across the
/// closed requests; `share` is that phase's fraction of the requests'
/// total wall latency (0..=1, computed against `Reconciliation::total_ms`
/// — a CPU-sum row must never appear here).
#[derive(Debug, Clone)]
pub struct ReconRow {
    /// Phase name.
    pub phase: &'static str,
    /// Aggregate milliseconds.
    pub total_ms: f64,
    /// Share of the requests' total latency (0..=1).
    pub share: f64,
}

/// The stacked accounting: `Σ request totals == Σ phases + residual`.
///
/// # Units / semantics
///
/// - `requests`: number of closed requests aggregated.
/// - `total_ms` / `residual_ms`: WALL milliseconds (cumulative class).
/// - `overlap == residual < 0`: the partition double-counted (nested
///   rows) — an instrumentation bug, asserted false by the courts.
#[derive(Debug, Clone)]
pub struct Reconciliation {
    /// Number of closed requests aggregated.
    pub requests: u64,
    /// Aggregate request wall time (ms).
    pub total_ms: f64,
    /// Phase rows, descending by total.
    pub rows: Vec<ReconRow>,
    /// Unaccounted time (ms): `total − Σ phases`.
    pub residual_ms: f64,
    /// Residual share of total (0..=1).
    pub residual_share: f64,
    /// `residual < 0`: the partition double-counted (nested rows).
    pub overlap: bool,
}

/// The request envelope guard. Dropping it closes the request, computes
/// the residual, and appends the result to [`Timings`]'s completed set.
///
/// # Pass-through nesting
///
/// `id == None` means this thread was already inside a request, so the
/// guard is a pass-through and its drop closes nothing — the inner
/// phases attach to the OUTER envelope (the FUSE handler opens the
/// envelope; the store entry points re-open it for direct callers).
///
/// # Linearization
///
/// The drop is the close point: `CURRENT_REQUEST` is cleared, the
/// envelope is removed, and `total_ns` is read from `t0.elapsed()` at
/// that instant (wall).
pub struct RequestGuard<'a> {
    timings: &'a Timings,
    /// `None`: this thread was already inside a request, so the guard is a
    /// pass-through and its drop closes nothing (the inner phases attach
    /// to the OUTER envelope: the FUSE handler opens the envelope and the
    /// store entry points re-open it for direct callers).
    id: Option<u64>,
}

impl Drop for RequestGuard<'_> {
    fn drop(&mut self) {
        let Some(id) = self.id else {
            return;
        };
        CURRENT_REQUEST.set(None);
        let mut m = self.timings.requests.lock().expect("requests poisoned");
        let acc = m.remove(&id).expect("request envelope must exist");
        let mut phases: Vec<(&'static str, u64)> = acc.phases.into_iter().collect();
        phases.sort_by(|a, b| b.1.cmp(&a.1));
        let total_ns = acc.t0.elapsed().as_nanos() as u64;
        let sum: u64 = phases.iter().map(|(_, ns)| *ns).sum();
        drop(m);
        self.timings
            .completed
            .lock()
            .expect("completed poisoned")
            .push(RequestResult {
                name: acc.name,
                total_ns,
                phases,
                residual_ns: total_ns as i128 - sum as i128,
            });
    }
}

/// A timing snapshot row.
///
/// # Units
///
/// `count` = samples; `total_ms` = cumulative WALL milliseconds over all
/// samples (ns / 1e6); `p50_us`/`p95_us`/`p99_us` = percentiles of the
/// bounded sample ring in microseconds (ns / 1e3).
#[derive(Debug, Clone, Copy)]
pub struct TimingRow {
    /// Phase name.
    pub phase: &'static str,
    /// Sample count.
    pub count: u64,
    /// Cumulative milliseconds.
    pub total_ms: f64,
    /// Median (p50) microseconds.
    pub p50_us: f64,
    /// p95 microseconds.
    pub p95_us: f64,
    /// p99 microseconds.
    pub p99_us: f64,
}

/// Nearest-rank percentile of a SORTED sample slice (µs-scale helper).
///
/// Empty input yields 0.0. The caller sorts `sorted`; `q` is the
/// quantile in 0..=1.
fn percentile(sorted: &[u64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[idx] as f64
}

impl Timings {
    /// Record one sample for a phase.
    ///
    /// `nanos` is WALL nanoseconds (monotonic); the caller is responsible
    /// for the unit. Cumulative total (all samples) and the bounded ring
    /// (percentiles) are both updated. CPU-sum rows like `worker_useful_cpu`
    /// ALSO flow through here — recording through [`Self::record`] is what
    /// keeps them OUT of request envelopes; the reconciliation must never
    /// treat them as wall partitions (module doc, units discipline).
    pub fn record(&self, phase: &'static str, nanos: u64) {
        let mut m = self.phases.lock().expect("timings poisoned");
        let p = m.entry(phase).or_default();
        p.nanos_total += nanos as u128;
        p.count += 1;
        if p.samples.len() >= MAX_SAMPLES {
            p.samples.remove(0);
        }
        p.samples.push(nanos);
    }

    /// Time a closure under a phase name (WALL nanoseconds, global table
    /// only — no request attachment).
    pub fn time<T>(&self, phase: &'static str, f: impl FnOnce() -> T) -> T {
        let t = Instant::now();
        let out = f();
        self.record(phase, t.elapsed().as_nanos() as u64);
        out
    }

    /// Snapshot all phases as sorted rows (by cumulative total, descending).
    ///
    /// # Units
    ///
    /// `total_ms` is cumulative WALL ms (ns / 1e6); the percentiles are
    /// derived from the bounded sample ring (µs). Rows sort by cumulative
    /// total so the biggest bucket is on top. This is the view the 11D
    /// oracle reads `worker_queue_wait` / `worker_scope_wall` /
    /// `worker_useful_cpu` from.
    pub fn snapshot(&self) -> Vec<TimingRow> {
        let m = self.phases.lock().expect("timings poisoned");
        let mut rows: Vec<TimingRow> = m
            .iter()
            .map(|(name, p)| {
                let mut s = p.samples.clone();
                s.sort_unstable();
                TimingRow {
                    phase: name,
                    count: p.count,
                    total_ms: p.nanos_total as f64 / 1e6,
                    p50_us: percentile(&s, 0.50) / 1e3,
                    p95_us: percentile(&s, 0.95) / 1e3,
                    p99_us: percentile(&s, 0.99) / 1e3,
                }
            })
            .collect();
        rows.sort_by(|a, b| b.total_ms.total_cmp(&a.total_ms));
        rows
    }

    /// Render the phase table (cumulative + percentiles).
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("phase timings (cumulative ms, per-sample us p50/p95/p99):\n");
        for r in self.snapshot() {
            out.push_str(&format!(
                "  {:<28} n={:>8} total={:>10.2} ms   p50={:>9.1} p95={:>9.1} p99={:>9.1} us\n",
                r.phase, r.count, r.total_ms, r.p50_us, r.p95_us, r.p99_us
            ));
        }
        out
    }

    // -------------------------------------------------------------------
    // Phase-11B request reconciliation
    //
    // The envelope + exclusive partition rows + the identity
    // `request latency == Σ phases + residual` (module doc).
    // -------------------------------------------------------------------

    /// Open a request envelope (Phase 11B). If this thread is already
    /// inside a request, the returned guard is a pass-through: the inner
    /// exclusive phases attach to the OUTER envelope. The FUSE handler
    /// opens the envelope; the store entry points re-open it so direct
    /// callers (benchmarks, recovery) get an envelope without one.
    ///
    /// # Units / linearization
    ///
    /// The envelope's `t0` is a WALL `Instant` taken here; the total is
    /// read at guard drop (the close point). The id is allocated from the
    /// monotonic `next_request` counter.
    pub fn request(&self, name: &'static str) -> RequestGuard<'_> {
        if CURRENT_REQUEST.get().is_some() {
            return RequestGuard {
                timings: self,
                id: None,
            };
        }
        let id = self
            .next_request
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.requests.lock().expect("requests poisoned").insert(
            id,
            RequestAcc {
                name,
                t0: Instant::now(),
                phases: HashMap::new(),
            },
        );
        CURRENT_REQUEST.set(Some(id));
        RequestGuard {
            timings: self,
            id: Some(id),
        }
    }
    /// An exclusive partition phase inside the current request (Phase 11B):
    /// timed like [`Timings::time`] AND attached to the thread's request
    /// envelope. Without an active request it degrades to the global phase
    /// table. Callers must use it on NON-OVERLAPPING leaf blocks only — a
    /// phase nested inside another partition row double-counts and the
    /// reconciliation flags the overlap. Inside a [`Timings::detach`]
    /// scope the phase records globally only.
    ///
    /// # Units / identity
    ///
    /// `ns` is WALL nanoseconds of the closure; it is added to the global
    /// phase's cumulative total AND to the enclosing envelope's partition
    /// (per-request class). The envelope identity
    /// `total == Σ phases + residual` requires every row here to be a
    /// disjoint leaf — never wrap another row-emitting call.
    pub fn time_request<T>(&self, phase: &'static str, f: impl FnOnce() -> T) -> T {
        let t = Instant::now();
        let out = f();
        let ns = t.elapsed().as_nanos() as u64;
        self.record(phase, ns);
        if DETACH_DEPTH.get() == 0 {
            if let Some(id) = CURRENT_REQUEST.get() {
                let mut m = self.requests.lock().expect("requests poisoned");
                if let Some(acc) = m.get_mut(&id) {
                    *acc.phases.entry(phase).or_default() += ns;
                }
            }
        }
        out
    }

    /// Run `f` with request attachment DISABLED: exclusive phases inside
    /// still hit the global phase table but do not join the request's
    /// partition. Used for internal helper reads that are part of a larger
    /// partition row (the RMW read inside `prepare` is preparation work).
    ///
    /// # Why detach exists
    ///
    /// The envelope's rows must be EXCLUSIVE LEAF partitions; an internal
    /// helper read that runs inside `prepare` (or any row) is not a
    /// top-level phase and must not double-count. `DETACH_DEPTH` is a
    /// per-thread nesting counter, so detach scopes compose.
    pub fn detach<T>(&self, f: impl FnOnce() -> T) -> T {
        DETACH_DEPTH.set(DETACH_DEPTH.get() + 1);
        let out = f();
        DETACH_DEPTH.set(DETACH_DEPTH.get() - 1);
        out
    }

    /// The closed requests (per-request identity checks — the aggregate
    /// can hide a single overlapping request).
    ///
    /// Units: [`RequestResult`] fields, wall ns.
    pub fn results(&self) -> Vec<RequestResult> {
        self.completed.lock().expect("completed poisoned").clone()
    }

    /// Reset the global phases and the request ledger (per-run isolation in
    /// a long-lived process; the daemon dump must not mix sweep runs).
    ///
    /// Without this, the `completed` ledger and the cumulative totals grow
    /// without bound across runs; the courts call this between thread
    /// counts.
    pub fn clear(&self) {
        self.phases.lock().expect("timings poisoned").clear();
        self.requests.lock().expect("requests poisoned").clear();
        self.completed.lock().expect("completed poisoned").clear();
    }

    /// The stacked accounting: aggregate the closed requests into
    /// `total == Σ phases + residual`.
    ///
    /// # Semantics
    ///
    /// Wall classes only: `total` is the sum of the requests' wall
    /// totals, the rows sum the per-request exclusive wall phases, and
    /// `residual = total − Σ phases` (i128; negative ⇒ overlap). Rows
    /// sort descending by total. CPU-sum rows never reach here — they are
    /// never attached to envelopes.
    pub fn reconcile(&self) -> Reconciliation {
        let completed = self.completed.lock().expect("completed poisoned");
        let mut total: u128 = 0;
        let mut sums: HashMap<&'static str, u128> = HashMap::new();
        for r in completed.iter() {
            total += r.total_ns as u128;
            for (p, ns) in &r.phases {
                *sums.entry(p).or_default() += *ns as u128;
            }
        }
        let phase_sum: u128 = sums.values().sum();
        let residual = total as i128 - phase_sum as i128;
        let mut rows: Vec<ReconRow> = sums
            .iter()
            .map(|(p, ns)| ReconRow {
                phase: p,
                total_ms: *ns as f64 / 1e6,
                share: if total > 0 {
                    *ns as f64 / total as f64
                } else {
                    0.0
                },
            })
            .collect();
        rows.sort_by(|a, b| b.total_ms.total_cmp(&a.total_ms));
        Reconciliation {
            requests: completed.len() as u64,
            total_ms: total as f64 / 1e6,
            rows,
            residual_ms: residual as f64 / 1e6,
            residual_share: if total > 0 {
                residual as f64 / total as f64
            } else {
                0.0
            },
            overlap: residual < 0,
        }
    }

    /// Render the stacked accounting table with the reconciliation
    /// identity and its explicit residual.
    ///
    /// The `unaccounted` row is the residual (FUSE/scheduler/other) and
    /// the total row prints the identity verdict: `OK` when
    /// `Σ phases + residual == total`, `OVERLAP!` when a nested row
    /// double-counted. This table is part of the daemon's `--stats-file`
    /// dump, so every `court-threads*` run archives it.
    pub fn render_reconciled(&self) -> String {
        let r = self.reconcile();
        let mut out = String::new();
        out.push_str(&format!(
            "request reconciliation (n={} requests, {:.2} ms total):\n",
            r.requests, r.total_ms
        ));
        out.push_str(&format!(
            "  {:<26} {:>12} {:>9}\n",
            "phase", "total ms", "share"
        ));
        for row in &r.rows {
            out.push_str(&format!(
                "  {:<26} {:>12.2} {:>8.1}%\n",
                row.phase,
                row.total_ms,
                row.share * 100.0
            ));
        }
        out.push_str(&format!(
            "  {:<26} {:>12.2} {:>8.1}%   <- residual (fuse/scheduler/other)\n",
            "unaccounted",
            r.residual_ms,
            r.residual_share * 100.0
        ));
        out.push_str(&format!(
            "  {:<26} {:>12.2} {:>8.1}%   <- sum(phases) + residual == total {}\n",
            "total",
            r.total_ms,
            100.0,
            if r.overlap {
                "OVERLAP! (nested phase rows)"
            } else {
                "OK"
            }
        ));
        out
    }
}

// ---------------------------------------------------------------------------
// FUSE request statistics
// ---------------------------------------------------------------------------

/// Write-request size buckets (bytes): the histogram answers whether the
/// kernel writeback path really delivers 1 MiB requests.
pub const WRITE_BUCKETS: [&str; 7] = ["<4K", "4-16K", "16-64K", "64-256K", "256K-1M", "=1M", ">1M"];

/// Map a write request length (bytes) to its histogram bucket index
/// (`write_bucket(len)` ∈ 0..=6, aligning with [`WRITE_BUCKETS`]).
fn write_bucket(len: usize) -> usize {
    match len {
        0..=4095 => 0,
        4096..=16383 => 1,
        16384..=65535 => 2,
        65536..=262143 => 3,
        262144..=1048575 => 4,
        1048576 => 5,
        _ => 6,
    }
}

/// One opcode's statistics.
///
/// # Units
///
/// `count` = completed requests; `nanos_total` = cumulative WALL
/// nanoseconds; `samples` = bounded WALL-ns latency ring (percentiles).
#[derive(Debug, Default)]
struct OpStat {
    count: u64,
    nanos_total: u128,
    samples: Vec<u64>,
}

/// FUSE request statistics (per-opcode counts + latency percentiles, write
/// size histogram, max concurrency).
///
/// # Role
///
/// The mount-facing collector: what the kernel actually sent (opcode mix,
/// write request sizes, true parallelism). The histogram answers whether
/// writeback really delivers 1 MiB requests; `max_in_flight` answers the
/// concurrency question the write plateau raised.
///
/// # Units
///
/// Op latencies are WALL nanoseconds; `write_sizes` counts requests per
/// byte bucket ([`WRITE_BUCKETS`]); `in_flight`/`max_in_flight` are
/// concurrent-request counts. The atomics use Relaxed ordering — counters
/// only, no ordering constraints.
#[derive(Debug, Default)]
pub struct FuseStats {
    ops: Mutex<HashMap<&'static str, OpStat>>,
    /// Write-size histogram buckets (counts per byte bucket).
    write_sizes: Mutex<[u64; 7]>,
    /// Requests currently in flight (count).
    in_flight: std::sync::atomic::AtomicU64,
    /// Maximum observed concurrency (count).
    max_in_flight: std::sync::atomic::AtomicU64,
}

/// Guard that tracks request concurrency; drops when the handler returns.
pub struct InFlight<'a> {
    stats: &'a FuseStats,
}

impl<'a> InFlight<'a> {
    /// Begin a request: increments the in-flight counter and updates the
    /// observed maximum.
    ///
    /// # Concurrency
    ///
    /// Counters only; Relaxed atomics suffice (no ordering constraints).
    /// `now` is the post-increment value; `fetch_max` keeps the running
    /// maximum. The returned guard's drop decrements the counter.
    pub fn begin(stats: &'a FuseStats) -> Self {
        let now = stats
            .in_flight
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        stats
            .max_in_flight
            .fetch_max(now, std::sync::atomic::Ordering::Relaxed);
        Self { stats }
    }
}

impl Drop for InFlight<'_> {
    /// End a request: decrement the in-flight counter (the concurrency
    /// measurement; the maximum is never decremented).
    fn drop(&mut self) {
        self.stats
            .in_flight
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl FuseStats {
    /// Record one completed request.
    ///
    /// # Units
    ///
    /// `nanos` is the request's WALL latency in nanoseconds; the sample
    /// ring is bounded at [`MAX_SAMPLES`], the cumulative total is not.
    pub fn record_op(&self, op: &'static str, nanos: u64) {
        let mut m = self.ops.lock().expect("fuse stats poisoned");
        let s = m.entry(op).or_default();
        s.count += 1;
        s.nanos_total += nanos as u128;
        if s.samples.len() >= MAX_SAMPLES {
            s.samples.remove(0);
        }
        s.samples.push(nanos);
    }

    /// Record a write request's size (bytes) into the byte histogram.
    pub fn record_write_size(&self, len: usize) {
        let mut h = self.write_sizes.lock().expect("fuse stats poisoned");
        h[write_bucket(len)] += 1;
    }

    /// Maximum observed request concurrency (a count, not a time).
    pub fn max_concurrency(&self) -> u64 {
        self.max_in_flight
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Per-opcode rows: (op, count, total_ms, p50_us, p95_us, p99_us).
    ///
    /// # Units
    ///
    /// `count` = completed requests; `total_ms` = cumulative WALL ms
    /// (ns / 1e6); the percentiles come from the bounded WALL-ns ring in
    /// µs. Rows sort descending by cumulative total.
    pub fn snapshot(&self) -> Vec<(String, u64, f64, f64, f64, f64)> {
        let m = self.ops.lock().expect("fuse stats poisoned");
        let mut rows: Vec<(String, u64, f64, f64, f64, f64)> = m
            .iter()
            .map(|(name, s)| {
                let mut v = s.samples.clone();
                v.sort_unstable();
                (
                    name.to_string(),
                    s.count,
                    s.nanos_total as f64 / 1e6,
                    percentile(&v, 0.50) / 1e3,
                    percentile(&v, 0.95) / 1e3,
                    percentile(&v, 0.99) / 1e3,
                )
            })
            .collect();
        rows.sort_by(|a, b| b.2.total_cmp(&a.2));
        rows
    }

    /// Render the FUSE stats.
    ///
    /// Output: max concurrency, the per-opcode table (count, cumulative
    /// ms, p50/p95/p99 µs), and the write-request size histogram.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "fuse requests: max concurrency {}\n",
            self.max_concurrency()
        ));
        for (op, count, total_ms, p50, p95, p99) in self.snapshot() {
            out.push_str(&format!(
                "  {:<12} n={:>8} total={:>10.2} ms   p50={:>9.1} p95={:>9.1} p99={:>9.1} us\n",
                op, count, total_ms, p50, p95, p99
            ));
        }
        let h = self.write_sizes.lock().expect("fuse stats poisoned");
        out.push_str("write request size histogram:\n");
        for (i, label) in WRITE_BUCKETS.iter().enumerate() {
            out.push_str(&format!("  {label:>10}: {}\n", h[i]));
        }
        out
    }
}
