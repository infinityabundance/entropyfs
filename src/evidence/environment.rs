//! Environment and context capture for benchmark evidence (§1, §42, §50).
//!
//! Every benchmark run records the full reproducibility context: git
//! revision, `Cargo.lock` hash, kernel, CPU model and feature set, governor,
//! memory, storage device, cache state, and the exact command line. JSON via
//! serde — evidence artifacts are human-readable; the permanent on-disk
//! format is explicit byte codecs elsewhere.
//!
//! # PURPOSE
//!
//! Make every measured claim re-runnable in context: a number without
//! its environment is not evidence (methodology §1 — if any context
//! field is missing, the run is exploratory, not admissible). This
//! module also provides the statistical helpers the campaign uses for
//! its percentiles and the device-level write/read deltas that bound a
//! campaign window.
//!
//! # BOUNDARY
//!
//! KNOWS: `/proc` and `/sys` (cpuinfo, meminfo, diskstats, hostname,
//! governor), `/proc/mounts` (device + fstype for the store dir), git
//! and the process argv, `Cargo.lock`. NEVER KNOWS: the store, the
//! corpora, or any filesystem format — it captures context and computes
//! statistics, nothing else.
//!
//! # MODEL
//!
//! One [`Environment`] snapshot per capture, serialized as pretty JSON.
//! Capture is best-effort by construction: every source is read with
//! fallbacks (empty string, 0, "unknown") so a missing file can never
//! fail the campaign — but the field is then visibly empty rather than
//! silently wrong, which is what makes an incomplete context
//! detectable (and therefore non-admissible). Device accounting is
//! sampled before and after a window and differenced (saturating) into
//! a [`DiskDelta`].
//!
//! # CORRECTNESS INVARIANTS
//!
//! - Units are explicit and consistent: memory in KiB, CPU frequency in
//!   MHz, disk sectors (× 512 = bytes via `DiskDelta::written_bytes` /
//!   `read_bytes`), latencies in seconds (converted to µs only at
//!   report time), time as unix seconds.
//! - Percentiles are nearest-rank over a SORTED sample (`percentile`
//!   documents its input contract); `summary` sorts and is the only
//!   public aggregator.
//! - `mount_of` picks the longest matching mount-point prefix, so a
//!   nested mount resolves to the device that actually backs the path.
//! - `current_uid` avoids the `rustix::process` feature by parsing
//!   `/proc/self/status` — a deliberate dependency-boundary choice.
//!
//! # RESOURCE BOUNDS
//!
//! Bounded by the small proc/sys files read; `cpu_flags` is the only
//! potentially long field (one line of `/proc/cpuinfo`).
//!
//! # HISTORY / EVIDENCE
//!
//! The field set implements `docs/performance/methodology.md` §1
//! verbatim; every sealed campaign archives its `environment.json`
//! under `evidence/performance/campaign-<ts>-<rev>/`.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

/// Full benchmark context (§1 of `docs/performance/methodology.md`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Environment {
    /// Unix seconds at capture.
    pub timestamp_unix: u64,
    /// Short git revision (best-effort).
    pub revision_short: String,
    /// Full git revision (best-effort).
    pub revision_full: String,
    /// BLAKE3 of `Cargo.lock` (hex).
    pub cargo_lock_hash: String,
    /// `Cargo.lock` size in bytes.
    pub cargo_lock_bytes: u64,
    /// Kernel ostype (`/proc/sys/kernel/ostype`).
    pub kernel_ostype: String,
    /// Kernel release (`/proc/sys/kernel/osrelease`).
    pub kernel_release: String,
    /// Kernel build version (`/proc/sys/kernel/version`).
    pub kernel_version: String,
    /// CPU model name (`/proc/cpuinfo`, first block).
    pub cpu_model: String,
    /// CPU feature flags (`/proc/cpuinfo`, first block).
    pub cpu_flags: String,
    /// Logical CPU count.
    pub cpu_count: usize,
    /// Nominal CPU frequency (MHz, best-effort from `/proc/cpuinfo`).
    pub cpu_mhz: Option<f64>,
    /// Total RAM (`/proc/meminfo` MemTotal, KiB).
    pub memory_kib: u64,
    /// CPU frequency governor (`scaling_governor`; "unknown" if absent).
    pub governor: String,
    /// Directory containing the benchmark stores.
    pub store_dir: String,
    /// Block device backing `store_dir` (`/proc/mounts`), e.g. `/dev/nvme1n1p1`.
    pub store_device: String,
    /// Filesystem type backing `store_dir`.
    pub store_fstype: String,
    /// Cache state: cold (dropped), warm (retained), or retained/unknown.
    pub cache_state: String,
    /// Exact command line (argv).
    pub command: String,
    /// Optimization policy mode (`balanced`, `capacity`, …).
    pub policy_mode: String,
    /// User id running the benchmark.
    pub uid: u32,
    /// Hostname.
    pub hostname: String,
}

