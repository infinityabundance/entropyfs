//! `entropyfs fsck <store>` and `entropyfs scrub <store>`: independent
//! validation (§34). scrub enables the full materialized-hash chain.

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
