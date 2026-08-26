//! fsck graph phase: independent reachability walk from all roots.
//!
//! The walk is implemented here (not by reusing `store::gc::mark_live`) so
//! that fsck independently verifies the store's own idea of reachability.
//! Everything in the derived object index that is not reachable from the
//! active root (or a snapshot root) is leaked.
//!
//! # PURPOSE
//!
//! Compute the live object set of a scanned store image: everything
//! reachable from the active root and every snapshot root, walking the
//! persistent B-trees and descriptor references exactly as the store
//! would — but through fsck's raw scan (`scan::FsckCtx`), never through
//! the mounted `Store` API. The complement — objects present in the
//! derived index but unreached — is the leak report: the
//! GC-reclaimable set (`docs/recovery/fsck.md` §1).
//!
//! # BOUNDARY
//!
//! Knows only the scanned image: `FsckCtx` (rebuilt object index, decoded
//! active root, segment payload reads, fsck's resource limits). It reads
//! roots, inodes, tree nodes, and descriptors ONLY to extract child ids;
//! it does not judge descriptor semantics (that is `verify.rs`'s job). It
//! never opens the mounted store, never observes epoch state, and never
//! mutates anything (`FsckCtx::put` is `unreachable!`).
//!
//! # MODEL
//!
//! A content-addressed object graph: every object's id is BLAKE3 of its
//! own bytes, and the only persistent pointers are (a) tree roots inside
//! the root object and inodes, (b) child ids inside internal B-tree
//! nodes, (c) inode ids in the inode-index leaves, and (d) model and
//! encoded-object ids inside extent/chunk-index descriptors. Reachability
//! from ALL roots is the only truth (`gc.md` §1: reference counts are
//! hints only).
//!
//! # CORRECTNESS INVARIANTS
//!
//! - The live set is root-closed: the active root object, every snapshot
//!   root, and everything reached from them is live.
//! - Every object the walk expands exists in a segment. An unreadable
//!   root, inode, or tree node is a HARD error that aborts the walk:
//!   with reachability uncomputable, a leak list would be a false
//!   accusation.
//! - A corrupt descriptor inside an extent/chunk leaf is downgraded to an
//!   issue and the walk continues — fsck never aborts on one bad record.
//! - The walk terminates: `live.insert` dedups, so every object is
//!   expanded at most once. Content-addressed graphs cannot contain
//!   cycles among valid ids (an object's id is a hash of its own bytes);
//!   the same dedup is the shared-subtree guard.
//!
//! # CONCURRENCY
//!
//! Single-threaded; `&mut FsckCtx` throughout. No locks.
//!
//! # DURABILITY
//!
//! Read-only: nothing here persists or acknowledges durability. The leak
//! report only informs later `--repair` decisions.
//!
//! # RESOURCE BOUNDS (hostile-media safety)
//!
//! The worklist holds at most one entry per distinct object (each is
//! expanded once), tree nodes decode with the entry count capped at
//! `max_fanout` and strictly-increasing keys enforced at `Node::decode`,
//! and descriptors decode under `FsckCtx::limits()`. Per-record work and
//! allocations are therefore bounded; total work is O(distinct objects)
//! with one payload fetch per object. Memory: the live set plus one
//! decoded node at a time.
//!
//! # PERFORMANCE
//!
//! Single-pass BFS (`VecDeque` worklist). Deliberately NOT
//! `store::gc::mark_live`: fsck must independently verify the store's own
//! idea of reachability — sharing the implementation would let a
//! store-side reachability bug hide from fsck (`docs/recovery/fsck.md`
//! §1: "it does not merely call the happy-path mounted APIs"). Snapshot
//! roots are pre-collected before the main sweep so they seed the walk up
//! front; the snapshot tree's own nodes are still swept during the main
//! walk.
//!
//! # FAILURE MODES
//!
//! - Hard `Err`: fetch/decode failure of a root, inode, or tree node;
//!   propagates and aborts fsck.
//! - Issue + continue: descriptor decode failure in a leaf
//!   (Category::Reference, Severity::Error); the object's refs are
//!   unknown, so the walk skips them rather than guess.
//! - Warning: leaked objects (Category::Reachability) — expected on any
//!   store with GC-reclaimable history.
//!
//! # HISTORY / EVIDENCE
//!
//! fsck exists because the store's own APIs cannot certify themselves
//! (`docs/recovery/fsck.md`; ADR-0011's chain: physical record →
//! descriptor → materialized bytes → logical content hash →
//! reachability). The hostile-media court (Phase 11A) drives fsck as one
//! of its targets, with B-tree exhibits (fanout 4096/4097, unsorted and
//! duplicate keys) that `Node::decode` must reject. The fsck-vs-runtime
//! agreement test pins the leak report to GC's accounting.

#![forbid(unsafe_code)]

use std::collections::{HashSet, VecDeque};

use crate::core::extent::ChunkId;
use crate::core::materialize::DecoderContext;
use crate::core::representation::{Representation, Residual};
use crate::store::inode::{Inode, InodeData};
use crate::store::root::Root;

