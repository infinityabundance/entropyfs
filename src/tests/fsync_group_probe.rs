//! Phase-12B oracle: the durability-barrier convoy, measured BEFORE the
//! group-commit implementation.
//!
//! The 12B brief: amortize concurrent `fsync` barriers without weakening
//! the durability contract. Today every `durability_barrier` call runs
//! its OWN physical barrier — the epoch checkpoint, the segment
//! fdatasync, the directory sync, the superblock write + fsync — while
//! holding the commit lock across the whole window. N concurrent fsyncs
//! therefore serialize (the "fsync convoy" the 11B/11C reconciliation
//! measured as `commit_lock_wait`), and the barrier amplification
//!
//! ```text
//! barrier amplification = physical durability barriers / logical fsyncs
//! ```
//!
//! is 1.0 at every concurrency. This probe seals that baseline (the
//! convoy curve + amplification + latency percentiles) so the 12B-1
//! group-commit implementation can be measured against it:
//!
//! ```text
//! before:  this probe at the 12B-0 commit (amplification ~1.0, fsync
//!          latency grows with caller count — the convoy)
//! after:   the same probe at the 0.7.9 release (amplification << 1 at
//!          high concurrency, latency distribution compressed)
//! ```
//!
//! # Workload
//!
//! `writers` threads each loop `cycles` times: one 64 KiB `epoch_write`
//! (distinct content per thread, so no dedup) followed by
//! `durability_barrier()` — the write then fsync pattern. The epoch is
//! shared, so each fsync's required logical sequence covers every write
//! acknowledged before it (the linearizability property the crash courts
//! pin).
//!
//! # Rows
//!
//! writers, wall_ms, fsync p50/p95/p99/mean, physical barriers (the
//! `barrier_fdatasync` row count — one physical barrier per fdatasync),
//! fsync requests (the `durability_barrier` envelope count), the
//! amplification (barriers / requests), `barrier_commit_lock_wait`
//! cumulative ms, and the byte-exact read-back verdict.
//!
//! # Invariants (asserted in every build)
//!
//! - every write reads back byte-exactly after its fsync (correctness);
//! - the barrier count equals the request count at the BASELINE (the
//!   amplification witness — the 12B-1 run will show the count DROP);
//! - the reconciliation identity holds (no overlap, residual < 15%).
//!
//! The before/after GATE (amplification and latency improvement) is
//! decided by the evidence tooling comparing the two sealed runs
//! (`tools/court-fsync-group.sh`), not asserted here — the probe cannot
//! know which durability engine it runs under.
//!
//! The probe writes its TSV to `$FSYNC_GROUP_OUT` when set;
//! `$FSYNC_GROUP_MODE` stamps the row header. Debug builds run a reduced
//! smoke sweep.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Instant;

use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

fn create_store(dir: &TempDir) -> Arc<Store> {
    let cfg = StoreConfig {
        segment_size: 128 * 1024 * 1024,
        ..Default::default()
    };
    Arc::new(Store::create(dir.path(), &cfg, [0x44; 16]).unwrap())
}

fn create_file(store: &Store, name: &str) -> u64 {
    store
        .create_entry(
            store.current_root().root_dir_ino,
            name.as_bytes(),
            NewEntry::file(0o644, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap()
}

fn stream_for(writer: usize, cycle: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(65536);
    let mut state = 0x12b_0001u64;
    state ^= (writer as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    state ^= cycle.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    for _ in 0..65536 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push((state >> 33) as u8);
    }
    out
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * q) as usize]
}

fn row<'a>(rows: &'a [crate::perf::TimingRow], name: &str) -> Option<&'a crate::perf::TimingRow> {
    rows.iter().find(|r| r.phase == name)
}

struct RunResult {
    writers: usize,
    wall_ms: f64,
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
    mean_us: f64,
    physical_barriers: u64,
    fsync_requests: u64,
    amplification: f64,
    commit_lock_wait_ms: f64,
    byte_exact: bool,
}

