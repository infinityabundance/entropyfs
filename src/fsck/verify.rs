//! fsck verify phase: semantic invariant checks over the scanned image.
//!
//! Checks inode invariants, directory invariants, extent ordering and
//! non-overlap, descriptor validity, reference resolvability, snapshot
//! roots, the chunk-index content binding, and hard-link reference counts
//! (`docs/recovery/fsck.md`).

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

/// Run all semantic verification phases.
pub fn verify_all(ctx: &mut FsckCtx) -> Result<(), String> {
    verify_superblock_features(ctx);
    let inodes = verify_inodes(ctx)?;
    verify_directories(ctx, &inodes)?;
    verify_extents(ctx, &inodes)?;
    verify_chunk_index(ctx)?;
    verify_snapshots(ctx)?;
    verify_reference_counts(ctx, &inodes)?;
    Ok(())
}

fn verify_superblock_features(ctx: &mut FsckCtx) {
    match crate::format::features::check(ctx.active.features(), false) {
        crate::format::features::Compatibility::Ok => {}
        crate::format::features::Compatibility::ReadOnlyOnly => {
            ctx.issues.push(FsckIssue::new(
                Severity::Warning,
                Category::Superblock,
                "store carries unknown ro_compat features".to_string(),
            ));
        }
        crate::format::features::Compatibility::Refused(msg) => {
            ctx.issues.push(FsckIssue::new(
                Severity::Error,
                Category::Superblock,
                format!("store features refuse mount: {msg}"),
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