use super::scan::FsckCtx;
use super::{Category, FsckIssue, Severity};

/// How a marked object is interpreted during the walk: the kind decides
/// what the object references and therefore what gets pushed next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkKind {
    /// Filesystem root object (the active root or a snapshot root).
    ///
    /// Pushes the four tree roots it points at: inode index, chunk
    /// index, snapshot tree, and model index (the `Tree*` kinds below).
    Root,
    /// Inode object (walk its trees).
    ///
    /// Pushes its xattr tree and, by data kind, its directory or extent
    /// tree.
    Inode,
    /// B-tree node whose leaf values are inode ids (inode index).
    ///
    /// Leaves push each inode id as [`MarkKind::Inode`].
    TreeInodeIndex,
    /// B-tree node whose leaf values are directory entries.
    ///
    /// Entries are `(ino, d_type)` pairs; the inos are semantic (checked
    /// by `verify.rs`), not object ids, so there is nothing to push.
    TreeDirectory,
    /// B-tree node whose leaf values are extent descriptors.
    ///
    /// Leaves decode each descriptor and push its model and
    /// encoded-object ids (see `mark_descriptor_refs`).
    TreeExtent,
    /// B-tree node whose leaf values are chunk descriptors (chunk index
    /// and model index — both map a `[u8;32]` id to a descriptor).
    TreeChunkIndex,
    /// B-tree node whose leaf values are snapshot entries.
    ///
    /// Leaves push each entry's root id as [`MarkKind::Root`].
    TreeSnapshot,
    /// B-tree node whose leaf values are xattr values (inline).
    ///
    /// Xattr values are inline bytes; there is nothing to push.
    TreeXattr,
    /// Plain data/model object — terminal: popping it pushes nothing.
    Object,
}

/// Walk the graph and return the live object set.
///
/// # What / Why
///
/// The fsck-side mirror of `store::gc::mark_live`: an independent,
/// raw-scan-based reachability computation. The returned set is the
/// "reachable from any root" truth against which the rebuilt object
/// index is compared by [`leaked`].
///
/// # Algorithm
///
/// BFS over a `(ChunkId, MarkKind)` worklist: pop → `live.insert` (the
/// dedup that makes the walk terminate) → expand by kind. A root pushes
/// its four trees; an inode pushes its xattr/dir/extent trees; a tree
/// node decodes and pushes its children; an extent/chunk leaf pushes its
/// descriptor's object refs.
///
/// # Failure behavior
///
/// Unreadable roots, inodes, or tree nodes abort with `Err`
/// (reachability is then unknowable and a leak list would be a false
/// accusation). Corrupt descriptors inside leaves are issues and
/// skipped — the walk never aborts on one bad record.
///
/// # Resource bounds
///
/// Each object is expanded at most once, so the worklist is bounded by
/// the number of distinct objects and total work is O(distinct objects).
pub fn mark_live(ctx: &mut FsckCtx) -> Result<HashSet<ChunkId>, String> {
    let mut live: HashSet<ChunkId> = HashSet::new();
    let mut work: VecDeque<(ChunkId, MarkKind)> = VecDeque::new();
    let root = ctx
        .root
        .as_ref()
        .ok_or_else(|| "cannot walk without a valid root".to_string())?;

    // -----------------------------------------------------------------
    // Stage 1: Seed the worklist with every root.
    //
    // The active root object and every snapshot root are the sources of
    // truth; anything not reachable from these is garbage by definition
    // (gc.md §1 — reachability is the only truth, reference counts are
    // hints). Snapshot roots are collected by walking the snapshot tree
    // BEFORE the main sweep so they are discovered up front.
    // -----------------------------------------------------------------
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

    // -----------------------------------------------------------------
    // Stage 2: Trace the graph.
    //
    // Pop → mark (the `live.insert` dedup is the termination and
    // shared-subtree guard) → expand by kind. Expansion never recurses;
    // all pending work lives in the bounded worklist.
    // -----------------------------------------------------------------
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
///
/// Every object in the derived index the walk did not reach is leaked:
/// GC-reclaimable. Returns `(count, bytes)` where `count` is the number
/// of unreachable objects and `bytes` is their total PHYSICAL on-disk
/// footprint (`Location::total_size` = record header + stored payload,
/// bytes) — the same physical accounting GC's reconciliation uses, so
/// the fsck leak report agrees with GC's (`docs/recovery/fsck.md` §5).
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

/// The value type a tree's leaves carry: decides how leaf values are
/// interpreted during the walk.
///
/// - `InodeId` → each leaf value is a 32-byte inode object id.
/// - `Directory` / `Xattr` → inline values; nothing to push.
/// - `ExtentDescriptor` / `ChunkDescriptor` → leaf values are descriptor
///   bytes; their object refs are marked via `mark_descriptor_refs`.
/// - `Snapshot` → each leaf value is a `SnapshotEntry` whose `root_id` is
///   pushed as a root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeValue {
    InodeId,
    Directory,
    ExtentDescriptor,
    ChunkDescriptor,
    Snapshot,
    Xattr,
}

