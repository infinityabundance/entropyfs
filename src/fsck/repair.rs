//! fsck repair phase (v1 scope is deliberately conservative).
//!
//! v1 repairs only conditions that are safe by construction:
//!
//! - Torn segment tails are truncated (identical to what `SegmentWriter`
//!   does on open; the appended data was never durable).
//! - The derived object index is disposable and rebuilt at mount, so no
//!   repair is needed there.
//!
//! Everything else (superblock corruption, mid-file corruption, root
//! mismatch, missing references) is report-only: rewriting authoritative
//! data without a known-good copy risks destroying the only evidence.

#![forbid(unsafe_code)]

use super::scan::FsckCtx;
use super::{Category, FsckIssue, Severity};

/// Apply the safe repairs: truncate torn segment tails.
pub fn repair(ctx: &mut FsckCtx) -> Result<Vec<String>, String> {
    let mut repaired = Vec::new();
    if !ctx.options.repair_torn_tails {
        return Ok(repaired);
    }
    let tails = std::mem::take(&mut ctx.torn_tails);
    for (seq, keep, truncate_to) in tails {
        let path = crate::store::segment::segment_path(&ctx.dir, seq);
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|e| format!("segment {seq} open for repair: {e}"))?;
        f.set_len(keep)
            .map_err(|e| format!("segment {seq} truncate: {e}"))?;
        f.sync_data()
            .map_err(|e| format!("segment {seq} sync: {e}"))?;
        repaired.push(format!(
            "segment {seq}: torn tail truncated from {} to {} bytes",
            truncate_to, keep
        ));
        ctx.issues.push(FsckIssue::new(
            Severity::Info,
            Category::Repair,
            format!("segment {seq}: torn tail truncated ({truncate_to} -> {keep} bytes)"),
        ));
    }
    Ok(repaired)
}
