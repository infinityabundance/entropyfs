//! `entropyfs gc <store>`: reachability garbage collection (§21).

#![forbid(unsafe_code)]

use std::path::PathBuf;

use crate::store::transaction::CrashHooks;
use crate::store::{Store, StoreConfig};

/// Options for gc.
#[derive(Debug, Clone, clap::Args)]
pub struct GcArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
    /// Full compaction (Phase-9H): compact every segment so the physical
    /// backing converges to the reachable persistent state plus bounded
    /// format overhead. Idempotent; a second run reclaims ~nothing.
    #[arg(long)]
    pub compact: bool,
}

/// Run gc.
pub fn run(args: &GcArgs) -> Result<(), String> {
    crate::fsck::ensure_unmounted(&args.store)?;
    let config = StoreConfig::default();
    let store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;
    let before = crate::store::gc::unreachable_bytes(&store).map_err(|e| e.to_string())?;
    let reclaimed = if args.compact {
        crate::store::gc::compact_full(&store, &CrashHooks::none()).map_err(|e| e.to_string())?
    } else {
        crate::store::gc::collect(&store, &CrashHooks::none()).map_err(|e| e.to_string())?
    };
    let after = crate::store::gc::unreachable_bytes(&store).map_err(|e| e.to_string())?;
    println!("unreachable before: {before} bytes");
    println!("reclaimed: {reclaimed} bytes");
    println!("unreachable after: {after} bytes");
    if args.compact {
        // Phase-9H: the physical reconciliation after full compaction.
        let report = crate::store::physical::physical_report(&store).map_err(|e| e.to_string())?;
        print!("{}", crate::store::physical::render(&report));
    }
    Ok(())
}
