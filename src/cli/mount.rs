//! `entropyfs mount <store> <mountpoint>`: the FUSE daemon.
//!
//! Preflights the environment (§47), opens the store, spawns the fuser
//! session, and parks until the mount is unmounted or SIGINT/SIGTERM
//! arrives (then unmounts cleanly).
//!
//! # PURPOSE
//!
//! Translate the CLI into a [`StoreConfig`] (foreground policy, io
//! backend, worker pool), open the store, and hand it to
//! [`crate::fuse::mount::mount`]; then own the daemon lifecycle: park,
//! react to signals, unmount-and-join, and stop the Phase-11E worker
//! pool so the process exits cleanly.
//!
//! # BOUNDARY
//!
//! KNOWS: the CLI flags and how each maps onto [`StoreConfig`] /
//! [`MountParams`]. NEVER KNOWS: FUSE request handling, the store's
//! internals, or any persistence logic — those live in `crate::fuse` and
//! `crate::store`. The daemon must not read or write store bytes itself.
//!
//! # MODEL
//!
//! One process, one mount session. `run` blocks until the session's
//! guard finishes (unmount elsewhere) or a signal flips the `AtomicBool`
//! stop flag, polling every 100 ms; then `umount_and_join` performs the
//! clean teardown. The worker pool (Phase-11E, the mount default since
//! the mounted-FUSE court sealed it) is
//! disabled only after the session ends so no worker outlives the store.
//!
//! # KEY INVARIANTS
//!
//! - Unknown `--foreground` / `--io-backend` values are rejected before
//!   any store is opened (fail fast, no partial state).
//! - `--read-only` and `--allow-other` are passed through verbatim to
//!   the session parameters; the store open itself is unaffected.
//! - `--no-background-optimize` disables the idle densifier; the default
//!   (on) matches the measured A8 ladder step.
//! - Signal handling is best-effort (`ctrlc`); the loop also exits when
//!   the session guard finishes, so a killed daemon still unmounts.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::fuse::mount::{MountParams, mount as do_mount};
use crate::store::{Store, StoreConfig};

/// Options for mount.
#[derive(Debug, Clone, clap::Args)]
pub struct MountArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
    /// Mountpoint.
    #[arg(value_name = "MOUNTPOINT")]
    pub mountpoint: PathBuf,
    /// Read-only mount.
    #[arg(long)]
    pub read_only: bool,
    /// Allow other users (requires fuse allow_other config).
    #[arg(long)]
    pub allow_other: bool,
    /// Event loop threads (v1 defaults to 1).
    #[arg(long, default_value_t = 1)]
    pub threads: usize,
    /// FUSE filesystem name.
    #[arg(long, default_value = "entropyfs")]
    pub fs_name: String,
    /// Disable the background optimizer worker (densifies cold data while
    /// the mount is idle; default on).
    #[arg(long)]
    pub no_background_optimize: bool,
    /// Phase-10A: dump FUSE request + write-path phase instrumentation to
    /// this file when the daemon unmounts (diagnostic; the perf court
    /// reads it).
    #[arg(long, value_name = "PATH")]
    pub stats_file: Option<PathBuf>,
    /// Phase-10B/12C-1 foreground representation policy: how much search
    /// CPU the write path spends per chunk. full = every family (pre-10B);
    /// cheap = probe first, high-entropy chunks go dedup+ZERO/FILL+RAW;
    /// focused = the Phase-12C-1 adaptive budget (the entropy probe + the
    /// semantic class-prior rANS deferral); pressure = the Phase-12C-1-2
    /// pressure-aware shape (focused + the worker-pool pressure gate with
    /// the hysteresis band enter 0.80 / leave 0.60, the configurational
    /// deferral, and the 1 GiB debt cap — the sealed 12C-1-2 evidence
    /// parameters); raw = hash->CAS->RAW (the background optimizer still
    /// densifies).
    #[arg(long, value_name = "MODE", default_value = "full")]
    pub foreground: String,
    /// Phase-10F storage transport (sync reference path | uring).
    #[arg(long, value_name = "BACKEND", default_value = "sync")]
    pub io_backend: String,
    /// io_uring submission queue capacity (UringIo only).
    #[arg(long, default_value_t = 256)]
    pub io_uring_entries: u32,
    /// Phase-11E worker pool: route the search/decode work through the
    /// persistent FAIR worker pool with N threads (capacity 8x N) instead
    /// of the 11C batch semaphore. The mounted-FUSE 11E court sealed the
    /// pool as the MOUNT DEFAULT (pool-16: parallel write +14%, latency-
    /// battery wall −26%, p95 −39%, p99 −48%, CPU +2.8%, crash/fsck/
    /// readback clean at 1/4/8/16 FUSE threads —
    /// `evidence/performance/worker-pool-mount-court-*/`). Without the
    /// flag the pool runs with `available_parallelism()` workers (the
    /// adopted pool-16 config on this machine); `--no-worker-pool` forces
    /// the 11C semaphore (the fallback).
    #[arg(long, value_name = "N")]
    pub worker_pool: Option<usize>,
    /// Force the Phase-11C batch semaphore (disable the Phase-11E worker
    /// pool, the mount default since the mounted-FUSE court sealed it).
    #[arg(long)]
    pub no_worker_pool: bool,
}

