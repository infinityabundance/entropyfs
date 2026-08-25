//! `entropyfs snapshot create <store> <name>` and `entropyfs snapshots
//! <store>`: snapshot management (ADR-0007).

#![forbid(unsafe_code)]

use std::path::PathBuf;

use crate::store::transaction::CrashHooks;
use crate::store::{Store, StoreConfig};

/// Options for `snapshot create`.
#[derive(Debug, Clone, clap::Args)]
pub struct SnapshotCreateArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
    /// Snapshot name.
    #[arg(value_name = "NAME")]
    pub name: String,
}

/// Options for `snapshot delete`.
#[derive(Debug, Clone, clap::Args)]
pub struct SnapshotDeleteArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
    /// Snapshot name.
    #[arg(value_name = "NAME")]
    pub name: String,
}

/// Options for `snapshot restore`.
#[derive(Debug, Clone, clap::Args)]
pub struct SnapshotRestoreArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
    /// Snapshot name.
    #[arg(value_name = "NAME")]
    pub name: String,
}

/// Options for `snapshots` (list).
#[derive(Debug, Clone, clap::Args)]
pub struct SnapshotsArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
}

/// Create a snapshot.
pub fn run_create(args: &SnapshotCreateArgs) -> Result<(), String> {
    let config = StoreConfig::default();
    let store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;
    let entry = store
        .create_snapshot(args.name.as_bytes(), &CrashHooks::none())
        .map_err(|e| e.to_string())?;
    println!(
        "snapshot '{}' created (root {})",
        args.name,
        crate::cli::mkfs::hex_encode(entry.root_id.as_bytes())
    );
    Ok(())
}

/// List snapshots.
pub fn run_list(args: &SnapshotsArgs) -> Result<(), String> {
    let config = StoreConfig::default();
    let store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;
    let snaps = store.list_snapshots().map_err(|e| e.to_string())?;
    if snaps.is_empty() {
        println!("no snapshots");
        return Ok(());
    }
    for (name, entry) in snaps {
        println!(
            "{:<32} root {}",
            String::from_utf8_lossy(&name),
            crate::cli::mkfs::hex_encode(entry.root_id.as_bytes())
        );
    }
    Ok(())
}

/// Delete a snapshot.
pub fn run_delete(args: &SnapshotDeleteArgs) -> Result<(), String> {
    crate::fsck::ensure_unmounted(&args.store)?;
    let config = StoreConfig::default();
    let store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;
    let present = store
        .delete_snapshot(args.name.as_bytes(), &CrashHooks::none())
        .map_err(|e| e.to_string())?;
    if present {
        println!("snapshot '{}' deleted", args.name);
    } else {
        println!("no such snapshot '{}'", args.name);
    }
    Ok(())
}

/// Restore (roll back to) a snapshot.
pub fn run_restore(args: &SnapshotRestoreArgs) -> Result<(), String> {
    crate::fsck::ensure_unmounted(&args.store)?;
    let config = StoreConfig::default();
    let store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;
    store
        .restore_snapshot(args.name.as_bytes(), &CrashHooks::none())
        .map_err(|e| e.to_string())?;
    println!("snapshot '{}' restored", args.name);
    Ok(())
}