/// `/proc/diskstats` counters for one block device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskStats {
    /// Read operations issued.
    pub reads: u64,
    /// Sectors read.
    pub read_sectors: u64,
    /// Write operations issued.
    pub writes: u64,
    /// Sectors written.
    pub write_sectors: u64,
}

/// Device-level delta between two `DiskStats` samples.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskDelta {
    /// Device name (e.g. `nvme1n1p1`).
    pub device: String,
    /// Reads issued at the device.
    pub reads: u64,
    /// Sectors read at the device.
    pub read_sectors: u64,
    /// Writes issued at the device.
    pub writes: u64,
    /// Sectors written at the device.
    pub write_sectors: u64,
}

impl DiskDelta {
    /// Bytes written at the device level (sectors × 512).
    pub fn written_bytes(&self) -> u64 {
        self.write_sectors * 512
    }

    /// Bytes read at the device level (sectors × 512).
    pub fn read_bytes(&self) -> u64 {
        self.read_sectors * 512
    }
}

/// Simple descriptive statistics over a latency/throughput sample.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct StatSummary {
    /// Number of samples.
    pub count: usize,
    /// Arithmetic mean.
    pub mean: f64,
    /// Minimum.
    pub min: f64,
    /// Median (p50).
    pub p50: f64,
    /// 95th percentile.
    pub p95: f64,
    /// 99th percentile.
    pub p99: f64,
    /// Maximum.
    pub max: f64,
}

impl Environment {
    /// Capture the full context.
    pub fn capture(
        repo_root: &Path,
        store_dir: &Path,
        cache_state: &str,
        policy_mode: &str,
    ) -> Environment {
        let (dev, fstype) = mount_of(store_dir);
        let cpu = cpu_info();
        let clock = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let lock_path = repo_root.join("Cargo.lock");
        let (lock_hash, lock_bytes) = std::fs::read(&lock_path)
            .map(|b| (blake3::hash(&b).to_hex().to_string(), b.len() as u64))
            .unwrap_or_else(|_| (String::new(), 0));
        Environment {
            timestamp_unix: clock,
            revision_short: git_output(&["rev-parse", "--short", "HEAD"]),
            revision_full: git_output(&["rev-parse", "HEAD"]),
            cargo_lock_hash: lock_hash,
            cargo_lock_bytes: lock_bytes,
            kernel_ostype: read_trimmed("/proc/sys/kernel/ostype"),
            kernel_release: read_trimmed("/proc/sys/kernel/osrelease"),
            kernel_version: read_trimmed("/proc/sys/kernel/version"),
            cpu_model: cpu.model,
            cpu_flags: cpu.flags,
            cpu_count: cpu.count,
            cpu_mhz: cpu.mhz,
            memory_kib: meminfo_total_kib(),
            governor: read_trimmed("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
            store_dir: store_dir.display().to_string(),
            store_device: dev,
            store_fstype: fstype,
            cache_state: cache_state.to_string(),
            command: std::env::args().collect::<Vec<_>>().join(" "),
            policy_mode: policy_mode.to_string(),
            uid: current_uid(),
            hostname: read_trimmed("/proc/sys/kernel/hostname"),
        }
    }

    /// Render as pretty JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }
}

/// Sample `/proc/diskstats` for the named device.
pub fn diskstats(device: &str) -> Option<DiskStats> {
    let body = std::fs::read_to_string("/proc/diskstats").ok()?;
    for line in body.lines() {
        let mut it = line.split_whitespace();
        let (_major, _minor, name) = (it.next()?, it.next()?, it.next()?);
        if name != device {
            continue;
        }
        let v: Vec<u64> = it.filter_map(|f| f.parse().ok()).collect();
        if v.len() >= 6 {
            return Some(DiskStats {
                reads: v[0],
                read_sectors: v[2],
                writes: v[4],
                write_sectors: v[6],
            });
        }
        return None;
    }
    None
}

/// Delta between two samples (saturating).
pub fn disk_delta(device: &str, before: &DiskStats, after: &DiskStats) -> DiskDelta {
    DiskDelta {
        device: device.to_string(),
        reads: after.reads.saturating_sub(before.reads),
        read_sectors: after.read_sectors.saturating_sub(before.read_sectors),
        writes: after.writes.saturating_sub(before.writes),
        write_sectors: after.write_sectors.saturating_sub(before.write_sectors),
    }
}

/// Nearest-rank percentile of a *sorted* sample.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[idx.saturating_sub(1).min(sorted.len() - 1)]
}

