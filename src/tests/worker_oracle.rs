//! Phase-11D worker oracle (diagnostic, not sealed evidence — the sealed
//! run is captured and archived per the repo's evidence protocol).
//!
//! The 11C semaphore bounds the search/decode CPU but leaves `prepare` as
//! one opaque bucket. This oracle decomposes it at 1/2/4/8/16 writer
//! threads:
//!
//! ```text
//! prepare = useful search/decode CPU
//!         + worker_queue_wait   (parked on the semaphore)
//!         + spawn/join overhead (scoped-thread construction)
//!         + compose/phase-3/hash/validation + gaps
//! ```
//!
//! with the worker budget's counters (requested/granted/blocked batches,
//! peak queue depth) and the per-request latency percentiles. The gates:
//!
//! - **Gate A/B** — queue wait or spawn/join is a significant share of
//!   `prepare` → a persistent fair worker pool is justified.
//! - **Gate C** — useful search CPU dominates `prepare` → STOP scheduler
//!   work; the next lever is reducing the search work itself (the adaptive
//!   foreground search budget), not a pool.
//!
//! The hard success criterion for any pool is that it beats the semaphore
//! at 8/16-thread wall OR tail latency without materially increasing total
//! search CPU — the oracle's `worker_useful_cpu` row is the baseline for
//! that comparison.
//!
//! Workload discipline (learned the hard way in the first oracle run): a
//! FRESH store, FRESH files, and PER-WRITE-DISTINCT content per thread
//! count. Reusing one store across the sweep made every write after the
//! first 1024-op checkpoint dedup-hit (EXACT_REF) and the 16-thread row
//! measured the dedup cache, not the search — the 11C court's corpus rule
//! ("16 distinct 16 MiB streams so the T concurrent writers never share a
//! page") applies to the oracle too.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Instant;

use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::workers;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

fn create_store(dir: &TempDir) -> Arc<Store> {
    let cfg = StoreConfig {
        segment_size: 128 * 1024 * 1024,
        ..Default::default()
    };
    Arc::new(Store::create(dir.path(), &cfg, [0x11; 16]).unwrap())
}

fn create_files(store: &Store, n: usize) -> Vec<u64> {
    let mut inos = Vec::with_capacity(n);
    for i in 0..n {
        let ino = store
            .create_entry(
                store.current_root().root_dir_ino,
                format!("f{i}").as_bytes(),
                NewEntry::file(0o644, 1000, 1000),
                &CrashHooks::none(),
            )
            .unwrap();
        inos.push(ino);
    }
    inos
}

fn deterministic_noise(len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = seed;
    for _ in 0..len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push((state >> 33) as u8);
    }
    out
}

/// A 1 MiB stream DISTINCT for every (file, range): the sweep must never
/// repeat a 64 KiB content, or the write path's exact-dedup (P2, always
/// first) turns later writes into EXACT_REF aliases and the oracle stops
/// measuring search CPU.
fn stream_for(file_index: usize, range: u64) -> Vec<u8> {
    let mut seed = 0x11d_0001u64;
    seed ^= (file_index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    seed ^= range.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    deterministic_noise(65536 * 16, seed)
}

fn phase_row<'a>(
    rows: &'a [crate::perf::TimingRow],
    name: &str,
) -> Option<&'a crate::perf::TimingRow> {
    rows.iter().find(|r| r.phase == name)
}

/// p50/p95/p99 of the per-write totals (Q5). Only the `epoch_write`
/// requests are sampled — the store-setup creates are µs-scale and would
/// drag the percentiles below the write distribution.
fn latency_percentiles(results: &[crate::perf::RequestResult]) -> (f64, f64, f64) {
    let mut v: Vec<u64> = results
        .iter()
        .filter(|r| r.name == "epoch_write")
        .map(|r| r.total_ns)
        .collect();
    v.sort_unstable();
    if v.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let p = |q: f64| v[((v.len() - 1) as f64 * q) as usize] as f64 / 1e3;
    (p(0.50), p(0.95), p(0.99))
}

/// The fraction of a named probe phase's samples that are non-zero (the
/// phase rows carry 0/1 samples; `total_ms * 1e6 / count` is the mean).
fn probe_fraction(rows: &[crate::perf::TimingRow], name: &str) -> f64 {
    phase_row(rows, name)
        .map(|r| r.total_ms * 1e6 / r.count.max(1) as f64)
        .unwrap_or(0.0)
}

