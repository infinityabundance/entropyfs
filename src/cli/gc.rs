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
}

/// Run gc.
pub fn run(args: &GcArgs) -> Result<(), String> {
    crate::fsck::ensure_unmounted(&args.store)?;
    let config = StoreConfig::default();
    let mut store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;
    let before = crate::store::gc::unreachable_bytes(&store).map_err(|e| e.to_string())?;
    let reclaimed =
        crate::store::gc::collect(&mut store, &CrashHooks::none()).map_err(|e| e.to_string())?;
    let after = crate::store::gc::unreachable_bytes(&store).map_err(|e| e.to_string())?;
    println!("unreachable before: {before} bytes");
    println!("reclaimed: {reclaimed} bytes");
    println!("unreachable after: {after} bytes");
    Ok(())
}
