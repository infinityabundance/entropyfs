//! Phase-11B: the write-path request reconciliation (the stacked accounting
//! table). Diagnostic, not sealed evidence — the sealed court is
//! `tools/recon-court.sh` at the FUSE layer.
//!
//! Drives the EPOCH write path (the FUSE production path) and the direct
//! transactional path with 1/2/4/8/16 writer threads on distinct inodes,
//! then prints the reconciliation for each thread count:
//!
//! ```text
//! request latency = Σ exclusive phases + residual
//! ```
//!
//! The court asserts the identity: no partition row may be nested inside
//! another (overlap ⇒ residual < 0) and the residual (FUSE/scheduler/other
//! plus the un-instrumented store gaps) must be a small share of the
//! request time — "essentially all wall time is accounted for". The
//! per-request residual must also be non-negative individually, not just in
//! aggregate.
//!
//! This is the performance equivalent of Phase 9H's physical byte
//! reconciliation: `file bytes = live + dead + hidden + padding + format +
//! unexplained` becomes `request latency = useful work + lock wait +
//! kernel/FUSE + storage wait + scheduler + unexplained`.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Instant;

use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

/// The residual share (of request time) the reconciliation court tolerates
/// on the write path before it declares time "unaccounted for". 15% is a
/// generous bar for a debug-mode CI run; the sealed court runs release and
/// reports the real number.
const MAX_RESIDUAL_SHARE: f64 = 0.15;