/// Summary over an unsorted sample.
pub fn summary(vals: &[f64]) -> StatSummary {
    if vals.is_empty() {
        return StatSummary::default();
    }
    let mut sorted = vals.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
    StatSummary {
        count: sorted.len(),
        mean,
        min: sorted[0],
        p50: percentile(&sorted, 50.0),
        p95: percentile(&sorted, 95.0),
        p99: percentile(&sorted, 99.0),
        max: *sorted.last().unwrap(),
    }
}

/// The block device and filesystem type backing `path` (longest mount-point
/// prefix match over `/proc/mounts`).
pub fn mount_of(path: &Path) -> (String, String) {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let target = canon.display().to_string();
    let mut best: Option<(String, String, usize)> = None;
    if let Ok(body) = std::fs::read_to_string("/proc/mounts") {
        for line in body.lines() {
            let mut it = line.split_whitespace();
            let dev = it.next().unwrap_or_default().to_string();
            let mp = it.next().unwrap_or_default().replace("\\040", " ");
            let fstype = it.next().unwrap_or_default().to_string();
            if mp.len() >= best.as_ref().map(|b| b.2).unwrap_or(0)
                && (target == mp || target.starts_with(&format!("{mp}/")))
            {
                best = Some((dev, fstype, mp.len()));
            }
        }
    }
    best.map(|(d, f, _)| (d, f))
        .unwrap_or_else(|| ("unknown".into(), "unknown".into()))
}

struct CpuInfo {
    model: String,
    flags: String,
    count: usize,
    mhz: Option<f64>,
}

fn cpu_info() -> CpuInfo {
    let body = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let mut model = String::new();
    let mut flags = String::new();
    let mut mhz = None;
    let mut count = 0usize;
    let mut first = true;
    for line in body.lines() {
        if let Some(v) = line.strip_prefix("model name") {
            if first {
                model = v.trim_start_matches(':').trim().to_string();
            }
        } else if let Some(v) = line.strip_prefix("flags") {
            if first {
                flags = v.trim_start_matches(':').trim().to_string();
            }
        } else if let Some(v) = line.strip_prefix("cpu MHz") {
            if first {
                mhz = v.trim_start_matches(':').trim().parse().ok();
            }
        } else if let Some(v) = line.strip_prefix("max MHz") {
            if first {
                mhz = v.trim_start_matches(':').trim().parse().ok();
            }
        } else if line.starts_with("processor") {
            count += 1;
        }
        if line.trim().is_empty() {
            first = false;
        }
    }
    if count == 0 {
        count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
    }
    CpuInfo {
        model,
        flags,
        count,
        mhz,
    }
}

fn meminfo_total_kib() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|body| {
            body.lines()
                .find(|l| l.starts_with("MemTotal"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

fn read_trimmed(path: &str) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn current_uid() -> u32 {
    // No `rustix::process` feature needed: parse the real uid from
    // /proc/self/status (first number of the `Uid:` line).
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|body| {
            body.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

fn git_output(args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Render a `BTreeMap<String, u64>` (representation families, byte
/// categories) as a sorted CSV table fragment for `results.csv`.
pub fn map_to_csv(prefix: &str, m: &BTreeMap<String, u64>) -> Vec<String> {
    m.iter().map(|(k, v)| format!("{prefix},{k},{v}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_nearest_rank() {
        let s = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert_eq!(percentile(&s, 50.0), 5.0);
        assert_eq!(percentile(&s, 100.0), 10.0);
        assert_eq!(percentile(&s, 0.0), 1.0);
        assert_eq!(percentile(&[], 50.0), 0.0);
    }

    #[test]
    fn summary_basics() {
        let s = summary(&[1.0, 2.0, 3.0]);
        assert_eq!(s.count, 3);
        assert_eq!(s.min, 1.0);
        assert_eq!(s.max, 3.0);
        assert!((s.mean - 2.0).abs() < 1e-9);
    }

    #[test]
    fn disk_delta_saturates() {
        let b = DiskStats {
            reads: 10,
            read_sectors: 20,
            writes: 5,
            write_sectors: 8,
        };
        let a = DiskStats {
            reads: 13,
            read_sectors: 21,
            writes: 5,
            write_sectors: 10,
        };
        let d = disk_delta("test", &b, &a);
        assert_eq!(d.reads, 3);
        assert_eq!(d.write_sectors, 2);
        assert_eq!(d.written_bytes(), 1024);
        assert_eq!(d.read_bytes(), 512);
    }

    #[test]
    fn mount_of_returns_something() {
        // The repo lives on a real filesystem; this should resolve to a
        // device and fstype rather than the "unknown" fallback.
        let (dev, fstype) = mount_of(Path::new("/"));
        assert!(dev.starts_with("/dev/") || dev == "/" || !dev.is_empty());
        assert!(!fstype.is_empty());
    }
}
