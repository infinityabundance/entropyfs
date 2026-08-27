//! fsck verify phase: semantic invariant checks over the scanned image.
//!
//! Checks inode invariants, directory invariants, extent ordering and
//! non-overlap, descriptor validity, reference resolvability, snapshot
//! roots, the chunk-index content binding, and hard-link reference counts
//! (`docs/recovery/fsck.md`).
//!
//! # PURPOSE
//!
//! The "does the store mean what it stores" pass: after `scan` rebuilds
//! the derived object index and decodes the root, verify checks the
//! semantic invariants the mounted store's read paths assume — extent
//! bounds, resolvable references, valid descriptors, consistent link
//! counts. It runs before the graph phase (reachability) and shares its
//! inode map across stages.
//!
//! # BOUNDARY
//!
//! Knows the scanned image through `FsckCtx` (segment payload reads, the
//! rebuilt object index, the decoded `Root`, and the `Limits` derived
//! from fsck options). It may MATERIALIZE descriptors in deep mode
//! (`--verify-materialized`) but never writes back; it does not compute
//! reachability (that is `graph.rs`) and does not re-check record
//! envelopes (the scan phase did).
//!
//! # MODEL
//!
//! The store is a set of content-addressed objects organized under the
//! root's B-trees: an inode index (`u64 ino → inode`), per-directory and
//! per-file trees, a chunk index (`[u8;32] content id → descriptor`), a
//! snapshot tree, and a model index. Every value is derived from the
//! same segment payloads the scan indexed, so a reference that resolves
//! nowhere can never materialize.
//!
//! # CORRECTNESS INVARIANTS (what fsck verifies and why)
//!
//! 1. The store is one this build may read (feature bits, format major).
//! 2. Inodes: mode type agrees with the data kind; `nlink > 0`; symlink
//!    size == target length; the root directory ino exists. Duplicate
//!    inos cannot be encoded: the inode index is a B-tree keyed by ino
//!    and `Node::decode` enforces strictly increasing keys (Phase 10G
//!    handed out duplicate inos when a checkpoint reset the inode
//!    high-water mark; the structural exclusion is the defense here).
//! 3. Directories: names valid (non-empty, not `.`/`..`, no `/` or NUL),
//!    entries decode, targets exist, `d_type` matches the target kind.
//! 4. Extents: descriptor decode + validation under `Limits`; no
//!    overlap; extent end <= file size (this check caught the Phase-10G
//!    stale-snapshot-merge regression: a checkpoint merged a stale
//!    snapshot onto a newer tree, size regressed, tail extents stayed);
//!    optionally the materialized bytes hash to the chunk-index key
//!    (ADR-0011's "valid physical record, wrong logical bytes").
//! 5. Chunk index: every entry decodes/validates; a content id's entry
//!    must materialize to bytes whose BLAKE3 is that id — proven by
//!    materialization, not descriptor-byte equality (an EXACT_REF alias
//!    legitimately differs from the canonical entry).
//! 6. Snapshots: entries decode; their root objects exist.
//! 7. Reference counts: directory references to an inode never exceed
//!    its `nlink` (the lower bound — directories carry `.`/`..`
//!    self-links, so refs < nlink is legal and unreported).
//!
//! # PERSISTENT AUTHORITY
//!
//! None: verify only reads. Findings are recorded as issues; repair
//! decisions happen later in `repair::repair`.
//!
//! # CONCURRENCY
//!
//! Single-threaded; `&mut FsckCtx` throughout. No locks.
//!
//! # RESOURCE BOUNDS (fsck must itself be hostile-media safe)
//!
//! fsck runs on untrusted backing bytes, so verify keeps CPU and
//! allocations bounded on malformed input: `max_records_per_segment`
//! caps the scan; `max_fanout` caps every B-tree node decode (and
//! `Node::decode` enforces strictly-increasing keys before trusting a
//! node); every descriptor decodes under `FsckCtx::limits()`
//! (`max_descriptor_bytes`, `max_inline_bytes`, `max_palette`,
//! `max_period`, `max_chunk_size`); materialization runs through
//! `materialize_to_vec` under the same limits (operation and allocation
//! budgets). There is no recursion over persisted data. Deep mode is
//! off by default: it multiplies work by the materialization cost of
//! every extent and every chunk-index entry.
//!
//! # PERFORMANCE
//!
//! A linear pass: each tree is scanned once per phase (`scan_all` with
//! fanout-capped nodes). Deep mode is the only superlinear option. The
//! counters `ctx.inodes_verified` / `ctx.extents_verified` /
//! `ctx.chunk_descriptors_verified` are counts of records actually
//! checked, surfaced in `FsckReport`.
//!
//! # FAILURE MODES
//!
//! - Hard `Err` (aborts fsck): a tree scan fails — the structure cannot
//!   even be walked (unreadable node, malformed key length).
//! - Issues (continue): every semantic violation is a typed
//!   `FsckIssue`; a corrupt record yields an Error issue and the walk
//!   proceeds to the next record (fsck never aborts on one bad record).
//!
//! # HISTORY / EVIDENCE
//!
//! - ADR-0011: the deep-mode chain (physical record → descriptor →
//!   materialized bytes → logical content hash) is its last link.
//! - Phase 10G: the "extent ends beyond file size" fsck finding came
//!   from exactly the check in `verify_extents` (overlapping checkpoints
//!   merged a stale snapshot onto a newer tree).
//! - Phase 11A: the hostile-media court drives fsck against mutated
//!   stores and demands typed findings, never panics.
//! - `docs/recovery/fsck.md` §1 enumerates the full verified surface.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::core::extent::ChunkId;
use crate::core::materialize::DecoderContext;
use crate::core::representation::Representation;
use crate::store::directory::{DirEntry, dt};
use crate::store::index;
use crate::store::inode::{Inode, InodeData, mode};

