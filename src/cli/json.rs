//! Phase 12E.6: operator-grade versioned JSON surfaces.
//!
//! `entropyfs status --json`, `entropyfs metrics --json`, `entropyfs
//! fsck --json`. The DTOs here are the EXTERNAL contract — explicitly
//! versioned (`schema_version`), never direct serializations of internal
//! Rust state. Programs parse these; they never parse CLI prose.
//!
//! # Schema discipline
//!
//! - Every DTO carries `schema_version`; a breaking change bumps it.
//! - Field units are explicit in the names (`_bytes`, `_us`, `_ms`).
//! - New fields are additive; old readers tolerate them (serde default).
//! - The schemas are documented in `docs/operations/fsck-json.md` and
//!   `docs/operations/metrics.md`.
//!
//! # PERSISTENT AUTHORITY
//!
//! None. Diagnostics only.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// `entropyfs status --json` DTO (schema 1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusJson {
    /// DTO schema version.
    pub schema_version: u32,
    /// `mounted` (lock held; no deeper read) | `ok`.
    pub state: String,
    /// Store directory (as given).
    pub store: String,
    /// Filesystem uuid (hex).
    pub uuid: String,
    /// Committed generation.
    pub generation: u64,
    /// On-disk format version + feature masks.
    pub format: crate::engine::FormatInfo,
    /// Physical capacity (bytes).
    pub physical_capacity_bytes: u64,
    /// Physical used (bytes).
    pub physical_used_bytes: u64,
    /// Physical free (bytes).
    pub physical_free_bytes: u64,
    /// Logical bytes across all inodes.
    pub logical_bytes: u64,
    /// Inode count.
    pub inode_count: usize,
    /// Snapshot count.
    pub snapshot_count: usize,
    /// fsck summary.
    pub fsck: StatusFsck,
    /// Phase-9H physical reconciliation (present when the store opened).
    pub physical: Option<crate::engine::PhysicalMetrics>,
    /// DSFB observer accounting.
    pub dsfb: StatusDsfb,
}

/// fsck summary inside status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusFsck {
    /// `clean` | `issues`.
    pub status: String,
    /// Error-severity finding count.
    pub errors: usize,
    /// Warning-severity finding count.
    pub warnings: usize,
    /// Unreachable (reclaimable) bytes per fsck.
    pub leaked_bytes: u64,
    /// Unreachable object count per fsck.
    pub leaked_objects: u64,
}

/// DSFB accounting inside status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusDsfb {
    /// Tracked chunks (snapshot).
    pub tracked_chunks: usize,
    /// Observer steps (cumulative).
    pub steps: u64,
    /// Drift events (cumulative).
    pub drift_events: u64,
    /// Slew events (cumulative).
    pub slew_events: u64,
    /// Narrowed searches (cumulative).
    pub narrowed_searches: u64,
}

/// `entropyfs fsck --json` DTO (schema 1): typed findings, never prose.
///
/// `findings[].code` is the stable uppercase category code (see
/// `docs/operations/fsck-json.md`): SUPERBLOCK, SEGMENT, RECORD, ROOT,
/// INODE, DIRECTORY, EXTENT, CHUNK_INDEX, SNAPSHOT, REFERENCE,
/// REACHABILITY, GRAPH, REPAIR.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FsckJson {
    /// DTO schema version.
    pub schema_version: u32,
    /// `clean` | `corrupt` (error-severity findings present).
    pub status: String,
    /// Superblock slots that decoded (0..2).
    pub superblock_slots_valid: u8,
    /// Segments scanned.
    pub segments_scanned: u64,
    /// Records scanned.
    pub records_scanned: u64,
    /// Live (reachable) object count.
    pub live_objects: u64,
    /// Leaked object count.
    pub leaked_objects: u64,
    /// Leaked bytes.
    pub leaked_bytes: u64,
    /// Inodes verified.
    pub inodes_verified: u64,
    /// Extents verified.
    pub extents_verified: u64,
    /// Chunk descriptors verified.
    pub chunk_descriptors_verified: u64,
    /// Repairs performed (human-readable; machine actions are the
    /// `findings` entries with code REPAIR).
    pub repairs: Vec<String>,
    /// Typed findings.
    pub findings: Vec<FsckFindingJson>,
}

