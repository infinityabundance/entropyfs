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
    /// Emit the versioned JSON DTO (Phase 12E.6; schema_version 1).
    #[arg(long)]
    pub json: bool,
}

/// Run status.
pub fn run(args: &StatusArgs) -> Result<(), String> {
    // Try-lock: a missing store is an error ("run entropyfs mkfs"); a
    // held lock means the store is mounted — report it (reading a live
    // store could observe a torn mid-checkpoint state).
    if let Err(m) = crate::fsck::ensure_unmounted(&args.store) {
        if m.starts_with("no entropyfs store") {
            return Err(m);
        }
        if args.json {
            let j = crate::cli::json::StatusJson {
                schema_version: 1,
                state: "mounted".into(),
                store: args.store.display().to_string(),
                uuid: String::new(),
                generation: 0,
                format: crate::engine::FormatInfo {
                    format_major: 0,
                    format_minor: 0,
                    compat: 0,
                    ro_compat: 0,
                    incompat: 0,
                    io_backend: String::new(),
                },
                physical_capacity_bytes: 0,
                physical_used_bytes: 0,
                physical_free_bytes: 0,
                logical_bytes: 0,
                inode_count: 0,
                snapshot_count: 0,
                fsck: crate::cli::json::StatusFsck {
                    status: "unknown".into(),
                    errors: 0,
                    warnings: 0,
                    leaked_bytes: 0,
                    leaked_objects: 0,
                },
                physical: None,
                dsfb: crate::cli::json::StatusDsfb {
                    tracked_chunks: 0,
                    steps: 0,
                    drift_events: 0,
                    slew_events: 0,
                    narrowed_searches: 0,
                },
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&j).unwrap_or_else(|_| "{}".into())
            );
            return Ok(());
        }
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
    // Phase-9H: physical reconciliation (independent of the derived index).
    let phys = crate::store::physical::physical_report(&store).ok();
    let dsfb = store.dsfb_stats();
    let uuid_hex = crate::cli::mkfs::hex_encode(&store.current_root().uuid);
    let bits = store.feature_bits();
    if args.json {
        let j = crate::cli::json::StatusJson {
            schema_version: 1,
            state: "ok".into(),
            store: args.store.display().to_string(),
            uuid: uuid_hex,
            generation: store.generation(),
            format: crate::engine::FormatInfo {
                format_major: store.current_root().format_major,
                format_minor: store.current_root().format_minor,
                compat: bits.compat,
                ro_compat: bits.ro_compat,
                incompat: bits.incompat,
                io_backend: store.config().io_backend.name().to_string(),
            },
            physical_capacity_bytes: capacity,
            physical_used_bytes: used,
            physical_free_bytes: capacity.saturating_sub(used),
            logical_bytes: logical,
            inode_count: inodes.len(),
            snapshot_count: snapshots.len(),
            fsck: crate::cli::json::StatusFsck {
                status: if report.is_clean() { "clean" } else { "issues" }.into(),
                errors: report.error_count(),
                warnings: report.warning_count(),
                leaked_bytes: report.leaked_bytes,
                leaked_objects: report.leaked_objects,
            },
            physical: phys.map(|p| crate::engine::PhysicalMetrics {
                live_bytes: p.live_bytes,
                dead_indexed_bytes: p.dead_indexed_bytes,
                index_hidden_bytes: p.index_hidden_bytes,
                unindexed_bytes: p.unindexed_bytes,
                torn_bytes: p.torn_bytes,
                zero_padding_bytes: p.zero_padding_bytes,
                format_overhead_bytes: p.format_overhead_bytes,
                unexplained_bytes: p.unexplained(),
            }),
            dsfb: crate::cli::json::StatusDsfb {
                tracked_chunks: dsfb.tracked_chunks,
                steps: dsfb.steps,
                drift_events: dsfb.drift_events,
                slew_events: dsfb.slew_events,
                narrowed_searches: dsfb.narrowed_searches,
            },
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&j).unwrap_or_else(|_| "{}".into())
        );
        return Ok(());
    }
    println!("store:          {}", args.store.display());
    println!("uuid:           {uuid_hex}");
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
    if let Some(phys) = phys {
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
