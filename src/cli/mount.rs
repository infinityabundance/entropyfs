//! `entropyfs mount <store> <mountpoint>`: the FUSE daemon.
//!
//! Preflights the environment (§47), opens the store, spawns the fuser
//! session, and parks until the mount is unmounted or SIGINT/SIGTERM
//! arrives (then unmounts cleanly).

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
    /// Phase-10B foreground representation policy: how much search CPU
    /// the write path spends per chunk. full = every family (pre-10B);
    /// cheap = probe first, high-entropy chunks go dedup+ZERO/FILL+RAW;
    /// raw = hash->CAS->RAW (the background optimizer still densifies).
    #[arg(long, value_name = "MODE", default_value = "full")]
    pub foreground: String,
}

/// Run the mount daemon.
pub fn run(args: &MountArgs) -> Result<(), String> {
    let mut config = StoreConfig::default();
    config.foreground = match args.foreground.as_str() {
        "full" => crate::optimizer::foreground::ForegroundPolicy::full(),
        "cheap" => crate::optimizer::foreground::ForegroundPolicy::cheap(),
        "raw" => crate::optimizer::foreground::ForegroundPolicy::raw_only(),
        other => {
            return Err(format!(
                "unknown --foreground mode {other:?} (expected full | cheap | raw)"
            ));
        }
    };
    let store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;
    let params = MountParams {
        store_dir: args.store.clone(),
        mountpoint: args.mountpoint.clone(),
        read_only: args.read_only,
        allow_other: args.allow_other,
        threads: args.threads,
        fs_name: args.fs_name.clone(),
        background_optimize: !args.no_background_optimize,
        stats_file: args.stats_file.clone(),
    };
    let session = do_mount(&params, store).map_err(|e| e.to_string())?;
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
    Ok(())
}
