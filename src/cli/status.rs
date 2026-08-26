//! `entropyfs status <store-dir>`: store accounting and health summary
//! (§22, §42). Works when unmounted (reads raw); reports "mounted" when
//! the lock is held.
//!
//! # PURPOSE
//!
//! Print the store's health and accounting at a glance: mount state,
//! uuid / generation, physical capacity vs used vs free, logical bytes
//! across inodes, snapshot count, a full fsck health summary (with GC
//! reclaimable bytes), the Phase-9H physical reconciliation, and DSFB
//! statistics.
//!
//! # BOUNDARY
//!
//! KNOWS: `Store`'s public accounting accessors, `crate::fsck`, and
//! `crate::store::physical::physical_report`. NEVER KNOWS: any write
//! path. If the store is mounted, it reports the lock state and stops —
//! it must not open a mounted store read-write.
//!
//! # MODEL
//!
//! Try-lock first: if `ensure_unmounted` fails, the store is mounted and
//! `status` says so (reading a live store could observe a torn
//! mid-checkpoint state). Otherwise it opens the store read-only and
//! composes the accounting from four authorities: the store's own
//! counters (capacity/used/logical), the fsck report (health + leaks),
//! the physical reconciliation (independent of the derived index, Phase
//! 9H), and the DSFB stats.
//!
//! # KEY INVARIANTS
//!
//! - Mounted store → lock-state report only; never a read of a live
//!   store (the crash-court's torn states are the reason).
//! - The Phase-9H reconciliation is reported independently of the
//!   derived index so index-vs-physical drift is visible, not
//!   self-confirming.
//! - Bytes are always labeled (capacity/used/free, logical, reclaimable,
//!   live/dead-indexed/index-hidden/unindexed/overhead) and never mixed
//!   across unit types.

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
