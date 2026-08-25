//! fsck graph phase: independent reachability walk from all roots.
//!
//! The walk is implemented here (not by reusing `store::gc::mark_live`) so
//! that fsck independently verifies the store's own idea of reachability.
//! Everything in the derived object index that is not reachable from the
//! active root (or a snapshot root) is leaked.

#![forbid(unsafe_code)]

use std::collections::{HashSet, VecDeque};

use crate::core::extent::ChunkId;
use crate::core::materialize::DecoderContext;
use crate::core::representation::{Representation, Residual};
use crate::store::inode::{Inode, InodeData};
use crate::store::root::Root;

use super::scan::FsckCtx;
use super::{Category, FsckIssue, Severity};

/// How a marked object is interpreted during the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkKind {
    /// Filesystem root object.
    Root,
    /// Inode object (walk its trees).
    Inode,
    /// B-tree node whose leaf values are inode ids.
    TreeInodeIndex,
    /// B-tree node whose leaf values are directory entries.
    TreeDirectory,
    /// B-tree node whose leaf values are extent descriptors.
    TreeExtent,
    /// B-tree node whose leaf values are chunk descriptors.
    TreeChunkIndex,
    /// B-tree node whose leaf values are snapshot entries.
    TreeSnapshot,
    /// B-tree node whose leaf values are xattr values.
    TreeXattr,
    /// Plain data/model object.
    Object,
}

/// Walk the graph and return the live object set.
pub fn mark_live(ctx: &mut FsckCtx) -> Result<HashSet<ChunkId>, String> {
    let mut live: HashSet<ChunkId> = HashSet::new();
    let mut work: VecDeque<(ChunkId, MarkKind)> = VecDeque::new();
    let root = ctx
        .root
        .as_ref()
        .ok_or_else(|| "cannot walk without a valid root".to_string())?;

    // Active root object.
    work.push_back((ctx.active.root_object_id, MarkKind::Root));
    // Snapshot roots: walk the snapshot tree first so their root objects
    // are discovered before the main sweep.
    let mut snapshot_roots: Vec<ChunkId> = Vec::new();
    if !root.snapshot_tree_root.is_zero() {
        collect_snapshot_roots(ctx, root.snapshot_tree_root, &mut snapshot_roots, &mut work)?;
    }
    for sid in snapshot_roots {
        work.push_back((sid, MarkKind::Root));
    }

    while let Some((id, kind)) = work.pop_front() {
        if !live.insert(id) {
            continue;
        }
        match kind {
            MarkKind::Root => {
                let r = fetch_root(ctx, &id)?;
                push_tree(&mut work, r.inode_index_root, MarkKind::TreeInodeIndex);
                push_tree(&mut work, r.chunk_index_root, MarkKind::TreeChunkIndex);
                push_tree(&mut work, r.snapshot_tree_root, MarkKind::TreeSnapshot);
                push_tree(&mut work, r.model_index_root, MarkKind::TreeChunkIndex);
            }
            MarkKind::Inode => {
                let inode = fetch_inode(ctx, &id)?;
                push_tree(&mut work, inode.xattr_root, MarkKind::TreeXattr);
                match &inode.data {
                    InodeData::Directory { dir_root } => {
                        push_tree(&mut work, *dir_root, MarkKind::TreeDirectory);
                    }
                    InodeData::File { extent_root } => {
                        push_tree(&mut work, *extent_root, MarkKind::TreeExtent);
                    }
                    _ => {}
                }
            }
            MarkKind::TreeInodeIndex => {
                walk_tree(ctx, &id, TreeValue::InodeId, &mut live, &mut work)?;
            }
            MarkKind::TreeDirectory => {
                walk_tree(ctx, &id, TreeValue::Directory, &mut live, &mut work)?;
            }
            MarkKind::TreeExtent => {
                walk_tree(ctx, &id, TreeValue::ExtentDescriptor, &mut live, &mut work)?;
            }
            MarkKind::TreeChunkIndex => {
                walk_tree(ctx, &id, TreeValue::ChunkDescriptor, &mut live, &mut work)?;
            }
            MarkKind::TreeSnapshot => {
                walk_tree(ctx, &id, TreeValue::Snapshot, &mut live, &mut work)?;
            }
            MarkKind::TreeXattr => {
                walk_tree(ctx, &id, TreeValue::Xattr, &mut live, &mut work)?;
            }
            MarkKind::Object => {}
        }
    }
    Ok(live)
}