use super::scan::FsckCtx;
use super::{Category, FsckIssue, Severity};

/// Run all semantic verification phases, in order.
///
/// The stages share derived state (`verify_inodes`' map is consumed by
/// the directory, extent, and reference-count phases):
///
/// ```text
/// Stage 1  superblock feature/version compatibility
/// Stage 2  inodes: index scan, per-inode invariants, resolvability
/// Stage 3  directories: names, d_type, target existence
/// Stage 4  extents: ordering, non-overlap, size bounds, deep check
/// Stage 5  chunk index: decode/validate, deep content binding
/// Stage 6  snapshots: decode, root presence
/// Stage 7  reference counts: nlink vs directory references
/// ```
///
/// Each stage records `FsckIssue`s and continues; only an unreadable
/// tree (scan failure) aborts. See the module doc for what each stage
/// proves and why.
pub fn verify_all(ctx: &mut FsckCtx) -> Result<(), String> {
    // -----------------------------------------------------------------
    // Stage 1: the store must be readable by this build.
    // -----------------------------------------------------------------
    verify_superblock_features(ctx);

    // -----------------------------------------------------------------
    // Stage 2: the inode index and every inode (returns the ino → inode
    // map the later stages reuse).
    // -----------------------------------------------------------------
    let inodes = verify_inodes(ctx)?;

    // -----------------------------------------------------------------
    // Stage 3: directory trees (name validity, d_type, resolvability).
    // -----------------------------------------------------------------
    verify_directories(ctx, &inodes)?;

    // -----------------------------------------------------------------
    // Stage 4: extent trees (ordering, non-overlap, size bounds).
    // -----------------------------------------------------------------
    verify_extents(ctx, &inodes)?;

    // -----------------------------------------------------------------
    // Stage 5: the chunk index content binding.
    // -----------------------------------------------------------------
    verify_chunk_index(ctx)?;

    // -----------------------------------------------------------------
    // Stage 6: snapshot entries and their roots.
    // -----------------------------------------------------------------
    verify_snapshots(ctx)?;

    // -----------------------------------------------------------------
    // Stage 7: hard-link reference counts against nlink.
    // -----------------------------------------------------------------
    verify_reference_counts(ctx, &inodes)?;
    Ok(())
}

