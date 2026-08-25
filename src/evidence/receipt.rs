//! Crash-court receipts (§38, §50): machine-readable records of a crash
//! test run. JSON (serde) is allowed here — receipts are human-readable
//! evidence artifacts, never the permanent on-disk format.

#![forbid(unsafe_code)]

use std::path::Path;

use serde::{Deserialize, Serialize};

/// One crash-court run result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrashReceipt {
    /// Crash point (durability boundary) exercised.
    pub crash_point: String,
    /// Pre-state content hash (logical bytes).
    pub pre_hash: String,
    /// Post-state content hash attempted.
    pub post_hash: String,
    /// Recovered content hash (must equal pre or post).
    pub recovered_hash: String,
    /// Whether the recovered state was admissible (pre or post).
    pub admissible: bool,
    /// Whether fsck passed on the recovered store.
    pub fsck_clean: bool,
    /// Whether the recovered store accepted a new write.
    pub writable: bool,
    /// Store directory (for reproduction).
    pub store_dir: String,
    /// EntropyFS revision (best-effort `git describe`).
    pub revision: String,
    /// Kernel version.
    pub kernel: String,
    /// Timestamp (unix seconds).
    pub unix_secs: u64,
}

impl CrashReceipt {
    /// The git revision (best-effort; empty when unavailable).
    pub fn current_revision() -> String {
        std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }

    /// The running kernel release.
    pub fn current_kernel() -> String {
        std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// Render as pretty JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }
}

/// Write a receipt atomically.
pub fn write_receipt(dir: &Path, name: &str, receipt: &CrashReceipt) -> std::io::Result<()> {
    let path = dir.join(format!("{name}.json"));
    crate::store::write_atomic(&path, receipt.to_json().as_bytes())
        .map_err(|e| std::io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_json_roundtrip() {
        let r = CrashReceipt {
            crash_point: "AfterSuperblockFsync".into(),
            pre_hash: "aa".into(),
            post_hash: "bb".into(),
            recovered_hash: "aa".into(),
            admissible: true,
            fsck_clean: true,
            writable: true,
            store_dir: "/tmp/x".into(),
            revision: "deadbeef".into(),
            kernel: "6.7".into(),
            unix_secs: 0,
        };
        let json = r.to_json();
        let back: CrashReceipt = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        let tmp = tempfile::TempDir::new().unwrap();
        write_receipt(tmp.path(), "run-001", &r).unwrap();
        assert!(tmp.path().join("run-001.json").exists());
    }
}
