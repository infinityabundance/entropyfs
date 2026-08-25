//! Phase-10A performance instrumentation (diagnostic, not sealed evidence).
//!
//! Two collectors:
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
//! Both are cheap (one `Instant` pair + one mutex push per sample) and
//! strictly diagnostic: they never affect correctness or persistence.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// Sample-ring bound per phase (keeps the memory bounded).
const MAX_SAMPLES: usize = 4096;

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
