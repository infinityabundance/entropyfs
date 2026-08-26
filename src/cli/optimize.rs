//! `entropyfs optimize <store>`: the background optimization pass (§16,
//! §44-H4).
//!
//! Scans every file extent, runs the full DSFB-guided search (all base
//! channels, universe negative control, reference-chain flattening), and
//! atomically replaces extents whose new representation is strictly
//! cheaper — after byte-exact validation (§32) and a CAS check (§25).
//!
//! # PURPOSE
//!
//! Expose the optimizer as a CLI: run the per-extent densification pass,
//! then the amortized shared-dictionary pass (Phase 9C) and the
//! amortized entropy-model pass (Phase 9G), printing the saved-bytes
//! accounting of each. The `--raw-only` / `--raw-rans` / `--no-dsfb`
//! flags are ablation gates so a store can be optimized under a
//! constrained pipeline for comparison.
//!
//! # BOUNDARY
//!
//! KNOWS: `OptimizeOptions` gates and the three background passes.
//! NEVER KNOWS: how any pass encodes; it only observes
//! `optimize_pass` stats. The command refuses to run on a mounted store
//! (`crate::fsck::ensure_unmounted`) — the background worker owns
//! optimization while mounted.
//!
//! # MODEL
//!
//! A linear pipeline over the store: per-extent pass → shared-dict pass →
//! model-bundle pass, each strict-cheaper (a rewrite only when the new
//! representation saves bytes after validation). Each pass reports
//! scanned / rewritten / saved bytes (persisted bytes, best-effort
//! estimate from the stats), and the shared/model passes self-gate on
//! their feature flags, so their output lines appear only when they did
//! work.
//!
//! # KEY INVARIANTS
//!
//! - Unmounted-only: running against a mounted store would race the
//!   background worker and the epoch write path.
//! - Every rewrite is strictly cheaper AND byte-exact validated (§32)
//!   and CAS-checked (§25) — this command never trades correctness for
//!   density.
//! - The three ablation flags compose: `--raw-only` / `--raw-rans`
//!   select a restricted pipeline; `--no-dsfb` disables plan ordering
//!   within the full pipeline.

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
    let store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;

    let mut options = OptimizeOptions::default();
    if args.raw_only {
        options = OptimizeOptions::raw_only();
    } else if args.raw_rans {
        options = OptimizeOptions::raw_rans();
    }
    if args.no_dsfb {
        options.allow_dsfb_ranking = false;
    }

    let stats = optimize_pass(&store, options, None, None).map_err(|e| e.to_string())?;
    println!(
        "optimize: scanned {} extents, rewrote {}, saved ~{} persisted bytes (stale {}, no-gain {}, errors {})",
        stats.scanned,
        stats.rewritten,
        stats.saved_bytes,
        stats.stale_skips,
        stats.no_gain,
        stats.errors
    );
    // Phase-9C: the shared amortized dictionary pass (directory anchors).
    let shared = crate::optimizer::background::shared_dict_pass(&store, options, None)
        .map_err(|e| e.to_string())?;
    if shared.rewritten > 0 || shared.scanned > 0 {
        println!(
            "optimize: shared-dict pass scanned {} extents, rewrote {}, saved ~{} persisted bytes (no-gain {}, errors {})",
            shared.scanned, shared.rewritten, shared.saved_bytes, shared.no_gain, shared.errors
        );
    }
    // Phase-9G: the amortized entropy-model pass (directory cohort models).
    let models = crate::optimizer::background::model_bundle_pass(&store, options, None)
        .map_err(|e| e.to_string())?;
    if models.rewritten > 0 || models.scanned > 0 {
        println!(
            "optimize: model-bundle pass scanned {} extents, rewrote {}, saved ~{} persisted bytes (no-gain {}, errors {})",
            models.scanned, models.rewritten, models.saved_bytes, models.no_gain, models.errors
        );
    }
    Ok(())
}
