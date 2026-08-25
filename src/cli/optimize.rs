//! `entropyfs optimize <store>`: the background optimization pass (§16,
//! §44-H4).
//!
//! Scans every file extent, runs the full DSFB-guided search (all base
//! channels, universe negative control, reference-chain flattening), and
//! atomically replaces extents whose new representation is strictly
//! cheaper — after byte-exact validation (§32) and a CAS check (§25).

#![forbid(unsafe_code)]

use std::path::PathBuf;

use crate::optimizer::background::optimize_pass;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::{Store, StoreConfig};

/// Options for optimize.
#[derive(Debug, Clone, clap::Args)]
pub struct OptimizeArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
    /// Ablation: RAW only (no dedup, no structure, no rANS, no bases).
    #[arg(long)]
    pub raw_only: bool,
    /// Ablation: RAW + rANS only.
    #[arg(long)]
    pub raw_rans: bool,
    /// Disable the DSFB plan ordering (evaluate everything, no budget).
    #[arg(long)]
    pub no_dsfb: bool,
}

/// Run optimize.
pub fn run(args: &OptimizeArgs) -> Result<(), String> {
    crate::fsck::ensure_unmounted(&args.store)?;
    let config = StoreConfig::default();
    let mut store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;

    let mut options = OptimizeOptions::default();
    if args.raw_only {
        options = OptimizeOptions::raw_only();
    } else if args.raw_rans {
        options = OptimizeOptions::raw_rans();
    }
    if args.no_dsfb {
        options.allow_dsfb_ranking = false;
    }

    let stats = optimize_pass(&mut store, options, None, None).map_err(|e| e.to_string())?;
    println!(
        "optimize: scanned {} extents, rewrote {}, saved ~{} persisted bytes (stale {}, no-gain {}, errors {})",
        stats.scanned,
        stats.rewritten,
        stats.saved_bytes,
        stats.stale_skips,
        stats.no_gain,
        stats.errors
    );
    Ok(())
}
