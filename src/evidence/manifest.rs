//! Benchmark manifests (§42, §50): the full reproducibility record for a
//! benchmark run — environment, command, representation distribution, and
//! byte accounting. JSON for human-readable evidence.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::evidence::environment::Environment;

/// One benchmark run's reproducibility manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Manifest {
    /// Corpus casefile path + its hash.
    pub corpus: String,
    /// Corpus hash (BLAKE3 of the casefile bytes).
    pub corpus_hash: String,
    /// EntropyFS revision.
    pub revision: String,
    /// ryg-rans-rs revision (from Cargo.lock, best-effort).
    pub ryg_rans_revision: String,
    /// DSFB revision.
    pub dsfb_revision: String,
    /// Kernel version.
    pub kernel: String,
    /// Mount configuration.
    pub mount_config: String,
    /// CPU feature set (space-separated flags).
    pub cpu_features: String,
    /// The exact benchmark command.
    pub command: String,
    /// Representation distribution: tag → count.
    pub representation_distribution: BTreeMap<String, u64>,
    /// Logical bytes stored.
    pub logical_bytes: u64,
    /// Physical reachable bytes.
    pub physical_reachable_bytes: u64,
    /// Total backing-store bytes.
    pub total_backing_bytes: u64,
    /// Metadata bytes.
    pub metadata_bytes: u64,
    /// Model bytes.
    pub model_bytes: u64,
    /// Descriptor bytes.
    pub descriptor_bytes: u64,
    /// Residual bytes.
    pub residual_bytes: u64,
    /// Dedup savings (bytes not stored due to dedup).
    pub dedup_savings: u64,
    /// Throughput: encode MB/s.
    pub encode_mbps: f64,
    /// Throughput: materialize MB/s.
    pub materialize_mbps: f64,
    /// Cold-read MB/s.
    pub cold_read_mbps: f64,
    /// Warm-read MB/s.
    pub warm_read_mbps: f64,
}

impl Manifest {
    /// Render as pretty JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }
}

/// Phase 12E.5: the sealed-evidence-directory manifest. Every evidence
/// archive (`court-<timestamp>-<revision>/`, `campaign-*` etc.) must
/// contain one of these — the machine-readable reproducibility record of
/// the RUN. Filenames stay human-readable; this file is the semantic
/// authority.
///
/// # Schema evolution
///
/// Historical evidence is IMMUTABLE. New fields are added through
/// `schema_version` bumps; old manifests are never rewritten. A reader
/// must tolerate unknown fields (serde's default) and must key behavior
/// off `schema_version`, never off field presence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidenceManifest {
    /// Manifest schema version (currently 1).
    pub schema_version: u32,
    /// Court/driver schema version (the tool that produced the run).
    pub court_schema_version: String,
    /// EntropyFS crate version (`CARGO_PKG_VERSION`).
    pub entropyfs_version: String,
    /// Full git revision of the build.
    pub git_revision: String,
    /// On-disk format major (superblock).
    pub format_major: u16,
    /// On-disk format minor (superblock).
    pub format_minor: u16,
    /// `compat` feature bits of the store under test (hex).
    pub compat_bits: String,
    /// `ro_compat` feature bits (hex).
    pub ro_compat_bits: String,
    /// `incompat` feature bits (hex).
    pub incompat_bits: String,
    /// Entropy universe versions in the format registry (e.g.
    /// `uniform-xof-v1`).
    pub entropy_universe_versions: Vec<String>,
    /// Representation encoder policy version — `none` until a stable,
    /// versioned encoder-policy exists (ForegroundPolicy has modes, not
    /// a version).
    pub representation_encoder_policy_version: String,
    /// Storage transport (`sync` | `uring`).
    pub io_backend: String,
    /// Worker scheduler in effect (`semaphore` | `pool-<n>`).
    pub worker_scheduler: String,
    /// Kernel release (uname).
    pub kernel: String,
    /// Architecture (uname -m).
    pub architecture: String,
    /// Distribution identifier (best-effort `/etc/os-release`).
    pub distribution: String,
    /// Distribution version (best-effort `/etc/os-release`).
    pub distribution_version: String,
    /// Immutable container image digest (empty for native runs).
    pub container_image_digest: String,
    /// Host-relevant reproducibility info (hostname + CPU model).
    pub host: String,
    /// Rust compiler version (`rustc_version!()`).
    pub compiler_version: String,
    /// rustc release string (`rustc --version`).
    pub rustc_version: String,
    /// Unix seconds at capture.
    pub timestamp_unix: u64,
}

impl EvidenceManifest {
    /// Capture a full run manifest. Best-effort by construction (every
    /// source has a fallback — an empty field is visible, never silently
    /// wrong); `entropyfs_version`/`format_*` come from the build, the
    /// rest from the environment.
    pub fn capture(
        repo_root: &std::path::Path,
        store_dir: &std::path::Path,
        io_backend: &str,
        worker_scheduler: &str,
        container_image_digest: &str,
        court_schema_version: &str,
    ) -> EvidenceManifest {
        let env = Environment::capture(repo_root, store_dir, "n/a", "n/a");
        let (dist, dist_ver) = os_release();
        let arch = std::env::consts::ARCH.to_string();
        EvidenceManifest {
            schema_version: 1,
            court_schema_version: court_schema_version.to_string(),
            entropyfs_version: env!("CARGO_PKG_VERSION").to_string(),
            git_revision: env.revision_full.clone(),
            format_major: crate::format::version::FORMAT_MAJOR,
            format_minor: crate::format::version::FORMAT_MINOR,
            compat_bits: "0x0000000000000000".into(),
            ro_compat_bits: "0x0000000000000000".into(),
            incompat_bits: "0x0000000000000000".into(),
            entropy_universe_versions: vec!["uniform-xof-v1".into()],
            representation_encoder_policy_version: "none".into(),
            io_backend: io_backend.to_string(),
            worker_scheduler: worker_scheduler.to_string(),
            kernel: env.kernel_release.clone(),
            architecture: arch,
            distribution: dist,
            distribution_version: dist_ver,
            container_image_digest: container_image_digest.to_string(),
            host: format!("{} | {}", env.hostname, env.cpu_model),
            compiler_version: rustc_version(),
            rustc_version: rustc_version(),
            timestamp_unix: env.timestamp_unix,
        }
    }

    /// Render as pretty JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }
}

/// Best-effort distribution identification from `/etc/os-release`.
fn os_release() -> (String, String) {
    let body = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let mut name = String::new();
    let mut version = String::new();
    for line in body.lines() {
        if let Some(v) = line.strip_prefix("ID=") {
            name = v.trim().trim_matches('"').to_string();
        } else if let Some(v) = line.strip_prefix("VERSION_ID=") {
            version = v.trim().trim_matches('"').to_string();
        }
    }
    if name.is_empty() {
        name = "unknown".into();
    }
    if version.is_empty() {
        version = "unknown".into();
    }
    (name, version)
}

/// `rustc --version` (best-effort).
fn rustc_version() -> String {
    std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_json_roundtrip() {
        let m = Manifest {
            corpus: "corpus.case".into(),
            corpus_hash: "deadbeef".into(),
            logical_bytes: 1_000_000,
            ..Default::default()
        };
        let back: Manifest = serde_json::from_str(&m.to_json()).unwrap();
        assert_eq!(back, m);
    }
}