/// Stage 1: verify the store is one this build may read.
///
/// Feature-bit compatibility (`ReadOnlyOnly` → warning; `Refused` →
/// error) and the decoded root's format major against this build's
/// `FORMAT_MAJOR`. Everything after this stage assumes format-v1
/// semantics, so an unreadable store must be reported before any deeper
/// walk.
fn verify_superblock_features(ctx: &mut FsckCtx) {
    match crate::format::features::check(ctx.active.features(), false) {
        crate::format::features::Compatibility::Ok => {}
        crate::format::features::Compatibility::ReadOnlyOnly(e) => {
            ctx.issues.push(FsckIssue::new(
                Severity::Warning,
                Category::Superblock,
                format!(
                    "store carries unknown ro_compat features (0x{:016x}); \
                     writable opens are refused, read-only access is safe: {}",
                    e.unknown_ro_compat, e.remediation
                ),
            ));
        }
        crate::format::features::Compatibility::Refused(e) => {
            ctx.issues.push(FsckIssue::new(
                Severity::Error,
                Category::Superblock,
                format!("store features refuse access: {e}"),
            ));
        }
    }
    if let Some(root) = &ctx.root {
        if root.format_major != crate::format::version::FORMAT_MAJOR {
            ctx.issues.push(FsckIssue::new(
                Severity::Error,
                Category::Root,
                format!(
                    "root format major {} is not readable by this build ({}); minor {}",
                    root.format_major,
                    crate::format::version::FORMAT_MAJOR,
                    root.format_minor
                ),
            ));
        }
    }
}

/// Verify the inode index: every value is an id, every inode decodes, and
/// internal invariants hold. Returns ino → inode for later phases.
///
/// # What is checked
///
/// - every index key is a valid u64 ino (8 bytes);
/// - every index value is a 32-byte content id;
/// - the referenced object exists in a segment (reference resolvability)
///   and decodes as an `Inode`;
/// - per-inode invariants via `check_inode_invariants`;
/// - the root directory ino is present in the index.
///
/// # Units
///
/// `ino` is the on-disk u64 inode number (the index key); sizes are
/// bytes. `ctx.inodes_verified` counts inodes that passed decode.
///
/// # Duplicate inos
///
/// The inode index is a B-tree keyed by ino and `Node::decode` enforces
/// strictly increasing keys, so duplicate inos cannot be encoded in any
/// decodable tree (Phase 10G's duplicate-ino bug — a checkpoint reset
/// the inode high-water mark — is structurally excluded here); the
/// root-ino presence check catches the same class of checkpoint
/// corruption.
///
/// # Failure behavior
///
/// A missing inode object or a decode failure is an Error issue and that
/// inode is skipped; only a failed index scan aborts.
fn verify_inodes(ctx: &mut FsckCtx) -> Result<HashMap<u64, Inode>, String> {
    let mut out = HashMap::new();
    let (inode_index_root, root_dir_ino) = match &ctx.root {
        Some(r) => (r.inode_index_root, r.root_dir_ino),
        None => {
            ctx.issues.push(FsckIssue::new(
                Severity::Error,
                Category::Inode,
                "skipping inode verification: no valid root".to_string(),
            ));
            return Ok(out);
        }
    };
    let entries = index::scan_all(
        inode_index_root,
        crate::store::BTREE_ORDER,
        ctx.options.max_fanout,
        ctx,
    )
    .map_err(|e| format!("inode index scan: {e}"))?;
    for (k, v) in entries {
        let ino = u64::from_be_bytes(
            k.as_slice()
                .try_into()
                .map_err(|_| "inode index key not 8 bytes".to_string())?,
        );
        if v.len() != 32 {
            ctx.issues.push(FsckIssue::new(
                Severity::Error,
                Category::Inode,
                format!("inode {ino}: index value is not a content id ({})", v.len()),
            ));
            continue;
        }
        let inode_id = ChunkId::new(v.as_slice().try_into().unwrap());
        let payload = match ctx.fetch_object(&inode_id) {
            Ok(p) => p,
            Err(e) => {
                ctx.issues.push(FsckIssue::new(
                    Severity::Error,
                    Category::Reference,
                    format!("inode {ino}: object {inode_id} missing ({e})"),
                ));
                continue;
            }
        };
        let inode = match Inode::decode(&payload) {
            Ok(i) => i,
            Err(e) => {
                ctx.issues.push(FsckIssue::new(
                    Severity::Error,
                    Category::Inode,
                    format!("inode {ino}: decode failed: {e:?}"),
                ));
                continue;
            }
        };
        check_inode_invariants(ctx, ino, &inode);
        out.insert(ino, inode);
        ctx.inodes_verified += 1;
    }
    // The root directory must exist.
    if !out.contains_key(&root_dir_ino) {
        ctx.issues.push(FsckIssue::new(
            Severity::Error,
            Category::Inode,
            format!(
                "root directory inode {} missing from the inode index",
                root_dir_ino
            ),
        ));
    }
    Ok(out)
}

