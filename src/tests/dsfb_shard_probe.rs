//! Phase-11F probe: the sharded DSFB observer vs the single-mutex observer.
//!
//! The 11D worker oracle measured ~0.5 s of DSFB observer-mutex contention
//! (an independent finding, deliberately kept out of the 11E pool
//! attribution), and the 11E probe predicted the mutex would become MORE
//! visible under the fair pool: more independent requests advance through
//! search simultaneously instead of whole batches being serialized, so 16
//! workers call `dsfb_observe`/`dsfb_plan`/`dsfb_trust` concurrently against
//! one store-level mutex. Phase-11F replaces that observer with a sharded
//! one (16 shards, stable `ChunkKey` hash → shard, per-shard mutex, atomic
//! global stats) and this probe measures the difference.
//!
//! # Attribution rule
//!
//! ONLY the observer changes between the two sealed runs:
//!
//! ```text
//! before: pool-16 + single-mutex StorageObserver  (rev = 11F-0 commit)
//! after:  pool-16 + ShardedStorageObserver         (rev = 0.7.7)
//! ```
//!
//! Same workload, same corpus, same ForegroundPolicy, same representation
//! set, same worker CPU work, same instrumentation. The probe itself is
//! observer-agnostic: it runs the identical sweep and rows at either
//! commit, and the evidence tool stamps the observer identity
//! (`DSFB_PROBE_MODE=mutex|sharded`) into the archived summary. Running
//! the same binary-shape at both commits is what makes the A/B attribution
//! clean — there is no `if sharded` anywhere in the probe.
//!
//! # Workload
//!
//! Pool-16 (the sealed mount default), writers 1/8/16, the same
//! per-write-distinct-content sweep as the 11E probe (fresh store, fresh
//! files, LCG noise seeded per (file, range) so no write ever dedups into
//! EXACT_REF and the search CPU stays real). Every run:
//!
//! 1. writes 4 x 1 MiB per file through `epoch_write` under the pool;
//! 2. reads every file back byte-exactly through the epoch overlay;
//! 3. checkpoints, then reads committed accounting: logical bytes
//!    (must equal the logical input exactly — the byte-identity invariant
//!    of the brief), reachable persisted bytes (density), the
//!    representation-family histogram, and the total candidate count;
//! 4. collects the perf rows: wall, p50/p99/mean/max epoch_write latency,
//!    `worker_useful_cpu` (parallel CPU sum), `search` (search wall sum),
//!    `prepare`, `worker_queue_wait`, and the `dsfb_plan`/`dsfb_trust`/
//!    `dsfb_observe` rows the store instrumentation added — the direct
//!    observer-call wall proxy for the brief's "DSFB lock wait".
//!
//! # Hard invariants (asserted in every build, both commits)
//!
//! - byte-exact readback of every file (scheduling order may vary;
//!   persisted semantic order may not);
//! - logical committed bytes == the exact logical input (density is
//!   measured against this identity, never guessed);
//! - the reconciliation partition stays non-overlapping with residual
//!   < 15% (the dsfb rows are global-only by design and cannot disturb
//!   it);
//! - the pool's backpressure bound holds (no unbounded queueing).
//!
//! The before/after GATES (wall/latency/CPU improvement) are NOT asserted
//! here — the probe cannot know which observer it runs under. They are
//! evaluated by the evidence tooling comparing the two sealed runs
//! (`tools/court-dsfb-shard.sh`), and the decision lives in the archived
//! `results.json`.
//!
//! The probe writes its summary TSV to `$DSFB_PROBE_OUT` when set (the
//! evidence tool's capture path); otherwise it prints the table to stdout
//! (`cargo test --release --lib dsfb_shard_probe -- --nocapture`).

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
    Arc::new(Store::create(dir.path(), &cfg, [0x22; 16]).unwrap())
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
/// first) turns later writes into EXACT_REF aliases and the probe stops
/// measuring search CPU (the 11D workload-validity discipline). The LCG
/// noise is incompressible, so the representation set is dominated by RAW
/// — this probe is about the OBSERVER's contention cost (wall/latency/CPU),
/// not about density; density identity is still asserted (logical == input)
/// and reported.
fn stream_for(file_index: usize, range: u64) -> Vec<u8> {
    let mut seed = 0x11f_0001u64;
    seed ^= (file_index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    seed ^= range.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    deterministic_noise(65536 * 16, seed)
}

fn row<'a>(rows: &'a [crate::perf::TimingRow], name: &str) -> Option<&'a crate::perf::TimingRow> {
    rows.iter().find(|r| r.phase == name)
}