#[test]
fn print_worker_oracle() {
    let opts = OptimizeOptions::default();
    println!("\n==== Phase-11D worker oracle (epoch write path, release) ====");
    println!(
        "{:<8} {:>9} {:>9} {:>9} {:>9} {:>10} {:>9} {:>9} {:>9} {:>8} {:>9} {:>9}",
        "threads",
        "wall_ms",
        "prepare%",
        "queue%",
        "spawn%",
        "useful_cpu%",
        "util",
        "granted",
        "blocked",
        "qdepth",
        "p50_us",
        "p99_us",
    );
    for t in [1usize, 2, 4, 8, 16] {
        // A FRESH store per thread count: the sweep must not accumulate
        // epoch state or committed dedup entries across rows (see the
        // module doc — the first oracle run's 16-thread row measured a
        // checkpoint-fed EXACT_REF cache, not search CPU).
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir);
        let files = create_files(&store, 64);
        let fg = store.foreground_policy();
        let before = workers::WORKERS.snapshot();
        let t0 = Instant::now();
        std::thread::scope(|s| {
            for w in 0..t {
                let store = Arc::clone(&store);
                let files = &files;
                s.spawn(move || {
                    let mut i = w;
                    while i < files.len() {
                        let ino = files[i];
                        for r in 0..4u64 {
                            let data = stream_for(i, r);
                            store
                                .epoch_write(
                                    ino,
                                    r * data.len() as u64,
                                    &data,
                                    opts,
                                    fg,
                                    &CrashHooks::none(),
                                )
                                .unwrap();
                        }
                        i += t;
                    }
                });
            }
        });
        let wall_s = t0.elapsed().as_secs_f64();
        let after = workers::WORKERS.snapshot();

        // The decomposition (global phases; the request partition stays
        // intact — these are drill-down rows inside `prepare`).
        let rows = store.perf().snapshot();
        let total_ns = store.perf().reconcile().total_ms;
        let prepare = phase_row(&rows, "prepare")
            .map(|r| r.total_ms)
            .unwrap_or(0.0);
        let useful = phase_row(&rows, "worker_useful_cpu")
            .map(|r| r.total_ms)
            .unwrap_or(0.0);
        let queue = phase_row(&rows, "worker_queue_wait")
            .map(|r| r.total_ms)
            .unwrap_or(0.0);
        let scope = phase_row(&rows, "worker_scope_wall")
            .map(|r| r.total_ms)
            .unwrap_or(0.0);
        let search = phase_row(&rows, "search")
            .map(|r| r.total_ms)
            .unwrap_or(0.0);
        let search_rans = phase_row(&rows, "search_byte_rans")
            .map(|r| r.total_ms)
            .unwrap_or(0.0)
            + phase_row(&rows, "search_sequence_rans")
                .map(|r| r.total_ms)
                .unwrap_or(0.0);
        let validation = phase_row(&rows, "validation")
            .map(|r| r.total_ms)
            .unwrap_or(0.0);
        let reads = phase_row(&rows, "read_decode")
            .map(|r| r.count)
            .unwrap_or(0);
        let tasks = phase_row(&rows, "worker_tasks")
            .map(|r| r.count)
            .unwrap_or(0);

        let requested = after.requested.saturating_sub(before.requested);
        let granted = after.granted.saturating_sub(before.granted);
        let blocked = after.blocked.saturating_sub(before.blocked);
        let batches = after.batches.saturating_sub(before.batches).max(1);
        let max_q = after.max_queue_depth;

        // Spawn/join estimate: the scope wall minus the parallel-execution
        // floor (useful CPU / granted workers) per batch. `useful_cpu` is
        // a CPU-time SUM across parallel workers, so it is reported as a
        // ratio to `prepare` (it may exceed 100% — that is the point).
        let floor_per_batch = useful / granted.max(1) as f64;
        let spawn_join = (scope - floor_per_batch * batches as f64).max(0.0);
        // Worker utilization: useful CPU / (granted × scope wall).
        let util = useful / (granted as f64 * (scope / batches as f64)).max(1e-9);

        let (p50, _p95, p99) = latency_percentiles(&store.perf().results());
        let rec_inspect = store.perf().reconcile();
        println!(
            "          reconcile: n_req={} total_ms={:.1} prepare_ms={:.1} residual_ms={:.1} overlap={}",
            rec_inspect.requests,
            rec_inspect.total_ms,
            prepare,
            rec_inspect.residual_ms,
            rec_inspect.overlap,
        );

        println!(
            "{:<8} {:>9.0} {:>8.1}% {:>8.1}% {:>8.1}% {:>9.1}% {:>8.2} {:>9} {:>9} {:>8} {:>9.0} {:>9.0}",
            t,
            wall_s * 1e3,
            prepare / total_ns * 100.0,
            queue / prepare * 100.0,
            spawn_join / prepare * 100.0,
            useful / prepare * 100.0,
            util,
            granted,
            blocked,
            max_q,
            p50,
            p99,
        );
        println!(
            "          search_ms={:>7.1} rans_ms={:>7.1} validation_ms={:>7.1} tasks={} reads={}",
            search, search_rans, validation, tasks, reads
        );
        println!(
            "          probe: dedup_hit_frac={:.4} decisive1_frac={:.4} avg_pre_rans_cands={:.2}",
            probe_fraction(&rows, "probe_dedup_hit"),
            probe_fraction(&rows, "probe_decisive1"),
            probe_fraction(&rows, "probe_pre_rans_cands"),
        );
        // Workload-validity gate: with per-write-distinct content the
        // exact-dedup hit rate must be ~0 and the decisive early exit must
        // never fire — otherwise the row is measuring the EXACT_REF cache,
        // not search CPU, and the gates are meaningless.
        assert!(
            probe_fraction(&rows, "probe_dedup_hit") < 0.01,
            "threads={t}: dedup hit fraction {:?} — the sweep must feed distinct content",
            probe_fraction(&rows, "probe_dedup_hit")
        );
        assert!(
            probe_fraction(&rows, "probe_decisive1") < 0.01,
            "threads={t}: decisive early-exit fraction {:?} — the search is not running",
            probe_fraction(&rows, "probe_decisive1")
        );
        // The reconciliation identity must still hold (the drill-down rows
        // never touch the request partition), and the WALL drill-down
        // (queue + scope) must not exceed the enclosing `prepare` row
        // (useful_cpu is a CPU sum across parallel workers — it is not a
        // wall partition and is excluded from this check).
        let rec = store.perf().reconcile();
        assert!(!rec.overlap, "threads={t}: partition overlap");
        assert!(
            rec.residual_share < 0.15,
            "threads={t}: residual {:.1}% too large",
            rec.residual_share * 100.0
        );
        assert!(
            queue + scope <= prepare * 1.05,
            "threads={t}: wall drill-down (queue+scope {:.1} ms) must not exceed prepare ({:.1} ms) + 5%",
            queue + scope,
            prepare
        );
        let _ = (search, search_rans, validation, tasks, reads, requested);
    }
    println!(
        "\n(prepare% = prepare / total request time; queue% = semaphore wait / prepare [Gate A];"
    );
    println!(
        " spawn% = scope-wall − useful/granted per batch / prepare [Gate B]; useful_cpu% = worker thread-CPU / prepare [Gate C, >100% = parallel CPU];"
    );
    println!(
        " util = useful / (granted × batch wall); qdepth = peak threads parked on the semaphore.)"
    );
}