/// Run the mount daemon.
pub fn run(args: &MountArgs) -> Result<(), String> {
    let config = StoreConfig {
        foreground: match args.foreground.as_str() {
            "full" => crate::optimizer::foreground::ForegroundPolicy::full(),
            "cheap" => crate::optimizer::foreground::ForegroundPolicy::cheap(),
            "focused" => crate::optimizer::foreground::ForegroundPolicy::focused(),
            "pressure" => crate::optimizer::foreground::ForegroundPolicy {
                // Phase 12C-1-2: the evidence-backed pressure-aware shape
                // (the sealed court's hysteresis band against flap, the
                // configurational deferral that closed the container-layers
                // wall gate, and a generous starvation cap the operator can
                // tighten — the debt bound mechanism the court pinned).
                pressure_enter: 0.80,
                pressure_leave: 0.60,
                pressure_defer_configurational: true,
                pressure_max_deferred_bytes: 1024 * 1024 * 1024,
                ..crate::optimizer::foreground::ForegroundPolicy::focused()
            },
            "raw" => crate::optimizer::foreground::ForegroundPolicy::raw_only(),
            other => {
                return Err(format!(
                    "unknown --foreground mode {other:?} (expected full | cheap | focused | pressure | raw)"
                ));
            }
        },
        io_backend: crate::store::io::IoBackendKind::parse(&args.io_backend)?,
        io_uring_entries: args.io_uring_entries,
        ..StoreConfig::default()
    };
    let store =
        Store::open(&args.store, &config).map_err(|e| crate::cli::errors::open(&args.store, &e))?;
    // Phase-11E default flip (sealed by the mounted-FUSE court): the pool
    // is ON by default with available_parallelism() workers; the semaphore
    // remains reachable via --no-worker-pool (the fallback) and via
    // --worker-pool N for an explicit size.
    let pool_threads = if args.no_worker_pool {
        None
    } else {
        Some(args.worker_pool.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|p| p.get())
                .unwrap_or(4)
        }))
    };
    let params = MountParams {
        store_dir: args.store.clone(),
        mountpoint: args.mountpoint.clone(),
        read_only: args.read_only,
        allow_other: args.allow_other,
        threads: args.threads,
        fs_name: args.fs_name.clone(),
        background_optimize: !args.no_background_optimize,
        stats_file: args.stats_file.clone(),
        worker_pool_threads: pool_threads,
    };
    let session = do_mount(&params, store)
        .map_err(|e| crate::cli::errors::fuse_mount(&args.mountpoint, &e.to_string()))?;
    println!(
        "entropyfs mounted: {} -> {} (pid {})",
        params.store_dir.display(),
        params.mountpoint.display(),
        std::process::id()
    );

    // Park until unmounted elsewhere or a termination signal arrives.
    let stop = Arc::new(AtomicBool::new(false));
    let stop_handler = Arc::clone(&stop);
    ctrlc::set_handler(move || {
        stop_handler.store(true, Ordering::SeqCst);
    })
    .map_err(|e| format!("signal handler: {e}"))?;

    while !session.guard.is_finished() && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
    session
        .umount_and_join()
        .map_err(|e| format!("unmount/join: {e}"))?;
    // Phase-11E: stop the pool's worker threads (parked idle during the
    // session; joined here so the process exits cleanly). A no-op when the
    // pool was never enabled.
    crate::store::workers::POOL.disable();
    Ok(())
}