/// The epoch_write request latencies only (the store-setup creates are
/// µs-scale and would drag the distribution below the write path).
fn write_latencies_us(results: &[crate::perf::RequestResult]) -> Vec<f64> {
    let mut v: Vec<f64> = results
        .iter()
        .filter(|r| r.name == "epoch_write")
        .map(|r| r.total_ns as f64 / 1e3)
        .collect();
    v.sort_unstable_by(|a, b| a.total_cmp(b));
    v
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * q) as usize]
}

/// Representation-family histogram across every committed extent of the
/// sweep files (the brief's "representation choices" row). Post-checkpoint
/// only: the committed extent trees are the persisted truth; the epoch
/// overlay is not part of the on-disk accounting.
fn representation_distribution(
    store: &Store,
    inos: &[u64],
) -> std::collections::BTreeMap<&'static str, u64> {
    let limits = *store.limits();
    let mut counts = std::collections::BTreeMap::new();
    for &ino in inos {
        let Ok(Some(inode)) = store.get_inode(ino) else {
            continue;
        };
        let root = match inode.data {
            crate::store::inode::InodeData::File { extent_root } => extent_root,
            _ => continue,
        };
        if root.is_zero() {
            continue;
        }
        let Ok(entries) = crate::store::extent_tree::scan_all(
            root,
            crate::store::BTREE_ORDER,
            limits.max_fanout,
            store,
        ) else {
            continue;
        };
        for (_, bytes) in entries {
            if let Ok(d) = crate::format::descriptor::decode(&bytes, &limits) {
                *counts.entry(d.family()).or_insert(0) += 1;
            }
        }
    }
    counts
}

struct RunResult {
    writers: usize,
    wall_ms: f64,
    p50_us: f64,
    p99_us: f64,
    mean_us: f64,
    max_us: f64,
    useful_cpu_ms: f64,
    search_ms: f64,
    prepare_ms: f64,
    dsfb_observe_ms: f64,
    dsfb_plan_ms: f64,
    dsfb_trust_ms: f64,
    queue_share_pct: f64,
    peak_in_flight: usize,
    candidates: u64,
    logical_bytes: u64,
    reachable_bytes: u64,
    byte_exact: bool,
    families: std::collections::BTreeMap<&'static str, u64>,
}

