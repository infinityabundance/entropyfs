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

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// Sample-ring bound per phase (keeps the memory bounded).
const MAX_SAMPLES: usize = 4096;

// The request this thread is currently inside (if any).
//
// Phase 11B: exclusive phases recorded while a thread is inside a request
// attach to that request's envelope, so the reconciliation can be computed
// per request even though the phase timers are scattered across the store.
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

/// One timed phase.
#[derive(Debug, Default)]
struct Phase {
    /// Cumulative nanoseconds.
    nanos_total: u128,
    /// Number of samples.
    count: u64,
    /// Bounded latency ring (nanoseconds).
    samples: Vec<u64>,
}

/// Named write-path phase timings.
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

/// One in-flight request's exclusive-phase accumulator.
#[derive(Debug)]
struct RequestAcc {
    name: &'static str,
    t0: Instant,
    phases: HashMap<&'static str, u64>,
}

/// A closed request: total latency, exclusive phase durations, residual.
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

fn percentile(sorted: &[u64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[idx] as f64
}

impl Timings {
    /// Record one sample for a phase.
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

    /// Time a closure under a phase name.
    pub fn time<T>(&self, phase: &'static str, f: impl FnOnce() -> T) -> T {
        let t = Instant::now();
        let out = f();
        self.record(phase, t.elapsed().as_nanos() as u64);
        out
    }

    /// Snapshot all phases as sorted rows (by cumulative total, descending).
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
    // -------------------------------------------------------------------

    /// Open a request envelope (Phase 11B). If this thread is already
    /// inside a request, the returned guard is a pass-through: the inner
    /// exclusive phases attach to the OUTER envelope. The FUSE handler
    /// opens the envelope; the store entry points re-open it so direct
    /// callers (benchmarks, recovery) get an envelope without one.
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
    pub fn detach<T>(&self, f: impl FnOnce() -> T) -> T {
        DETACH_DEPTH.set(DETACH_DEPTH.get() + 1);
        let out = f();
        DETACH_DEPTH.set(DETACH_DEPTH.get() - 1);
        out
    }

    /// The closed requests (per-request identity checks — the aggregate
    /// can hide a single overlapping request).
    pub fn results(&self) -> Vec<RequestResult> {
        self.completed.lock().expect("completed poisoned").clone()
    }

    /// Reset the global phases and the request ledger (per-run isolation in
    /// a long-lived process; the daemon dump must not mix sweep runs).
    pub fn clear(&self) {
        self.phases.lock().expect("timings poisoned").clear();
        self.requests.lock().expect("requests poisoned").clear();
        self.completed.lock().expect("completed poisoned").clear();
    }

    /// The stacked accounting: aggregate the closed requests into
    /// `total == Σ phases + residual`.
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
#[derive(Debug, Default)]
struct OpStat {
    count: u64,
    nanos_total: u128,
    samples: Vec<u64>,
}

/// FUSE request statistics (per-opcode counts + latency percentiles, write
/// size histogram, max concurrency).
#[derive(Debug, Default)]
pub struct FuseStats {
    ops: Mutex<HashMap<&'static str, OpStat>>,
    /// Write-size histogram buckets.
    write_sizes: Mutex<[u64; 7]>,
    /// Requests currently in flight.
    in_flight: std::sync::atomic::AtomicU64,
    /// Maximum observed concurrency.
    max_in_flight: std::sync::atomic::AtomicU64,
}

/// Guard that tracks request concurrency; drops when the handler returns.
pub struct InFlight<'a> {
    stats: &'a FuseStats,
}

impl<'a> InFlight<'a> {
    /// Begin a request: increments the in-flight counter and updates the
    /// observed maximum.
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
    fn drop(&mut self) {
        self.stats
            .in_flight
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl FuseStats {
    /// Record one completed request.
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

    /// Record a write request's size.
    pub fn record_write_size(&self, len: usize) {
        let mut h = self.write_sizes.lock().expect("fuse stats poisoned");
        h[write_bucket(len)] += 1;
    }

    /// Maximum observed request concurrency.
    pub fn max_concurrency(&self) -> u64 {
        self.max_in_flight
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Per-opcode rows: (op, count, total_ms, p50_us, p95_us, p99_us).
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