/// Compute leaked (unreachable) objects and their bytes.
pub fn leaked(ctx: &FsckCtx, live: &HashSet<ChunkId>) -> (u64, u64) {
    let mut count = 0u64;
    let mut bytes = 0u64;
    for (id, loc) in ctx.object_index.iter() {
        if !live.contains(&id) {
            count += 1;
            bytes += loc.total_size();
        }
    }
    (count, bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeValue {
    InodeId,
    Directory,
    ExtentDescriptor,
    ChunkDescriptor,
    Snapshot,
    Xattr,
}

fn push_tree(work: &mut VecDeque<(ChunkId, MarkKind)>, root: ChunkId, kind: MarkKind) {
    if !root.is_zero() {
        work.push_back((root, kind));
    }
}

fn fetch_root(ctx: &FsckCtx, id: &ChunkId) -> Result<Root, String> {
    let bytes = ctx
        .fetch_object(id)
        .map_err(|e| format!("root object {id}: {e}"))?;
    Root::decode(&bytes).map_err(|e| format!("root decode {id}: {e:?}"))
}

fn fetch_inode(ctx: &FsckCtx, id: &ChunkId) -> Result<Inode, String> {
    let bytes = ctx
        .fetch_object(id)
        .map_err(|e| format!("inode object {id}: {e}"))?;
    Inode::decode(&bytes).map_err(|e| format!("inode decode {id}: {e:?}"))
}

fn walk_tree(
    ctx: &mut FsckCtx,
    node_id: &ChunkId,
    value_kind: TreeValue,
    live: &mut HashSet<ChunkId>,
    work: &mut VecDeque<(ChunkId, MarkKind)>,
) -> Result<(), String> {
    if node_id.is_zero() {
        return Ok(());
    }
    let payload = ctx
        .fetch_object(node_id)
        .map_err(|e| format!("tree node {node_id}: {e}"))?;
    let node = crate::store::index::Node::decode(
        &payload,
        crate::store::BTREE_ORDER,
        ctx.max_records_per_segment as u32,
    )
    .map_err(|e| format!("tree node decode {node_id}: {e:?}"))?;
    match node {
        crate::store::index::Node::Internal {
            first_child,
            entries,
        } => {
            let kind = match value_kind {
                TreeValue::InodeId => MarkKind::TreeInodeIndex,
                TreeValue::Directory => MarkKind::TreeDirectory,
                TreeValue::ExtentDescriptor => MarkKind::TreeExtent,
                TreeValue::ChunkDescriptor => MarkKind::TreeChunkIndex,
                TreeValue::Snapshot => MarkKind::TreeSnapshot,
                TreeValue::Xattr => MarkKind::TreeXattr,
            };
            work.push_back((first_child, kind));
            for e in entries {
                let child = ChunkId::new(
                    e.value
                        .as_slice()
                        .try_into()
                        .map_err(|_| format!("internal child id not 32 bytes in {node_id}"))?,
                );
                work.push_back((child, kind));
            }
        }
        crate::store::index::Node::Leaf { entries } => {
            for e in entries {
                match value_kind {
                    TreeValue::InodeId => {
                        let ino_id = ChunkId::new(
                            e.value
                                .as_slice()
                                .try_into()
                                .map_err(|_| format!("inode value not 32 bytes in {node_id}"))?,
                        );
                        work.push_back((ino_id, MarkKind::Inode));
                    }
                    TreeValue::Directory | TreeValue::Xattr => {}
                    TreeValue::ExtentDescriptor | TreeValue::ChunkDescriptor => {
                        mark_descriptor_refs(ctx, &e.value, live, work)?;
                    }
                    TreeValue::Snapshot => {
                        let entry = crate::store::snapshot::SnapshotEntry::decode(&e.value)
                            .map_err(|e| format!("snapshot entry decode: {e:?}"))?;
                        work.push_back((entry.root_id, MarkKind::Root));
                    }
                }
            }
        }
    }
    Ok(())
}

fn mark_descriptor_refs(
    ctx: &mut FsckCtx,
    bytes: &[u8],
    live: &mut HashSet<ChunkId>,
    work: &mut VecDeque<(ChunkId, MarkKind)>,
) -> Result<(), String> {
    let o = &ctx.options;
    let desc = match crate::format::descriptor::decode(
        bytes,
        o.max_descriptor_bytes,
        o.max_inline_bytes,
        o.max_palette,
        o.max_period,
        o.max_chunk_size,
    ) {
        Ok(d) => d,
        // A corrupt descriptor cannot be walked; report it as an issue and
        // continue (fsck must never abort on one bad record).
        Err(e) => {
            ctx.issues.push(FsckIssue::new(
                Severity::Error,
                Category::Reference,
                format!("descriptor decode failed during reachability walk: {e:?}"),
            ));
            return Ok(());
        }
    };
    let mut refs = Vec::new();
    match &desc {
        Representation::Raw { obj, .. } => refs.push(*obj),
        Representation::Rans { model, enc_obj, .. } => {
            refs.push(*model);
            refs.push(*enc_obj);
        }
        Representation::SequenceRans { model, enc_obj, .. } => {
            refs.push(*model);
            refs.push(*enc_obj);
        }
        Representation::BaseResidual {
            residual: Residual::RansCoded { enc_obj, model, .. },
            ..
        }
        | Representation::EntropyRef {
            residual: Residual::RansCoded { enc_obj, model, .. },
            ..
        } => {
            refs.push(*enc_obj);
            refs.push(*model);
        }
        Representation::BaseResidual {
            residual: Residual::BaseSequence { enc_obj, model, .. },
            ..
        } => {
            refs.push(*enc_obj);
            refs.push(*model);
        }
        _ => {}
    }
    for r in refs {
        if live.insert(r) {
            work.push_back((r, MarkKind::Object));
        }
    }
    Ok(())
}

/// Collect snapshot root ids by walking the snapshot tree.
fn collect_snapshot_roots(
    ctx: &FsckCtx,
    tree_root: ChunkId,
    out: &mut Vec<ChunkId>,
    work: &mut VecDeque<(ChunkId, MarkKind)>,
) -> Result<(), String> {
    // The snapshot tree nodes themselves are marked during the main sweep
    // (via MarkKind::TreeSnapshot); here we only need the entries' roots.
    let mut queue = VecDeque::from([tree_root]);
    while let Some(node_id) = queue.pop_front() {
        let payload = ctx
            .fetch_object(&node_id)
            .map_err(|e| format!("snapshot tree node {node_id}: {e}"))?;
        let node = crate::store::index::Node::decode(
            &payload,
            crate::store::BTREE_ORDER,
            ctx.max_records_per_segment as u32,
        )
        .map_err(|e| format!("snapshot tree decode: {e:?}"))?;
        match node {
            crate::store::index::Node::Internal {
                first_child,
                entries,
            } => {
                queue.push_back(first_child);
                for e in entries {
                    queue.push_back(ChunkId::new(
                        e.value
                            .as_slice()
                            .try_into()
                            .map_err(|_| "snapshot child id not 32 bytes".to_string())?,
                    ));
                }
            }
            crate::store::index::Node::Leaf { entries } => {
                for e in entries {
                    let entry = crate::store::snapshot::SnapshotEntry::decode(&e.value)
                        .map_err(|e| format!("snapshot entry decode: {e:?}"))?;
                    out.push(entry.root_id);
                    // Also record the snapshot tree for the main sweep.
                    work.push_back((tree_root, MarkKind::TreeSnapshot));
                }
            }
        }
    }
    Ok(())
}

/// Report leaked objects into the issue list.
pub fn report_leaks(ctx: &mut FsckCtx, live: &HashSet<ChunkId>) -> Result<(), String> {
    let (count, bytes) = leaked(ctx, live);
    ctx.leaked_bytes = bytes;
    if count > 0 {
        ctx.issues.push(FsckIssue::new(
            Severity::Warning,
            Category::Reachability,
            format!("{count} unreachable objects ({bytes} bytes) — GC reclaimable"),
        ));
    }
    Ok(())
}

/// Record a missing-reference issue (used by verify).
pub fn issue_missing(ctx: &mut FsckCtx, what: &str, id: &ChunkId) {
    ctx.issues.push(FsckIssue::new(
        Severity::Error,
        Category::Reference,
        format!("{what} {id} is referenced but not present in any segment"),
    ));
}

/// Record a cycle issue (defensive; depth caps prevent infinite walks).
pub fn issue_cycle(ctx: &mut FsckCtx, what: &str, id: &ChunkId) {
    ctx.issues.push(FsckIssue::new(
        Severity::Error,
        Category::Graph,
        format!("{what} {id} revisited during a single walk (cycle)"),
    ));
}