/// Run one sweep: `writers` threads write `files` files (4 x 1 MiB each,
/// distinct content) through the pool at 16 persistent workers, verify
/// byte-exact readback, checkpoint, and collect the 11F oracle rows. The
/// caller must hold `workers::tests::POOL_LOCK` (the pool is
/// process-global).
fn run_sweep(writers: usize, files: usize, opts: OptimizeOptions) -> RunResult {
    workers::POOL.enable(16, 8);
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let inos = create_files(&store, files);
    workers::POOL.bind(&store);
    store.enable_worker_pool();
    let fg = store.foreground_policy();
    let t0 = Instant::now();
    std::thread::scope(|s| {
        for w in 0..writers {
            let store = Arc::clone(&store);
            let inos = &inos;
            s.spawn(move || {
                let mut i = w;
                while i < inos.len() {
                    for r in 0..4u64 {
                        let data = stream_for(i, r);
                        store
                            .epoch_write(
                                inos[i],
                                r * data.len() as u64,
                                &data,
                                opts,
                                fg,
                                &CrashHooks::none(),
                            )
                            .unwrap();
                    }
                    i += writers;
                }
            });
        }
    });
    let wall_ms = t0.elapsed().as_secs_f64() * 1e3;

    // Byte-exact readback through the epoch overlay (the writes are still
    // in the active epoch here — exactly what the FUSE read handler sees).
    let mut byte_exact = true;
    let ep = store.epoch();
    for (i, ino) in inos.iter().enumerate() {
        let mut expected = Vec::with_capacity(4 * 1024 * 1024);
        for r in 0..4u64 {
            expected.extend_from_slice(&stream_for(i, r));
        }
        match store.read_file_epoch(&ep, *ino, 0, expected.len() as u64) {
            Ok(got) if got == expected => {}
            _ => byte_exact = false,
        }
    }
    drop(ep);

    // Checkpoint so the committed accounting (logical/reachable bytes,
    // representation families) reflects the persisted truth.
    store.epoch_checkpoint(&CrashHooks::none()).unwrap();

    let rows = store.perf().snapshot();
    let prepare = row(&rows, "prepare").map(|r| r.total_ms).unwrap_or(0.0);
    let queue = row(&rows, "worker_queue_wait")
        .map(|r| r.total_ms)
        .unwrap_or(0.0);
    let search = row(&rows, "search").map(|r| r.total_ms).unwrap_or(0.0);
    let useful = row(&rows, "worker_useful_cpu")
        .map(|r| r.total_ms)
        .unwrap_or(0.0);
    let dsfb_observe = row(&rows, "dsfb_observe")
        .map(|r| r.total_ms)
        .unwrap_or(0.0);
    let dsfb_plan = row(&rows, "dsfb_plan").map(|r| r.total_ms).unwrap_or(0.0);
    let dsfb_trust = row(&rows, "dsfb_trust").map(|r| r.total_ms).unwrap_or(0.0);
    let lat = write_latencies_us(&store.perf().results());
    let p50 = percentile(&lat, 0.50);
    let p99 = percentile(&lat, 0.99);
    let mean_us = lat.iter().sum::<f64>() / lat.len().max(1) as f64;
    let max_us = lat.iter().copied().fold(0.0f64, f64::max);
    let diag = workers::POOL.diagnostics();
    let candidates = store.candidates_evaluated();

    // Logical identity: the committed logical bytes (sum of materialized
    // descriptor lengths, `Store::logical_bytes`) must equal the logical
    // input exactly. Reachable persisted bytes (the mark-live walk over
    // the object index, the same accounting the benchmark uses) bounds the
    // density measurement.
    let logical = store.logical_bytes().unwrap();
    let reachable: u64 = crate::store::gc::mark_live(&store)
        .unwrap()
        .into_iter()
        .filter_map(|id| store.object_index().get(&id).map(|loc| loc.total_size()))
        .sum();

    // Reconciliation identity (the dsfb rows are global-only, so the
    // envelope partition must still close without overlap).
    let rec = store.perf().reconcile();
    assert!(!rec.overlap, "partition overlap at {writers} writers");
    assert!(
        rec.residual_share < 0.15,
        "residual {:.1}% too large at {writers} writers",
        rec.residual_share * 100.0
    );

    // Byte-identity + density invariants (the brief's hard requirements):
    // logical committed bytes must equal the logical input exactly, and
    // the reachable persisted bytes must be bounded by the same input
    // (density >= 1.0 is a structural guarantee — reachable bytes are
    // physical records; the identity check is the meaningful assertion).
    let logical_input = (files * 4 * 1024 * 1024) as u64;
    assert_eq!(
        logical, logical_input,
        "logical committed bytes must equal the logical input exactly ({logical_input})"
    );
    assert!(
        reachable > 0 && reachable <= logical_input * 2,
        "reachable persisted bytes {reachable} out of sane bounds vs logical input {logical_input}"
    );

    RunResult {
        writers,
        wall_ms,
        p50_us: p50,
        p99_us: p99,
        mean_us,
        max_us,
        useful_cpu_ms: useful,
        search_ms: search,
        prepare_ms: prepare,
        dsfb_observe_ms: dsfb_observe,
        dsfb_plan_ms: dsfb_plan,
        dsfb_trust_ms: dsfb_trust,
        queue_share_pct: if prepare > 0.0 {
            queue / prepare * 100.0
        } else {
            0.0
        },
        peak_in_flight: diag.peak_in_flight,
        candidates,
        logical_bytes: logical,
        reachable_bytes: reachable,
        byte_exact,
        families: representation_distribution(&store, &inos),
    }
}