/// One typed fsck finding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FsckFindingJson {
    /// Stable uppercase category code.
    pub code: String,
    /// `info` | `warning` | `error`.
    pub severity: String,
    /// Stable lowercase category name (the registry key).
    pub category: String,
    /// Human-readable detail (informational; never parsed).
    pub message: String,
}

/// Stable uppercase finding code for a category (the machine contract).
pub fn finding_code(category: crate::fsck::Category) -> &'static str {
    match category {
        crate::fsck::Category::Superblock => "SUPERBLOCK",
        crate::fsck::Category::Segment => "SEGMENT",
        crate::fsck::Category::Record => "RECORD",
        crate::fsck::Category::Root => "ROOT",
        crate::fsck::Category::Inode => "INODE",
        crate::fsck::Category::Directory => "DIRECTORY",
        crate::fsck::Category::Extent => "EXTENT",
        crate::fsck::Category::ChunkIndex => "CHUNK_INDEX",
        crate::fsck::Category::Snapshot => "SNAPSHOT",
        crate::fsck::Category::Reference => "REFERENCE",
        crate::fsck::Category::Reachability => "REACHABILITY",
        crate::fsck::Category::Graph => "GRAPH",
        crate::fsck::Category::Repair => "REPAIR",
    }
}

/// Severity string.
pub fn severity_str(s: crate::fsck::Severity) -> &'static str {
    match s {
        crate::fsck::Severity::Info => "info",
        crate::fsck::Severity::Warning => "warning",
        crate::fsck::Severity::Error => "error",
    }
}

impl FsckJson {
    /// Build from a fsck report.
    pub fn from_report(report: &crate::fsck::FsckReport) -> Self {
        let findings = report
            .issues
            .iter()
            .map(|i| FsckFindingJson {
                code: finding_code(i.category).to_string(),
                severity: severity_str(i.severity).to_string(),
                category: i.category.name().to_string(),
                message: i.message.clone(),
            })
            .collect();
        FsckJson {
            schema_version: 1,
            status: if report.is_clean() {
                "clean"
            } else {
                "corrupt"
            }
            .into(),
            superblock_slots_valid: report.superblock_slots_valid,
            segments_scanned: report.segments_scanned,
            records_scanned: report.records_scanned,
            live_objects: report.live_objects,
            leaked_objects: report.leaked_objects,
            leaked_bytes: report.leaked_bytes,
            inodes_verified: report.inodes_verified,
            extents_verified: report.extents_verified,
            chunk_descriptors_verified: report.chunk_descriptors_verified,
            repairs: report.repaired.clone(),
            findings,
        }
    }

    /// Render as pretty JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fsck_json_roundtrip() {
        let report = crate::fsck::FsckReport {
            issues: vec![crate::fsck::FsckIssue::new(
                crate::fsck::Severity::Warning,
                crate::fsck::Category::Extent,
                "example finding",
            )],
            ..Default::default()
        };
        let j = FsckJson::from_report(&report);
        assert_eq!(j.status, "clean"); // warnings only
        assert_eq!(j.findings[0].code, "EXTENT");
        assert_eq!(j.findings[0].severity, "warning");
        let back: FsckJson = serde_json::from_str(&j.to_json()).unwrap();
        assert_eq!(back, j);
    }

    #[test]
    fn finding_codes_are_stable_uppercase() {
        use crate::fsck::Category::*;
        for c in [
            Superblock,
            Segment,
            Record,
            Root,
            Inode,
            Directory,
            Extent,
            ChunkIndex,
            Snapshot,
            Reference,
            Reachability,
            Graph,
            Repair,
        ] {
            let code = finding_code(c);
            assert_eq!(code, code.to_uppercase());
            assert!(!code.contains(' '));
        }
    }
}
