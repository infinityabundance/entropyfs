//! `entropyfs evidence-manifest <out.json>` (Phase 12E.5): capture the
//! machine-readable reproducibility manifest every sealed evidence
//! directory must contain.
//!
//! # PURPOSE
//!
//! Courts and campaigns run in bash; this command gives them a one-call
//! way to write the [`EvidenceManifest`] that turns an archive directory
//! into *sealed evidence*: version, revision, format, feature bits,
//! universe versions, transport, scheduler, kernel/arch/distro/host,
//! compiler, digest, timestamp. Filenames (`court-<ts>-<rev>/`) stay
//! human-readable; the manifest is the semantic authority.
//!
//! # BOUNDARY
//!
//! KNOWS: the environment captures and the build constants. NEVER KNOWS:
//! the store, the corpora, or any run result — it seals context, not
//! conclusions. The caller (court script) records the run's numbers
//! alongside.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use crate::evidence::manifest::EvidenceManifest;

/// Options for evidence-manifest.
#[derive(Debug, Clone, clap::Args)]
pub struct EvidenceManifestArgs {
    /// Output JSON path.
    #[arg(value_name = "OUT")]
    pub out: PathBuf,
    /// Store directory under test (for mount/device context).
    #[arg(long, default_value = ".")]
    pub store: PathBuf,
    /// Storage transport used by the run (`sync` | `uring`).
    #[arg(long, default_value = "sync")]
    pub io_backend: String,
    /// Worker scheduler used by the run (`semaphore` | `pool-<n>`).
    #[arg(long, default_value = "semaphore")]
    pub worker_scheduler: String,
    /// Court/driver schema version (the tool that produced the run).
    #[arg(long, default_value = "1")]
    pub court_schema_version: String,
    /// Immutable container image digest (empty for native runs).
    #[arg(long, default_value = "")]
    pub container_image_digest: String,
}

/// Run evidence-manifest.
pub fn run(args: &EvidenceManifestArgs) -> Result<(), String> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let m = EvidenceManifest::capture(
        &repo_root,
        &args.store,
        &args.io_backend,
        &args.worker_scheduler,
        &args.container_image_digest,
        &args.court_schema_version,
    );
    std::fs::write(&args.out, m.to_json())
        .map_err(|e| format!("write {}: {e}", args.out.display()))?;
    println!("evidence manifest written: {}", args.out.display());
    Ok(())
}