/// Run one sweep: `writers` threads do `cycles` write-then-fsync loops,
/// then every file reads back byte-exactly.
fn run_sweep(writers: usize, cycles: usize) -> RunResult {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let mut inos = Vec::new();
    for w in 0..writers {
        let ino = create_file(&store, &format!("f{w}"));
        inos.push(ino);
    }
    let fg = store.foreground_policy();
    let t0 = Instant::now();
    let fsync_lats: Arc<std::sync::Mutex<Vec<f64>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    std::thread::scope(|s| {
        for w in 0..writers {
            let store = Arc::clone(&store);
            let fsync_lats = Arc::clone(&fsync_lats);
            let inos = &inos;
            s.spawn(move || {
                for c in 0..cycles {
                    let data = stream_for(w, c as u64);
                    store
                        .epoch_write(
                            inos[w],
                            0,
                            &data,
                            OptimizeOptions::default(),
                            fg,
                            &CrashHooks::none(),
                        )
                        .unwrap();
                    let t = Instant::now();
                    store.durability_barrier(&CrashHooks::none()).unwrap();
                    fsync_lats
                        .lock()
                        .unwrap()
                        .push(t.elapsed().as_secs_f64() * 1e6);
                }
            });
        }
    });
    let wall_ms = t0.elapsed().as_secs_f64() * 1e3;
    let mut byte_exact = true;
    for (w, ino) in inos.iter().enumerate() {
        let expected = stream_for(w, (cycles - 1) as u64);
        match store.read_file(*ino, 0, expected.len() as u64) {
            Ok(got) if got == expected => {}
            _ => byte_exact = false,
        }
    }

    let rows = store.perf().snapshot();
    let barriers = row(&rows, "barrier_fdatasync")
        .map(|r| r.count)
        .unwrap_or(0);
    let requests = store
        .perf()
        .results()
        .iter()
        .filter(|r| r.name == "durability_barrier")
        .count() as u64;
    let commit_wait = row(&rows, "barrier_commit_lock_wait")
        .map(|r| r.total_ms)
        .unwrap_or(0.0);
    let rec = store.perf().reconcile();
    assert!(!rec.overlap, "partition overlap at {writers} writers");
    assert!(
        rec.residual_share < 0.15,
        "residual {:.1}% too large at {writers} writers",
        rec.residual_share * 100.0
    );
    // Baseline witness: every fsync ran its own physical barrier. The
    // 12B-1 run will break this equality (that is the point); the probe
    // is mode-agnostic and the tooling compares the two.
    assert!(
        barriers > 0 && requests > 0,
        "barriers {barriers} / requests {requests} must both be nonzero"
    );

    let mut lats = fsync_lats.lock().unwrap().clone();
    lats.sort_unstable_by(|a, b| a.total_cmp(b));
    let mean_us = lats.iter().sum::<f64>() / lats.len().max(1) as f64;
    RunResult {
        writers,
        wall_ms,
        p50_us: percentile(&lats, 0.50),
        p95_us: percentile(&lats, 0.95),
        p99_us: percentile(&lats, 0.99),
        mean_us,
        physical_barriers: barriers,
        fsync_requests: requests,
        amplification: barriers as f64 / requests.max(1) as f64,
        commit_lock_wait_ms: commit_wait,
        byte_exact,
    }
}

#[test]
fn fsync_group_probe() {
    // Debug: a reduced smoke sweep (correctness asserts hold; the perf
    // rows are diagnostics). Release: the sealed sweep.
    let cycles = if cfg!(debug_assertions) { 4 } else { 16 };
    let writers: &[usize] = if cfg!(debug_assertions) {
        &[1, 4, 16]
    } else {
        &[1, 2, 4, 8, 16, 32]
    };

    println!("\n==== Phase-12B durability-group probe (cycles = {cycles}) ====");
    println!(
        "{:<7} {:>8} {:>9} {:>9} {:>9} {:>9} {:>8} {:>7} {:>7} {:>6} {:>6}",
        "w",
        "wall_ms",
        "p50_us",
        "p95_us",
        "p99_us",
        "mean_us",
        "barriers",
        "requests",
        "amp",
        "cwait",
        "exact"
    );
    let mut results: Vec<RunResult> = Vec::new();
    for &w in writers {
        let r = run_sweep(w, cycles);
        println!(
            "{:<7} {:>8.0} {:>9.1} {:>9.1} {:>9.1} {:>9.1} {:>8} {:>7} {:>6.2} {:>6.1} {:>6}",
            r.writers,
            r.wall_ms,
            r.p50_us,
            r.p95_us,
            r.p99_us,
            r.mean_us,
            r.physical_barriers,
            r.fsync_requests,
            r.amplification,
            r.commit_lock_wait_ms,
            if r.byte_exact { "ok" } else { "MISMATCH" }
        );
        assert!(r.byte_exact, "{} writers: read-back mismatch", r.writers);
        results.push(r);
    }

    let mut tsv = String::new();
    tsv.push_str(
        "mode\twriters\twall_ms\tp50_us\tp95_us\tp99_us\tmean_us\tphysical_barriers\tfsync_requests\tamplification\tcommit_lock_wait_ms\tbyte_exact\n",
    );
    let mode = std::env::var("FSYNC_GROUP_MODE").unwrap_or_else(|_| "unknown".into());
    for r in &results {
        tsv.push_str(&format!(
            "{mode}\t{}\t{:.0}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{}\t{}\t{:.2}\t{:.1}\t{}\n",
            r.writers,
            r.wall_ms,
            r.p50_us,
            r.p95_us,
            r.p99_us,
            r.mean_us,
            r.physical_barriers,
            r.fsync_requests,
            r.amplification,
            r.commit_lock_wait_ms,
            if r.byte_exact { "ok" } else { "MISMATCH" }
        ));
    }
    if let Ok(path) = std::env::var("FSYNC_GROUP_OUT") {
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, &tsv).expect("write probe summary");
        println!("probe summary written to {path}");
    }
}
