//! fsck: independent validation and repair of an EntropyFS store.
//!
//! fsck does not call the happy-path mounted APIs. It scans the superblock
//! file and every segment file raw, rebuilds the derived index itself,
//! walks the object graph independently, and verifies semantic invariants
//! (`docs/recovery/fsck.md`, §34). Repairs are conservative (v1: torn
//! segment tails only); authoritative corruption is reported, never
//! silently rewritten.
//!
//! Usage is read-mostly: `entropyfs fsck <store-dir> [--repair] [--verify-materialized]`.

#![forbid(unsafe_code)]

pub mod graph;
pub mod repair;
pub mod scan;
pub mod verify;

use std::collections::HashMap;
use std::path::Path;

use crate::core::limits::Limits;
use crate::format::version::RecordTag;

/// Severity of a fsck finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Informational (e.g., a repair performed).
    Info,
    /// Suspicious but not corrupting (torn tail, unreachable objects).
    Warning,
    /// Corruption or invariant violation.
    Error,
}

/// Issue categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Superblock slots / feature bits.
    Superblock,
    /// Segment files / record envelopes.
    Segment,
    /// Record-level envelope problems.
    Record,
    /// Root object / generation binding.
    Root,
    /// Inode invariants.
    Inode,
    /// Directory invariants.
    Directory,
    /// Extent ordering / overlap / bounds.
    Extent,
    /// Chunk index content binding.
    ChunkIndex,
    /// Snapshot entries / roots.
    Snapshot,
    /// Reference resolvability.
    Reference,
    /// Reachability / leaks.
    Reachability,
    /// Graph cycles.
    Graph,
    /// Repairs performed.
    Repair,
}

impl Category {
    /// Stable short name (for machine-readable receipts).
    pub const fn name(self) -> &'static str {
        match self {
            Category::Superblock => "superblock",
            Category::Segment => "segment",
            Category::Record => "record",
            Category::Root => "root",
            Category::Inode => "inode",
            Category::Directory => "directory",
            Category::Extent => "extent",
            Category::ChunkIndex => "chunk-index",
            Category::Snapshot => "snapshot",
            Category::Reference => "reference",
            Category::Reachability => "reachability",
            Category::Graph => "graph",
            Category::Repair => "repair",
        }
    }
}

/// A single fsck finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsckIssue {
    /// Severity.
    pub severity: Severity,
    /// Category.
    pub category: Category,
    /// Human-readable message.
    pub message: String,
}

impl FsckIssue {
    /// Construct an issue.
    pub fn new(severity: Severity, category: Category, message: impl Into<String>) -> Self {
        Self {
            severity,
            category,
            message: message.into(),
        }
    }
}

/// fsck options.
#[derive(Debug, Clone)]
pub struct FsckOptions {
    /// Materialize every reachable extent and every chunk-index descriptor
    /// and verify the logical content hash (slow; the full §33 chain).
    pub verify_materialized: bool,
    /// Cap on records scanned per segment (resource bound, §51).
    pub max_records_per_segment: u64,
    /// B-tree node entry cap.
    pub max_fanout: u32,
    /// Descriptor decode bounds (mirror `Limits`).
    pub max_descriptor_bytes: u64,
    /// Inline byte cap.
    pub max_inline_bytes: u64,
    /// Palette symbol cap.
    pub max_palette: usize,
    /// Period cap.
    pub max_period: u32,
    /// Chunk size cap.
    pub max_chunk_size: u64,
    /// Truncate torn segment tails (safe; mirrors mount-time behavior).
    pub repair_torn_tails: bool,
}

impl Default for FsckOptions {
    fn default() -> Self {
        let l = Limits::default();
        Self {
            verify_materialized: false,
            max_records_per_segment: 1_000_000,
            max_fanout: l.max_fanout,
            max_descriptor_bytes: l.max_descriptor_bytes,
            max_inline_bytes: l.max_inline_bytes,
            max_palette: l.max_palette,
            max_period: l.max_period,
            max_chunk_size: l.max_chunk_size,
            repair_torn_tails: false,
        }
    }
}