/// Push a tree root onto the worklist unless it is the zero id (the
/// all-zero id encodes "no such tree").
fn push_tree(work: &mut VecDeque<(ChunkId, MarkKind)>, root: ChunkId, kind: MarkKind) {
    if !root.is_zero() {
        work.push_back((root, kind));
    }
}

/// Fetch and decode a root object by id.
///
/// Hard error when the object is absent or undecodable: a reachable root
/// that cannot be read makes the whole walk unknowable.
fn fetch_root(ctx: &FsckCtx, id: &ChunkId) -> Result<Root, String> {
    let bytes = ctx
        .fetch_object(id)
        .map_err(|e| format!("root object {id}: {e}"))?;
    Root::decode(&bytes).map_err(|e| format!("root decode {id}: {e:?}"))
}

/// Fetch and decode an inode object by id (same hard-error policy as
/// [`fetch_root`]).
fn fetch_inode(ctx: &FsckCtx, id: &ChunkId) -> Result<Inode, String> {
    let bytes = ctx
        .fetch_object(id)
        .map_err(|e| format!("inode object {id}: {e}"))?;
    Inode::decode(&bytes).map_err(|e| format!("inode decode {id}: {e:?}"))
}

/// Expand one B-tree node: decode it (entry count capped at
/// `max_fanout`; keys strictly increasing, enforced at decode), push
/// internal children, and interpret leaf values by `value_kind`.
///
/// # Failure behavior
///
/// Fetch/decode failures are HARD errors: an unreadable node in a
/// reachable tree means reachability cannot be computed. Descriptor
/// failures inside leaves are NOT hard errors — `mark_descriptor_refs`
/// reports them as issues and the walk continues.
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

/// Extract and mark the objects an extent/chunk-index descriptor
/// references: the raw object (RAW), or the model + encoded object for
/// every coding family; the residual-coded families contribute their
/// `enc_obj`/`model` too. Inline-class representations reference no
/// objects (the catch-all `_` arm).
///
/// Reference objects are terminal (`MarkKind::Object`): they are inserted
/// into `live` here and pushed as work so they end up in the returned
/// set.
///
/// # Why a decode failure is an issue, not a hard error
///
/// A corrupt descriptor in ONE leaf must not abort the whole store's
/// walk: reachability of every other tree remains computable, and the
/// corruption is itself a finding. The object's refs are unknown, so the
/// walk skips them rather than guess.
fn mark_descriptor_refs(
    ctx: &mut FsckCtx,
    bytes: &[u8],
    live: &mut HashSet<ChunkId>,
    work: &mut VecDeque<(ChunkId, MarkKind)>,
) -> Result<(), String> {
    let desc = match crate::format::descriptor::decode(bytes, &ctx.limits()) {
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
        Representation::SparseBlock64 { model, enc_obj, .. } => {
            refs.push(*model);
            refs.push(*enc_obj);
        }
        Representation::SequenceDict { model, enc_obj, .. } => {
            refs.push(*model);
            refs.push(*enc_obj);
        }
        Representation::SequenceSharedDict { model, enc_obj, .. } => {
            refs.push(*model);
            refs.push(*enc_obj);
        }
        Representation::SequenceDeep { model, enc_obj, .. } => {
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

/// Collect every snapshot root id by walking the snapshot tree.
///
/// Runs BEFORE the main sweep so snapshot root objects seed the
/// worklist up front (Stage 1 of [`mark_live`]). The snapshot tree's own
/// nodes are still marked during the main sweep
/// (`MarkKind::TreeSnapshot`); the `work` pushes below only ensure the
/// tree is swept — duplicate pushes are absorbed by `live.insert`.
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
///
/// Emits one Reachability warning when any object is unreachable and
/// stashes the physical byte total in `ctx.leaked_bytes` for the report
/// (bytes: record header + stored payload, `Location::total_size`).
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

/// Record a missing-reference issue: `what` (e.g. "inode 5") references
/// `id`, but no segment contains it. Referencing an object that does not
/// exist is corruption — such a reference could never materialize
/// (Category::Reference, Severity::Error).
pub fn issue_missing(ctx: &mut FsckCtx, what: &str, id: &ChunkId) {
    ctx.issues.push(FsckIssue::new(
        Severity::Error,
        Category::Reference,
        format!("{what} {id} is referenced but not present in any segment"),
    ));
}

/// Record a cycle issue (defensive; depth caps prevent infinite walks).
///
/// [`mark_live`] dedups with one global `live` set, so a revisit is
/// silently absorbed and this verb is NOT emitted by the current walk; it
/// exists for walkers that track per-path visitation, where a revisit
/// would be a genuine cycle. Content-addressed graphs cannot contain
/// cycles among valid ids (an object's id is a hash of its own bytes), so
/// this must never fire on a well-formed store (Category::Graph,
/// Severity::Error).
pub fn issue_cycle(ctx: &mut FsckCtx, what: &str, id: &ChunkId) {
    ctx.issues.push(FsckIssue::new(
        Severity::Error,
        Category::Graph,
        format!("{what} {id} revisited during a single walk (cycle)"),
    ));
}
