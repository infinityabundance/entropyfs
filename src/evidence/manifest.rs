//! Benchmark manifests (§42, §50): the full reproducibility record for a
//! benchmark run — environment, command, representation distribution, and
//! byte accounting. JSON for human-readable evidence.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