/// The fsck report.
#[derive(Debug, Clone, Default)]
pub struct FsckReport {
    /// Superblock slots that decoded (0..2).
    pub superblock_slots_valid: u8,
    /// Segments scanned.
    pub segments_scanned: u64,
    /// Records scanned.
    pub records_scanned: u64,
    /// Records by tag.
    pub records_by_tag: HashMap<RecordTag, u64>,
    /// Live (reachable) object count.
    pub live_objects: u64,
    /// Leaked (unreachable) object count.
    pub leaked_objects: u64,
    /// Leaked bytes.
    pub leaked_bytes: u64,
    /// Inodes verified.
    pub inodes_verified: u64,
    /// Extents verified.
    pub extents_verified: u64,
    /// Chunk descriptors verified.
    pub chunk_descriptors_verified: u64,
    /// Issues found.
    pub issues: Vec<FsckIssue>,
    /// Repairs performed.
    pub repaired: Vec<String>,
}

impl FsckReport {
    /// Whether no error-severity issue was found.
    pub fn is_clean(&self) -> bool {
        !self.issues.iter().any(|i| i.severity == Severity::Error)
    }

    /// Error count.
    pub fn error_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .count()
    }

    /// Warning count.
    pub fn warning_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == Severity::Warning)
            .count()
    }

    /// Render a human-readable report.
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "superblock slots valid: {}/{}\n",
            self.superblock_slots_valid, 2
        ));
        s.push_str(&format!("segments scanned: {}\n", self.segments_scanned));
        s.push_str(&format!("records scanned: {}\n", self.records_scanned));
        s.push_str(&format!("live objects: {}\n", self.live_objects));
        s.push_str(&format!(
            "leaked objects: {} ({} bytes)\n",
            self.leaked_objects, self.leaked_bytes
        ));
        s.push_str(&format!("inodes verified: {}\n", self.inodes_verified));
        s.push_str(&format!("extents verified: {}\n", self.extents_verified));
        s.push_str(&format!(
            "chunk descriptors verified: {}\n",
            self.chunk_descriptors_verified
        ));
        if !self.repaired.is_empty() {
            for r in &self.repaired {
                s.push_str(&format!("repaired: {r}\n"));
            }
        }
        for issue in &self.issues {
            s.push_str(&format!(
                "{:>7} [{}] {}\n",
                format!("{:?}", issue.severity).to_lowercase(),
                issue.category.name(),
                issue.message
            ));
        }
        if self.issues.is_empty() {
            s.push_str("no issues found\n");
        }
        s
    }
}

/// Run fsck over a store directory.
pub fn fsck(dir: &Path, options: &FsckOptions) -> Result<FsckReport, String> {
    let mut ctx = scan::FsckCtx::scan(dir, options)?;
    let mut report = FsckReport {
        superblock_slots_valid: ctx.slots.len() as u8,
        ..Default::default()
    };

    // Semantic verification first (needs the decoded root).
    verify::verify_all(&mut ctx)?;

    // Independent reachability walk.
    let live = graph::mark_live(&ctx)?;
    let (leaked_objects, leaked_bytes) = graph::leaked(&ctx, &live);
    graph::report_leaks(&mut ctx, &live)?;

    // Safe repairs.
    report.repaired = repair::repair(&mut ctx)?;

    // Assemble the report.
    report.segments_scanned = ctx.segments_scanned;
    report.records_scanned = ctx.records_scanned;
    report.records_by_tag = ctx.records_by_tag;
    report.live_objects = live.len() as u64;
    report.leaked_objects = leaked_objects;
    report.leaked_bytes = leaked_bytes;
    report.inodes_verified = ctx.inodes_verified;
    report.extents_verified = ctx.extents_verified;
    report.chunk_descriptors_verified = ctx.chunk_descriptors_verified;
    report.issues = ctx.issues;

    Ok(report)
}

/// Ensure fsck does not run against a mounted store: try-lock the store's
/// exclusive lock; fail with a clear message when the store is in use.
pub fn ensure_unmounted(dir: &Path) -> Result<(), String> {
    use std::fs::OpenOptions;
    let path = dir.join("lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| format!("lock open: {e}"))?;
    use rustix::fs::{FlockOperation, flock};
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(()),
        Err(rustix::io::Errno::WOULDBLOCK) => Err(
            "store is mounted or otherwise in use (mount lock held); unmount before fsck".into(),
        ),
        Err(e) => Err(format!("lock: {e}")),
    }
}