#[test]
fn dsfb_shard_probe() {
    // The pool is process-global; serialize with the 11E probe and the
    // workers.rs mechanism test.
    let _guard = workers::tests::POOL_LOCK
        .lock()
        .expect("pool test lock poisoned");
    // Debug: a machinery smoke sweep (16 files, still byte-exact-asserted —
    // correctness invariants run in every build; perf rows are diagnostics).
    // Release: the sealed 64-file sweep the oracle was derived from.
    let files = if cfg!(debug_assertions) { 16 } else { 64 };
    let opts = OptimizeOptions::default();

    println!("\n==== Phase-11F DSFB-observer probe (pool-16; {files} files) ====");

    // Alone baseline (1 writer) + the contention curve (8/16 writers) —
    // the DSFB mutex cost is a CONTENTION phenomenon, so the curve is the
    // measurement. The stress run (16 writers x 256 files = 16k distinct
    // chunks, 1 GiB) is the scale probe: a 4x bigger observer map with 4x
    // the calls, in case the mutex's cost is scale-dependent (bigger map
    // lookups, longer critical sections) rather than contention-only.
    let alone = run_sweep(1, files, opts);
    let c8 = run_sweep(8, files, opts);
    let c16 = run_sweep(16, files, opts);
    let stress = if cfg!(debug_assertions) {
        run_sweep(16, 64, opts) // debug smoke: keep the suite short
    } else {
        run_sweep(16, 256, opts)
    };

    workers::POOL.disable();

    let slowdown = |r: &RunResult| (r.p50_us / alone.mean_us, r.max_us / alone.mean_us);

    println!(
        "{:<4} {:>8} {:>9} {:>9} {:>9} {:>9} {:>7} {:>7} {:>9} {:>9} {:>9} {:>9} {:>6} {:>8} {:>9} {:>6} {:>6}",
        "w",
        "wall_ms",
        "p50_us",
        "p99_us",
        "mean_us",
        "max_us",
        "cpu_ms",
        "search",
        "prepare",
        "dsfbObs",
        "dsfbPlan",
        "dsfbTrst",
        "queue%",
        "cand",
        "reachMB",
        "ratio",
        "exact"
    );
    for r in [&alone, &c8, &c16, &stress] {
        let (s50, smax) = slowdown(r);
        let ratio = r.logical_bytes as f64 / r.reachable_bytes.max(1) as f64;
        println!(
            "{:<4} {:>8.0} {:>9.0} {:>9.0} {:>9.0} {:>9.0} {:>7.0} {:>7.0} {:>9.0} {:>9.2} {:>9.2} {:>9.2} {:>5.1}% {:>8} {:>9.2} {:>6.2} {:>6} (med {:.1}x, max {:.1}x) families={}",
            r.writers,
            r.wall_ms,
            r.p50_us,
            r.p99_us,
            r.mean_us,
            r.max_us,
            r.useful_cpu_ms,
            r.search_ms,
            r.prepare_ms,
            r.dsfb_observe_ms,
            r.dsfb_plan_ms,
            r.dsfb_trust_ms,
            r.queue_share_pct,
            r.candidates,
            r.reachable_bytes as f64 / (1024.0 * 1024.0),
            ratio,
            if r.byte_exact { "ok" } else { "MISMATCH" },
            s50,
            smax,
            r.families
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",")
        );
    }

    // Correctness: every run read back byte-exactly, and the committed
    // logical accounting matched the input exactly (asserted inside
    // run_sweep). The backpressure bound: peak in-flight <= 8 x workers
    // with the single-request floor (the 64-extent read-back decode is
    // admitted by an idle pool and is its own lower bound).
    for r in [&alone, &c8, &c16, &stress] {
        assert!(r.byte_exact, "{} writers: read-back mismatch", r.writers);
        assert!(
            r.peak_in_flight <= 16 * 8 + 64,
            "{} writers: peak in-flight {} exceeded the backpressure bound + single-request floor",
            r.writers,
            r.peak_in_flight
        );
    }

    // The summary TSV for the evidence archive. The tool stamps the
    // observer identity (`DSFB_PROBE_MODE`) into the header row; the probe
    // itself is observer-agnostic (the attribution rule).
    let mut tsv = String::new();
    tsv.push_str(
        "mode\twriters\twall_ms\tp50_us\tp99_us\tmean_us\tmax_us\tuseful_cpu_ms\tsearch_ms\tprepare_ms\tdsfb_observe_ms\tdsfb_plan_ms\tdsfb_trust_ms\tqueue%\tpeak_in_flight\tcandidates\tlogical_bytes\treachable_bytes\tbyte_exact\tfamilies\n",
    );
    let mode = std::env::var("DSFB_PROBE_MODE").unwrap_or_else(|_| "unknown".into());
    for r in [&alone, &c8, &c16, &stress] {
        let families = r
            .families
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",");
        tsv.push_str(&format!(
            "{mode}\t{}\t{:.0}\t{:.0}\t{:.0}\t{:.0}\t{:.0}\t{:.0}\t{:.0}\t{:.0}\t{:.2}\t{:.2}\t{:.2}\t{:.1}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            r.writers,
            r.wall_ms,
            r.p50_us,
            r.p99_us,
            r.mean_us,
            r.max_us,
            r.useful_cpu_ms,
            r.search_ms,
            r.prepare_ms,
            r.dsfb_observe_ms,
            r.dsfb_plan_ms,
            r.dsfb_trust_ms,
            r.queue_share_pct,
            r.peak_in_flight,
            r.candidates,
            r.logical_bytes,
            r.reachable_bytes,
            if r.byte_exact { "ok" } else { "MISMATCH" },
            families
        ));
    }
    if let Ok(path) = std::env::var("DSFB_PROBE_OUT") {
        std::fs::write(&path, &tsv).expect("write probe summary");
        println!("probe summary written to {path}");
    }
}