/// Per-inode semantic checks (Stage-2 inner loop):
///
/// - mode type bits (`S_IFMT`) agree with the data kind (device/other
///   kinds carried in `rdev` skip via the catch-all arm);
/// - `nlink > 0` — every indexed inode must be linked at least once;
/// - a directory's `nlink >= 2` (warning): `.`/`..` self-links make a
///   lower count suspicious;
/// - a symlink's `size` equals its target's byte length.
fn check_inode_invariants(ctx: &mut FsckCtx, ino: u64, inode: &Inode) {
    // mode type bits must agree with the data kind.
    let mode_type = inode.mode & mode::S_IFMT;
    let kind_ok = match mode_type {
        mode::S_IFDIR => matches!(inode.data, InodeData::Directory { .. }),
        mode::S_IFREG => matches!(inode.data, InodeData::File { .. }),
        mode::S_IFLNK => matches!(inode.data, InodeData::Symlink { .. }),
        _ => true, // device/other kinds are carried in rdev
    };
    if !kind_ok {
        ctx.issues.push(FsckIssue::new(
            Severity::Error,
            Category::Inode,
            format!("inode {ino}: mode type {mode_type:#o} contradicts data kind"),
        ));
    }
    if inode.nlink == 0 {
        ctx.issues.push(FsckIssue::new(
            Severity::Error,
            Category::Inode,
            format!("inode {ino}: nlink is zero"),
        ));
    }
    if matches!(inode.data, InodeData::Directory { .. }) && inode.nlink < 2 {
        ctx.issues.push(FsckIssue::new(
            Severity::Warning,
            Category::Inode,
            format!("inode {ino}: directory with nlink {}", inode.nlink),
        ));
    }
    if let InodeData::Symlink { target } = &inode.data {
        if inode.size != target.len() as u64 {
            ctx.issues.push(FsckIssue::new(
                Severity::Error,
                Category::Inode,
                format!(
                    "inode {ino}: symlink size {} != target length {}",
                    inode.size,
                    target.len()
                ),
            ));
        }
    }
}