fn create_store(dir: &TempDir) -> Arc<Store> {
    let cfg = StoreConfig {
        segment_size: 128 * 1024 * 1024,
        ..Default::default()
    };
    Arc::new(Store::create(dir.path(), &cfg, [0x53; 16]).unwrap())
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

/// Create `n` files in the root directory.
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

/// One thread count of the sweep: `files` files written concurrently by
/// `threads` writers (each writer owns `files / threads` distinct inodes and
/// writes `rounds` sequential 1 MiB epochs per inode), then the reconciled
/// table is printed and the identity asserted.
fn sweep_threads(
    store: &Arc<Store>,
    label: &str,
    threads: usize,
    files: &[u64],
    rounds: usize,
) -> f64 {
    store.perf().clear();
    let data = deterministic_noise(65536 * 16, 0xabcd_0000 ^ threads as u64);
    let options = OptimizeOptions::default();
    let fg = store.foreground_policy();
    let t0 = Instant::now();
    std::thread::scope(|s| {
        for w in 0..threads {
            let store = Arc::clone(store);
            let data = &data;
            let files = files;
            s.spawn(move || {
                let mut i = w;
                while i < files.len() {
                    let ino = files[i];
                    for r in 0..rounds {
                        // The FUSE production path: epoch writes (log append
                        // + ack; the trees merge at the checkpoint).
                        store
                            .epoch_write(
                                ino,
                                (r as u64) * data.len() as u64,
                                data,
                                options,
                                fg,
                                &CrashHooks::none(),
                            )
                            .unwrap();
                    }
                    i += threads;
                }
            });
        }
    });
    let wall = t0.elapsed().as_secs_f64();

    // The identity court.
    let results = store.perf().results();
    for r in &results {
        if r.residual_ns < 0 {
            eprintln!(
                "{label} threads={threads}: overlapping request (residual {:.1} µs):",
                -r.residual_ns as f64 / 1e3
            );
            for (p, ns) in &r.phases {
                eprintln!("    {p:<26} {:>10.3} ms", *ns as f64 / 1e6);
            }
            panic!(
                "{label} threads={threads}: request {}/{} residual {:.1} µs < 0 — a \
                 partition row was nested inside another (double count)",
                r.name,
                r.total_ns,
                -r.residual_ns as f64 / 1e3,
            );
        }
    }
    let rec = store.perf().reconcile();
    assert!(
        !rec.overlap,
        "{label} threads={threads}: aggregate overlap (residual {:.2} ms)",
        rec.residual_ms
    );
    if rec.residual_share >= MAX_RESIDUAL_SHARE {
        // Diagnostics: where does the unaccounted time sit? Show the
        // request whose residual share is the largest.
        let results = store.perf().results();
        let mut worst: Option<&crate::perf::RequestResult> = None;
        for r in results.iter() {
            match worst {
                None => worst = Some(r),
                Some(w) => {
                    if r.residual_ns as f64 / r.total_ns.max(1) as f64
                        > w.residual_ns as f64 / w.total_ns.max(1) as f64
                    {
                        worst = Some(r);
                    }
                }
            }
        }
        if let Some(w) = worst {
            eprintln!(
                "{label} threads={threads}: worst request total={:.3} ms residual={:.3} ms ({:.1}%):",
                w.total_ns as f64 / 1e6,
                w.residual_ns as f64 / 1e6,
                w.residual_ns as f64 / w.total_ns.max(1) as f64 * 100.0
            );
            for (p, ns) in &w.phases {
                eprintln!("    {p:<26} {:>10.3} ms", *ns as f64 / 1e6);
            }
        }
    }
    assert!(
        rec.residual_share < MAX_RESIDUAL_SHARE,
        "{label} threads={threads}: residual {:.1}% of request time exceeds \
         the {:.0}% bar — wall time is NOT accounted for",
        rec.residual_share * 100.0,
        MAX_RESIDUAL_SHARE * 100.0,
    );

    println!("\n-- {label} threads={threads}  wall={wall:.3} s --");
    print!("{}", store.perf().render_reconciled());
    wall
}

#[test]
fn print_write_path_reconciliation() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let files = create_files(&store, 64);
    println!("\n==== Phase-11B write-path reconciliation (epoch path) ====");
    let mut walls = Vec::new();
    for t in [1usize, 2, 4, 8, 16] {
        walls.push(sweep_threads(&store, "epoch_write", t, &files, 4));
    }
    let _ = walls;
    // Byte-exactness after all of it: the epoch path must have persisted
    // every write (checkpoint + fsck).
    store.epoch_checkpoint(&CrashHooks::none()).unwrap();
    for (i, ino) in files.iter().enumerate() {
        let out = store.read_file(*ino, 0, 4 * 65536 * 16).unwrap();
        assert_eq!(
            out.len(),
            4 * 65536 * 16,
            "file {i} full length after the sweep"
        );
    }
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

/// The direct transactional path (`write_region_with_fg`) gets the same
/// treatment: it is the non-epoch write path (CLI, recovery, tools), and
/// its reconciliation must hold too.
#[test]
fn print_transactional_write_reconciliation() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let files = create_files(&store, 32);
    println!("\n==== Phase-11B write-path reconciliation (transactional path) ====");
    for t in [1usize, 4, 16] {
        store.perf().clear();
        let data = deterministic_noise(65536 * 8, 0x5eed_0000 ^ t as u64);
        let options = OptimizeOptions::default();
        let t0 = Instant::now();
        std::thread::scope(|s| {
            for w in 0..t {
                let store = Arc::clone(&store);
                let data = &data;
                let files = &files;
                s.spawn(move || {
                    let mut i = w;
                    while i < files.len() {
                        store
                            .write_region_with_fg(
                                files[i],
                                0,
                                data,
                                options,
                                store.foreground_policy(),
                            )
                            .unwrap();
                        i += t;
                    }
                });
            }
        });
        let wall = t0.elapsed().as_secs_f64();
        for r in store.perf().results() {
            if r.residual_ns < 0 {
                eprintln!(
                    "tx threads={t}: overlapping request (residual {:.1} µs):",
                    -r.residual_ns as f64 / 1e3
                );
                for (p, ns) in &r.phases {
                    eprintln!("    {p:<26} {:>10.3} ms", *ns as f64 / 1e6);
                }
                panic!(
                    "tx threads={t}: request {}/{} residual < 0",
                    r.name, r.total_ns
                );
            }
        }
        let rec = store.perf().reconcile();
        assert!(!rec.overlap, "tx threads={t}: overlap");
        assert!(
            rec.residual_share < MAX_RESIDUAL_SHARE,
            "tx threads={t}: residual {:.1}% too large",
            rec.residual_share * 100.0
        );
        println!("\n-- write_region threads={t}  wall={wall:.3} s --");
        print!("{}", store.perf().render_reconciled());
    }
}
