//! `entropyfs status <store-dir>`: store accounting and health summary
//! (§22, §42). Works when unmounted (reads raw); reports "mounted" when
//! the lock is held.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use crate::fsck::FsckOptions;
use crate::store::{Store, StoreConfig};

/// Options for status.
#[derive(Debug, Clone, clap::Args)]
pub struct StatusArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
}

/// Run status.
pub fn run(args: &StatusArgs) -> Result<(), String> {
    // Try-lock: if the store is mounted, report it.
    if crate::fsck::ensure_unmounted(&args.store).is_err() {
        println!("state: mounted (lock held)");
        return Ok(());
    }
    let config = StoreConfig::default();
    let store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;
    let capacity = store.physical_capacity();
    let used = store.physical_used();
    let logical = store.logical_bytes().unwrap_or(0);
    let snapshots = store.list_snapshots().unwrap_or_default();
    let inodes = store.all_inodes().unwrap_or_default();
    // Full fsck accounting for the health summary.
    let report =
        crate::fsck::fsck(&args.store, &FsckOptions::default()).map_err(|e| e.to_string())?;
    println!("store:          {}", args.store.display());
    println!(
        "uuid:           {}",
        crate::cli::mkfs::hex_encode(&store.current_root().uuid)
    );
    println!("generation:     {}", store.generation());
    println!(
        "physical:       {} bytes capacity, {} used ({} free)",
        capacity,
        used,
        capacity.saturating_sub(used)
    );
    println!(
        "logical:        {} bytes across {} inodes",
        logical,
        inodes.len()
    );
    println!("snapshots:      {}", snapshots.len());
    println!(
        "fsck:           {} ({} errors, {} warnings)",
        if report.is_clean() { "clean" } else { "ISSUES" },
        report.error_count(),
        report.warning_count()
    );
    println!(
        "gc reclaimable: {} bytes ({} objects)",
        report.leaked_bytes, report.leaked_objects
    );
    // Phase-9H: physical reconciliation (independent of the derived index).
    if let Ok(phys) = crate::store::physical::physical_report(&store) {
        println!(
            "physical:       {} B files = {} B live + {} B dead-indexed + {} B index-hidden + {} B unindexed + {} B overhead ({} unexplained)",
            phys.file_bytes,
            phys.live_bytes,
            phys.dead_indexed_bytes,
            phys.index_hidden_bytes,
            phys.unindexed_bytes,
            phys.format_overhead_bytes
                .saturating_add(phys.torn_bytes)
                .saturating_add(phys.zero_padding_bytes),
            phys.unexplained()
        );
    }
    let dsfb = store.dsfb_stats();
    println!(
        "dsfb:           {} chunks tracked, {} steps, {} drift, {} slew, {} narrowed",
        dsfb.tracked_chunks,
        dsfb.steps,
        dsfb.drift_events,
        dsfb.slew_events,
        dsfb.narrowed_searches
    );
    Ok(())
}
