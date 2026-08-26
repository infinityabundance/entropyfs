//! `entropyfs fsck <store>` and `entropyfs scrub <store>`: independent
//! validation (§34). scrub enables the full materialized-hash chain.
//!
//! # PURPOSE
//!
//! Expose the `crate::fsck` engine as CLI commands: `fsck` validates the
//! store's structure and accounting; `scrub` additionally verifies every
//! extent by materializing it and checking the hash chain. Both print
//! the report and exit non-zero (with the error count) when the store is
//! not clean.
//!
//! # BOUNDARY
//!
//! KNOWS: `crate::fsck::FsckOptions` and the two flags it exposes here
//! (`--repair` for safe repairs such as torn segment tails; scrub's full
//! materialized verification). NEVER KNOWS: how the store is written or
//! how recovery works — the validator is independent by design.
//!
//! # MODEL
//!
//! Two configurations of one engine: `run_fsck` uses the default
//! structural/accounting checks; `run_scrub` sets
//! `verify_materialized: true` (the full chain: descriptors → objects →
//! materialized bytes → hash). Both refuse a mounted store
//! (`ensure_unmounted`) and both treat any error or warning as a
//! non-clean result.
//!
//! # KEY INVARIANTS
//!
//! - fsck is read-only unless `--repair` is given, and even then the
//!   only repairs are safe ones (torn segment tails — the same
//!   truncation `open_segment` performs).
//! - scrub is the strictly stronger check: `verify_materialized` implies
//!   the full structural checks, never the reverse.
//! - Exit status reflects the report: `Err` with the error count
//!   propagates a non-zero exit to the shell, so scripts cannot
//!   silently pass a dirty store.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use crate::fsck::{FsckOptions, fsck};

/// Options for fsck.
#[derive(Debug, Clone, clap::Args)]
pub struct FsckArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
    /// Repair safe issues (torn segment tails).
    #[arg(long)]
    pub repair: bool,
}

/// Options for scrub.
#[derive(Debug, Clone, clap::Args)]
pub struct ScrubArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
    /// Repair safe issues.
    #[arg(long)]
    pub repair: bool,
}

/// Run fsck.
pub fn run_fsck(args: &FsckArgs) -> Result<(), String> {
    crate::fsck::ensure_unmounted(&args.store)?;
    let options = FsckOptions {
        repair_torn_tails: args.repair,
        ..Default::default()
    };
    let report = fsck(&args.store, &options).map_err(|e| e.to_string())?;
    print!("{}", report.render());
    if report.is_clean() {
        println!("fsck: OK");
        Ok(())
    } else {
        println!(
            "fsck: {} errors, {} warnings",
            report.error_count(),
            report.warning_count()
        );
        Err("fsck found issues".into())
    }
}

/// Run scrub (full materialized verification).
pub fn run_scrub(args: &ScrubArgs) -> Result<(), String> {
    crate::fsck::ensure_unmounted(&args.store)?;
    let options = FsckOptions {
        verify_materialized: true,
        repair_torn_tails: args.repair,
        ..Default::default()
    };
    let report = fsck(&args.store, &options).map_err(|e| e.to_string())?;
    print!("{}", report.render());
    if report.is_clean() {
        println!("scrub: OK");
        Ok(())
    } else {
        println!(
            "scrub: {} errors, {} warnings",
            report.error_count(),
            report.warning_count()
        );
        Err("scrub found issues".into())
    }
}