#[test]
fn worker_oracle_identity_holds() {
    // The oracle's drill-down rows must never disturb the request
    // partition: run one sweep and check the reconciliation identity and
    // that the drill-down sums do not exceed the enclosing row. Fresh
    // store, distinct content (the module-doc workload discipline).
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let files = create_files(&store, 16);
    let opts = OptimizeOptions::default();
    let fg = store.foreground_policy();
    std::thread::scope(|s| {
        for w in 0..4 {
            let store = Arc::clone(&store);
            let files = &files;
            s.spawn(move || {
                let mut i = w;
                while i < files.len() {
                    let data = stream_for(i, 0);
                    store
                        .epoch_write(files[i], 0, &data, opts, fg, &CrashHooks::none())
                        .unwrap();
                    i += 4;
                }
            });
        }
    });
    let rec = store.perf().reconcile();
    assert!(!rec.overlap, "partition overlap: {rec:?}");
    let rows = store.perf().snapshot();
    let prepare = phase_row(&rows, "prepare")
        .map(|r| r.total_ms)
        .unwrap_or(0.0);
    // WALL drill-down only: queue wait + scope wall are sub-segments of the
    // enclosing `prepare` row. `worker_useful_cpu` is a parallel CPU-time
    // SUM (it may legitimately exceed prepare's wall) and is excluded from
    // the wall budget by design.
    let drill: f64 = ["worker_queue_wait", "worker_scope_wall"]
        .iter()
        .map(|n| phase_row(&rows, n).map(|r| r.total_ms).unwrap_or(0.0))
        .sum();
    assert!(
        drill <= prepare * 1.05,
        "wall drill-down ({drill:.1} ms) must not exceed prepare ({prepare:.1} ms) + 5%"
    );
}