/// Verify directory trees: entries decode, names are valid bytes, target
/// inodes exist, and d_type matches the target kind.
///
/// # Name validity
///
/// The on-disk format never stores synthesized entries: names must be
/// non-empty, not `.`/`..`, and free of `/` and NUL (the mount path
/// synthesizes `.`/`..` on read).
///
/// # Why the d_type cross-check exists
///
/// `d_type` is the readdir hint; a hint contradicting the target inode's
/// kind breaks `readdir` consumers (the kernel filters by it), so the
/// contradiction is a directory-tree corruption.
fn verify_directories(ctx: &mut FsckCtx, inodes: &HashMap<u64, Inode>) -> Result<(), String> {
    for (&ino, inode) in inodes {
        if let InodeData::Directory { dir_root } = &inode.data {
            if dir_root.is_zero() {
                continue;
            }
            let entries = index::scan_all(
                *dir_root,
                crate::store::BTREE_ORDER,
                ctx.options.max_fanout,
                ctx,
            )
            .map_err(|e| format!("directory {ino} scan: {e}"))?;
            for (name, value) in entries {
                if name.is_empty() || name == b"." || name == b".." {
                    ctx.issues.push(FsckIssue::new(
                        Severity::Error,
                        Category::Directory,
                        format!(
                            "directory {ino}: invalid entry name {:?}",
                            String::from_utf8_lossy(&name)
                        ),
                    ));
                    continue;
                }
                if name.contains(&b'/') || name.contains(&0u8) {
                    ctx.issues.push(FsckIssue::new(
                        Severity::Error,
                        Category::Directory,
                        format!(
                            "directory {ino}: name {:?} contains '/' or NUL",
                            String::from_utf8_lossy(&name)
                        ),
                    ));
                }
                let entry = match DirEntry::decode(&value) {
                    Ok(e) => e,
                    Err(e) => {
                        ctx.issues.push(FsckIssue::new(
                            Severity::Error,
                            Category::Directory,
                            format!(
                                "directory {ino}: entry {:?} decode failed: {e:?}",
                                String::from_utf8_lossy(&name)
                            ),
                        ));
                        continue;
                    }
                };
                let target = match inodes.get(&entry.ino) {
                    Some(t) => t,
                    None => {
                        ctx.issues.push(FsckIssue::new(
                            Severity::Error,
                            Category::Reference,
                            format!(
                                "directory {ino}: entry {:?} references missing inode {}",
                                String::from_utf8_lossy(&name),
                                entry.ino
                            ),
                        ));
                        continue;
                    }
                };
                let d_type_ok = match target.data {
                    InodeData::Directory { .. } => entry.d_type == dt::DT_DIR,
                    InodeData::File { .. } => entry.d_type == dt::DT_REG,
                    InodeData::Symlink { .. } => entry.d_type == dt::DT_LNK,
                    _ => true,
                };
                if !d_type_ok {
                    ctx.issues.push(FsckIssue::new(
                        Severity::Error,
                        Category::Directory,
                        format!(
                            "directory {ino}: entry {:?} d_type {} contradicts target kind",
                            String::from_utf8_lossy(&name),
                            entry.d_type
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Verify extent trees: descriptors decode and validate, extents do not
/// overlap, no extent extends past the file size, and (optionally) the
/// materialized bytes hash to the chunk index key.
///
/// # Units
///
/// Extent keys are u64 byte offsets; `prev_end`, `start`, and `end` are
/// byte offsets; `desc.len()` is the extent's logical byte length;
/// `ctx.extents_verified` counts extents checked.
///
/// # Ordering / overlap
///
/// Tree keys are strictly increasing (enforced at `Node::decode`), so
/// `start` is always greater than the previous start; overlap is exactly
/// `start < prev_end` where the previous extent is longer than its own
/// key span. `prev_end = prev_end.max(end)` keeps the running end even
/// past a corrupt (overlapping) extent so one bad record does not hide
/// the next.
///
/// # Extent end <= file size
///
/// An extent ending past `inode.size` would make the read path
/// materialize bytes the file's logical size does not admit. This check
/// caught the Phase-10G stale-snapshot-merge regression (a checkpoint
/// merged a stale snapshot onto a newer tree: size regressed while tail
/// extents stayed).
///
/// # Failure behavior
///
/// A decode/validation failure is an Error issue and the extent is
/// skipped; only a failed tree scan aborts.
fn verify_extents(ctx: &mut FsckCtx, inodes: &HashMap<u64, Inode>) -> Result<(), String> {
    for (&ino, inode) in inodes {
        if let InodeData::File { extent_root } = &inode.data {
            if extent_root.is_zero() {
                continue;
            }
            let entries = index::scan_all(
                *extent_root,
                crate::store::BTREE_ORDER,
                ctx.options.max_fanout,
                ctx,
            )
            .map_err(|e| format!("extent tree scan (ino {ino}): {e}"))?;
            let mut prev_end = 0u64;
            let mut first = true;
            for (start_bytes, desc_bytes) in entries {
                let start = u64::from_be_bytes(
                    start_bytes
                        .as_slice()
                        .try_into()
                        .map_err(|_| "extent key not 8 bytes".to_string())?,
                );
                let desc = match crate::format::descriptor::decode(&desc_bytes, &ctx.limits()) {
                    Ok(d) => d,
                    Err(e) => {
                        ctx.issues.push(FsckIssue::new(
                            Severity::Error,
                            Category::Extent,
                            format!(
                                "ino {ino}: extent at {start}: descriptor decode failed: {e:?}"
                            ),
                        ));
                        continue;
                    }
                };
                if desc.validate(&ctx.limits()).is_err() {
                    ctx.issues.push(FsckIssue::new(
                        Severity::Error,
                        Category::Extent,
                        format!("ino {ino}: extent at {start}: descriptor validation failed"),
                    ));
                }
                let end = start.saturating_add(desc.len());
                if !first && start < prev_end {
                    ctx.issues.push(FsckIssue::new(
                        Severity::Error,
                        Category::Extent,
                        format!("ino {ino}: extent at {start} overlaps previous extent ending at {prev_end}"),
                    ));
                }
                if end > inode.size {
                    ctx.issues.push(FsckIssue::new(
                        Severity::Error,
                        Category::Extent,
                        format!(
                            "ino {ino}: extent at {start} ends at {end} beyond file size {}",
                            inode.size
                        ),
                    ));
                }
                if ctx.options.verify_materialized {
                    verify_extent_content(ctx, ino, start, &desc);
                }
                ctx.extents_verified += 1;
                prev_end = prev_end.max(end);
                first = false;
            }
        }
    }
    Ok(())
}

/// Deep check for one extent (only when `--verify-materialized`):
/// materialize the descriptor, check the byte length equals
/// `desc.len()`, hash the bytes to a content id, and prove the chunk
/// index binding — the index entry for that id must itself materialize
/// to the SAME bytes.
///
/// This is the ADR-0011 chain's last link: "a valid physical record
/// that materializes to wrong logical bytes" must be detected. It is
/// expensive (a full materialization per extent) and is gated behind
/// `verify_materialized`, which is off by default.
fn verify_extent_content(ctx: &mut FsckCtx, ino: u64, start: u64, desc: &Representation) {
    let limits = ctx.limits();
    // Materialize the extent and check internal consistency, then verify
    // the chunk-index binding for the materialized content id (§33).
    let bytes = match crate::core::materialize::materialize_to_vec(desc, ctx, &limits) {
        Ok(b) => b,
        Err(e) => {
            ctx.issues.push(FsckIssue::new(
                Severity::Error,
                Category::Extent,
                format!("ino {ino}: extent at {start}: materialization failed: {e}"),
            ));
            return;
        }
    };
    if bytes.len() as u64 != desc.len() {
        ctx.issues.push(FsckIssue::new(
            Severity::Error,
            Category::Extent,
            format!(
                "ino {ino}: extent at {start}: materialized {} bytes, descriptor declares {}",
                bytes.len(),
                desc.len()
            ),
        ));
        return;
    }
    let cid = crate::core::extent::ChunkId::of(&bytes);
    let root = match &ctx.root {
        Some(r) => r,
        None => return,
    };
    match index::get(
        root.chunk_index_root,
        cid.as_bytes(),
        crate::store::BTREE_ORDER,
        ctx.options.max_fanout,
        ctx,
    ) {
        Ok(Some(v)) => {
            // The chunk index must map the materialized content id to a
            // descriptor that materializes to these exact bytes. The index
            // entry may legitimately differ from the extent's own
            // descriptor (e.g. an EXACT_REF alias resolves to the original
            // descriptor for the shared content), so the binding is proven
            // by materialization, not descriptor-byte equality (§33).
            let idx_desc = match crate::format::descriptor::decode(&v, &ctx.limits()) {
                Ok(d) => d,
                Err(e) => {
                    ctx.issues.push(FsckIssue::new(
                        Severity::Error,
                        Category::ChunkIndex,
                        format!(
                            "ino {ino}: extent at {start}: chunk index entry for {cid} does not decode: {e:?}"
                        ),
                    ));
                    return;
                }
            };
            match crate::core::materialize::materialize_to_vec(&idx_desc, ctx, &limits) {
                Ok(idx_bytes) if idx_bytes == bytes => {}
                Ok(_) => ctx.issues.push(FsckIssue::new(
                    Severity::Error,
                    Category::ChunkIndex,
                    format!(
                        "ino {ino}: extent at {start} materializes to content {cid} but the chunk index entry for that id materializes to different bytes"
                    ),
                )),
                Err(e) => ctx.issues.push(FsckIssue::new(
                    Severity::Error,
                    Category::ChunkIndex,
                    format!(
                        "ino {ino}: extent at {start}: chunk index entry for {cid} failed to materialize: {e}"
                    ),
                )),
            }
        }
        Ok(None) => ctx.issues.push(FsckIssue::new(
            Severity::Error,
            Category::ChunkIndex,
            format!(
                "ino {ino}: extent at {start} materializes to content {cid} which has no chunk index entry"
            ),
        )),
        Err(e) => ctx.issues.push(FsckIssue::new(
            Severity::Error,
            Category::ChunkIndex,
            format!("ino {ino}: extent at {start}: chunk index lookup failed: {e}"),
        )),
    }
}

/// Verify the chunk index: descriptors decode, validate, and (optionally)
/// materialize to bytes whose hash equals the index key.
///
/// # Units
///
/// Keys are 32-byte content ids (BLAKE3 of the materialized logical
/// bytes); `ctx.chunk_descriptors_verified` counts entries checked.
///
/// # The content binding
///
/// The chunk index maps a content id to the descriptor capable of
/// materializing that content. Deep mode re-derives each entry's bytes
/// and compares the hash against the key
/// (`integrity::content::verify_descriptor`), proving the binding the
/// read path depends on when it resolves an EXACT_REF or a base
/// residual.
fn verify_chunk_index(ctx: &mut FsckCtx) -> Result<(), String> {
    let root = match &ctx.root {
        Some(r) => r,
        None => return Ok(()),
    };
    if root.chunk_index_root.is_zero() {
        return Ok(());
    }
    let entries = index::scan_all(
        root.chunk_index_root,
        crate::store::BTREE_ORDER,
        ctx.options.max_fanout,
        ctx,
    )
    .map_err(|e| format!("chunk index scan: {e}"))?;
    for (k, v) in entries {
        let key = ChunkId::new(
            k.as_slice()
                .try_into()
                .map_err(|_| "chunk index key not 32 bytes".to_string())?,
        );
        let desc = match crate::format::descriptor::decode(&v, &ctx.limits()) {
            Ok(d) => d,
            Err(e) => {
                ctx.issues.push(FsckIssue::new(
                    Severity::Error,
                    Category::ChunkIndex,
                    format!("chunk {key}: descriptor decode failed: {e:?}"),
                ));
                continue;
            }
        };
        if desc.validate(&ctx.limits()).is_err() {
            ctx.issues.push(FsckIssue::new(
                Severity::Error,
                Category::ChunkIndex,
                format!("chunk {key}: descriptor validation failed"),
            ));
        }
        if ctx.options.verify_materialized {
            let limits = ctx.limits();
            match crate::integrity::content::verify_descriptor(&desc, &key, ctx, &limits) {
                Ok(_) => {}
                Err(e) => ctx.issues.push(FsckIssue::new(
                    Severity::Error,
                    Category::ChunkIndex,
                    format!("chunk {key}: materialized content verification failed: {e}"),
                )),
            }
        }
        ctx.chunk_descriptors_verified += 1;
    }
    Ok(())
}

/// Verify snapshot entries: decode, root object present.
///
/// A snapshot's root object must exist in a segment; the graph phase
/// independently walks that root for reachability. A snapshot whose root
/// vanished would silently lose the pinned state it promises.
fn verify_snapshots(ctx: &mut FsckCtx) -> Result<(), String> {
    let root = match &ctx.root {
        Some(r) => r,
        None => return Ok(()),
    };
    if root.snapshot_tree_root.is_zero() {
        return Ok(());
    }
    let entries = index::scan_all(
        root.snapshot_tree_root,
        crate::store::BTREE_ORDER,
        ctx.options.max_fanout,
        ctx,
    )
    .map_err(|e| format!("snapshot tree scan: {e}"))?;
    for (name, v) in entries {
        let entry = match crate::store::snapshot::SnapshotEntry::decode(&v) {
            Ok(e) => e,
            Err(e) => {
                ctx.issues.push(FsckIssue::new(
                    Severity::Error,
                    Category::Snapshot,
                    format!(
                        "snapshot {:?}: entry decode failed: {e:?}",
                        String::from_utf8_lossy(&name)
                    ),
                ));
                continue;
            }
        };
        if !ctx.object_index.contains(&entry.root_id) {
            ctx.issues.push(FsckIssue::new(
                Severity::Error,
                Category::Snapshot,
                format!(
                    "snapshot {:?}: root object {} missing",
                    String::from_utf8_lossy(&name),
                    entry.root_id
                ),
            ));
        }
    }
    Ok(())
}

/// Count directory references per inode and compare with nlink.
///
/// # What is checked (and why only this direction)
///
/// Re-scan every directory tree, count the decodable `DirEntry`s per
/// target ino, and warn when the count EXCEEDS `nlink`: every directory
/// reference must be accounted in the inode's link count, so a deficit
/// means the link/unlink path under-counted (or the entry is corrupt).
///
/// The reverse direction (count < nlink) is NOT reported: directories
/// carry `.`/`..` self-links in `nlink` (>= 2 for a single parent), so
/// refs < nlink is legal and expected.
///
/// # Units
///
/// `refs` counts directory entries; `nlink` is the inode's on-disk
/// u32 link count. Entries that fail `DirEntry::decode` are skipped
/// (they were already reported by `verify_directories`).
fn verify_reference_counts(ctx: &mut FsckCtx, inodes: &HashMap<u64, Inode>) -> Result<(), String> {
    let mut refs: HashMap<u64, u64> = HashMap::new();
    for (&ino, inode) in inodes {
        if let InodeData::Directory { dir_root } = &inode.data {
            if dir_root.is_zero() {
                continue;
            }
            let entries = index::scan_all(
                *dir_root,
                crate::store::BTREE_ORDER,
                ctx.options.max_fanout,
                ctx,
            )
            .map_err(|e| format!("directory {ino} scan (refcounts): {e}"))?;
            for (_, v) in entries {
                if let Ok(e) = DirEntry::decode(&v) {
                    *refs.entry(e.ino).or_insert(0) += 1;
                }
            }
        }
    }
    for (&ino, &count) in refs.iter() {
        if let Some(inode) = inodes.get(&ino) {
            if count > inode.nlink as u64 {
                ctx.issues.push(FsckIssue::new(
                    Severity::Warning,
                    Category::Inode,
                    format!(
                        "inode {ino}: nlink {} is below the {} directory references",
                        inode.nlink, count
                    ),
                ));
            }
        }
    }
    Ok(())
}
