//! Reachability GC (ADR-0009, `docs/architecture/gc.md`).
//!
//! Mark from all roots (current + snapshots) through the object graph;
//! compute per-segment live ratios; copy live records from low-utilization
//! segments; commit the new root; delete obsolete segments only after the
//! new root is durable (`BEFORE_OLD_SEGMENT_DELETE` is a crash-court
//! boundary).
//!
//! # Purpose
//!
//! The store is append-only: every mutation appends new records and leaves
//! the superseded ones in place (transaction-model.md §1). Unreachable
//! records are reclaimable space, and reachability from ALL roots (current
//! root + every snapshot root) is the only source of truth — reference
//! counts are hints only (gc.md §1). This module implements the tracing
//! mark-and-sweep with compaction: mark the live object set, choose victim
//! segments, copy their live records into a fresh segment, rebuild the
//! derived chunk index from reachability, publish a new root, and only
//! then delete the obsolete segments.
//!
//! # Boundary
//!
//! GC reads committed state: roots, the derived `ObjectIndex`, segment
//! files, and config limits. It writes fresh segment records, a new root,
//! and superblock slots (through `Store`), and it deletes old segment
//! files. It must NOT observe in-flight epoch state: the reachability walk
//! sees only committed roots, and epoch-staged objects are referenced only
//! by the mutation log (Phase-10D), so `collect` forces one checkpoint
//! before marking. It must never compact a segment while a foreground
//! writer is appending to it — the segment-writer replacement and index
//! pruning performed here are offline operations (`Store::install_segment`,
//! `ObjectIndex::remove`).
//!
//! # Model
//!
//! MARK: trace from all roots through the object graph (inodes, B-tree
//! nodes, extent descriptors, payload/model objects). SWEEP: pick victim
//! segments by live ratio. COMPACT: copy the victims' live records plus a
//! rebuilt chunk index into a fresh segment, commit a new root, delete the
//! victims.
//!
//! The chunk index is a DERIVED structure (§34): it is disposable and is
//! rebuilt from reachability during compaction rather than migrated. GC's
//! overall job is PHYSICAL CONVERGENCE (Phase-9H): the backing must
//! converge to the reachable persistent state plus bounded format
//! overhead — `compact_full` achieves exactly that and is idempotent.
//!
//! # Persistent authority
//!
//! GC changes on-disk semantics: it appends records, publishes a new root
//! through the two-slot superblock protocol, and deletes victim segments.
//! The delete is the dangerous step: it runs only after the new root is
//! durable (`BEFORE_OLD_SEGMENT_DELETE` crash point), so a crash leaves
//! either the old root with its old segments intact or the new root with
//! the old segments as garbage — both correct (ADR-0008, gc.md §3).
//!
//! # Correctness invariants
//!
//! - Reachability from all roots is the only truth; the mark walk must
//!   close over trees, inodes, descriptor object refs, and the
//!   EXACT_REF / BASE_RESIDUAL reference chains (bounded by
//!   `max_reference_depth`).
//! - The rebuilt chunk index keeps exactly the live extents' descriptors
//!   (dedup hit-ability) plus the transitive reference closure
//!   (decodability), so overwritten, unsnapshotted content ids stop
//!   accumulating entries.
//! - Copied records preserve their envelope flags and materialized length
//!   byte-exactly.
//! - The current root record is NOT re-copied (the fresh root supersedes
//!   it); snapshot roots ARE Root-tagged records and are copied.
//! - Victims are deleted only after the superblock flip is durable.
//! - `compact_full` is idempotent: backing converges to reachable + bounded
//!   overhead and a second pass reclaims ≈ 0.
//!
//! # Concurrency
//!
//! GC runs as an offline maintenance pass (CLI `gc`, benchmark/evidence
//! harnesses). The final publication goes through the commit coordinator
//! (`Store::publish_commit` takes the commit lock); before marking,
//! `collect` flushes the active epoch so epoch-staged objects cannot be
//! misread as garbage. GC must not run concurrently with foreground
//! writers appending to the current segment: the segment install and index
//! pruning are offline by design.
//!
//! # Durability
//!
//! The commit follows transaction-model.md §2 exactly: append records →
//! fdatasync(segment) → fsync(segments dir) → write the inactive superblock
//! slot → fsync(superblock); only then does GC ack (return the reclaimed
//! byte count) and delete the victims (gc.md §3). Crash before the flip:
//! old root, old segments. Crash after: new root, old segments are
//! garbage. Records GC appended but no root references are garbage by
//! definition and are reclaimed by the next pass.
//!
//! # Resource bounds
//!
//! Tree/node decoding is bounded by `limits.max_fanout` and the B-tree
//! depth cap (128, in `store::index`); descriptor decoding by `Limits`
//! (`max_descriptor_bytes`, `max_chunk_size`, ...); reference chains by
//! `Limits::max_reference_depth`. The physical scan is bounded per segment
//! by `config.max_records_per_segment`. The reference-resolution queue is
//! deduped (`seen`), so each content id resolves at most once per pass.
//!
//! # Performance
//!
//! Victim selection measures PHYSICAL occupancy (Phase-9H) rather than
//! index occupancy, and the chunk-index rebuild bulk-loads bottom-up so
//! each final node is staged exactly once — both shaped by the 2.66 MB
//! dead-BtreeNode finding; see HISTORY / EVIDENCE.
//!
//! # Failure modes
//!
//! Missing root/inode/tree objects → `StoreError::Invariant` (persistent
//! corruption; fsck territory). Undecodable descriptors are skipped
//! defensively during marking — a content id that cannot be decoded
//! contributes no refs and cannot pin anything. A mid-file envelope error
//! fails the physical scan. What must NEVER happen: deleting victims
//! before the new root is durable; running the mark while an epoch is
//! active without flushing it first.
//!
//! # History / evidence
//!
//! - Phase-8B (§34): the chunk index is derived; GC rebuilds it from
//!   reachability (`rebuild_chunk_index`).
//! - Phase-9A: `unreachable_bytes_by_record_tag` — the floor diagnosis of
//!   which record class makes up the reachable → backing gap.
//! - Phase-9H (physical convergence, sealed campaign
//!   `evidence/performance/campaign-1787688017-0a03ece/`, revision
//!   `0a03ece`): the derived index can diverge from what is actually on
//!   disk — the chunk-index REBUILD staged every intermediate COW path
//!   version physically (2.66 MB of dead `BtreeNode` records on the real
//!   tree), so index-derived occupancy understated the dead bytes.
//!   `physical::scan_physical` reconciles every segment byte (live /
//!   dead-indexed / index-hidden / unindexed / torn / padding / format).
//!   Fixes: physical victim selection (`physical_ratios`), the `bulk_load`
//!   rebuild (each final node staged exactly once), `compact_full`
//!   (idempotent full compaction), and no re-copy of the current root
//!   record. Measured: tree-court backing 9,129,988 B → 1,100,161 B;
//!   post-GC reconciliation = reachable 1,100,157 B + 0 B dead + 0 B
//!   index-hidden + 0 B unindexed + 4 B format overhead.
//! - Phase-10D: the epoch must be flushed before marking (in-flight objects
//!   are referenced only by the mutation log).

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use crate::core::extent::ChunkId;
use crate::format::codec::CodecError;
use crate::store::Store;
use crate::store::StoreError;
use crate::store::inode::{Inode, InodeData};
use crate::store::object::Location;
use crate::store::root::Root;
use crate::store::segment::{self, SegmentWriter};

/// How a marked object is interpreted during the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkKind {
    /// The filesystem root object.
    Root,
    /// An inode object (walk its trees).
    Inode,
    /// A B-tree node whose leaf values are inode object ids.
    TreeInodeIndex,
    /// A B-tree node whose leaf values are directory entries.
    TreeDirectory,
    /// A B-tree node whose leaf values are extent descriptors.
    TreeExtent,
    /// A B-tree node whose leaf values are chunk descriptors.
    TreeChunkIndex,
    /// A B-tree node whose leaf values are snapshot entries.
    TreeSnapshot,
    /// A B-tree node whose leaf values are xattr values (inline).
    TreeXattr,
    /// A data/model object (leaf; nothing further to walk).
    Object,
}

/// The result of the mark walk: the live object set plus the two derived
/// sets needed to rebuild the chunk index from reachability (Phase-8B,
/// §34). This is the single input that drives both victim selection
/// (`physical_ratios`) and the chunk-index rebuild (`rebuild_chunk_index`),
/// so all three sets must be consistent with the same walk.
///
/// - `referenced`: the transitive closure of content ids that live extents
///   reference (EXACT_REF targets, BASE_RESIDUAL bases, transitively
///   through their descriptors). These entries must survive for
///   decodability.
/// - `live_descriptors`: the descriptor bytes of every live extent. These
///   entries must survive so future identical writes still dedup.
///
/// Everything else in the chunk index is historical metadata from
/// overwritten, unsnapshotted content and must not persist past GC.
pub struct LiveMark {
    /// The live object set (data, models, tree nodes, roots).
    pub live: HashSet<ChunkId>,
    /// Content ids that must resolve through the chunk index.
    pub referenced: HashSet<ChunkId>,
    /// Descriptor bytes of every live extent.
    pub live_descriptors: HashSet<Vec<u8>>,
}

/// Mark the live object set from all roots.
///
/// The chunk index is a *derived* structure (§34): its tree nodes are
/// root-reachable and stay live, but the objects its entries reference are
/// pinned only when the content id is actually referenced by a live extent
/// (an EXACT_REF target or a BASE_RESIDUAL base). Without this, deleted
/// data stays pinned by the ever-growing index and GC could never reclaim
/// it.
pub fn mark_live(store: &Store) -> Result<HashSet<ChunkId>, StoreError> {
    Ok(mark_live_full(store)?.live)
}

/// The full mark walk (see [`LiveMark`]).
pub fn mark_live_full(store: &Store) -> Result<LiveMark, StoreError> {
    let mut live: HashSet<ChunkId> = HashSet::new();
    let mut worklist: Vec<(ChunkId, MarkKind)> = Vec::new();
    // Content ids referenced by live extents (through EXACT_REF targets
    // and BASE_RESIDUAL bases). Resolved through the chunk index after the
    // main walk.
    let mut referenced: HashSet<ChunkId> = HashSet::new();
    // Descriptor bytes of every live extent (for the index rebuild).
    let mut live_descriptors: HashSet<Vec<u8>> = HashSet::new();

    // ---------------------------------------------------------------------
    // Stage 1: Seed the worklist with every root.
    //
    // The current root object and every snapshot root are the sources of
    // truth; anything not reachable from these is garbage by definition
    // (gc.md §1 — reachability is the only truth, reference counts are
    // hints only).
    // ---------------------------------------------------------------------

    // Roots: current root object + snapshot roots.
    worklist.push((store.current_root().id(), MarkKind::Root));
    let snapshots = crate::store::snapshot::list(
        store.current_root().snapshot_tree_root,
        crate::store::BTREE_ORDER,
        store.config().limits.max_fanout,
        store,
    )?;
    for (_, entry) in snapshots {
        worklist.push((entry.root_id, MarkKind::Root));
    }

    // ---------------------------------------------------------------------
    // Stage 2: Trace the object graph from the roots.
    //
    // Every popped object is marked live once (the `live.insert` dedup is
    // also the cycle guard — content-addressed graphs cannot contain
    // cycles, but shared subtrees are visited once regardless). Tree nodes
    // push their children, inodes push their xattr/dir/extent trees,
    // extent descriptors push their payload/model objects and record their
    // referenced content ids + descriptor bytes for the rebuild.
    // ---------------------------------------------------------------------
    while let Some((id, kind)) = worklist.pop() {
        if !live.insert(id) {
            continue;
        }
        match kind {
            MarkKind::Root => {
                let root = decode_root(store, &id)?;
                worklist.push((root.inode_index_root, MarkKind::TreeInodeIndex));
                worklist.push((root.chunk_index_root, MarkKind::TreeChunkIndex));
                if !root.snapshot_tree_root.is_zero() {
                    worklist.push((root.snapshot_tree_root, MarkKind::TreeSnapshot));
                }
                if !root.model_index_root.is_zero() {
                    worklist.push((root.model_index_root, MarkKind::TreeChunkIndex));
                }
            }
            MarkKind::Inode => {
                let inode = decode_inode(store, &id)?;
                if !inode.xattr_root.is_zero() {
                    worklist.push((inode.xattr_root, MarkKind::TreeXattr));
                }
                match &inode.data {
                    InodeData::Directory { dir_root } if !dir_root.is_zero() => {
                        worklist.push((*dir_root, MarkKind::TreeDirectory));
                    }
                    InodeData::File { extent_root } if !extent_root.is_zero() => {
                        worklist.push((*extent_root, MarkKind::TreeExtent));
                    }
                    _ => {}
                }
            }
            MarkKind::TreeInodeIndex => walk_tree(
                store,
                &id,
                TreeValue::InodeId,
                &mut live,
                &mut worklist,
                &mut referenced,
                &mut live_descriptors,
            )?,
            MarkKind::TreeDirectory => walk_tree(
                store,
                &id,
                TreeValue::Directory,
                &mut live,
                &mut worklist,
                &mut referenced,
                &mut live_descriptors,
            )?,
            MarkKind::TreeExtent => walk_tree(
                store,
                &id,
                TreeValue::ExtentDescriptor,
                &mut live,
                &mut worklist,
                &mut referenced,
                &mut live_descriptors,
            )?,
            MarkKind::TreeChunkIndex => walk_tree(
                store,
                &id,
                TreeValue::ChunkIndexEntry,
                &mut live,
                &mut worklist,
                &mut referenced,
                &mut live_descriptors,
            )?,
            MarkKind::TreeSnapshot => walk_tree(
                store,
                &id,
                TreeValue::Snapshot,
                &mut live,
                &mut worklist,
                &mut referenced,
                &mut live_descriptors,
            )?,
            MarkKind::TreeXattr => walk_tree(
                store,
                &id,
                TreeValue::Xattr,
                &mut live,
                &mut worklist,
                &mut referenced,
                &mut live_descriptors,
            )?,
            MarkKind::Object => {}
        }
    }

    // ---------------------------------------------------------------------
    // Stage 3: Resolve extent-referenced content ids through the chunk
    // index.
    //
    // The mark walk recorded which content ids live extents reference; the
    // chunk index's entries pin the objects those ids materialize. Their
    // descriptors pin objects, and their own references (chains of
    // EXACT_REF / BASE_RESIDUAL) are followed, bounded by the depth cap
    // (`Limits::max_reference_depth`); `seen` dedups so each id resolves
    // at most once.
    // ---------------------------------------------------------------------
    // Resolve extent-referenced content ids through the chunk index:
    // their descriptors pin objects, and their own references (chains of
    // EXACT_REF / BASE_RESIDUAL) are followed, bounded by the depth cap.
    let limits = store.config().limits;
    let mut queue: Vec<ChunkId> = referenced.iter().copied().collect();
    let mut seen: HashSet<ChunkId> = HashSet::new();
    while let Some(cid) = queue.pop() {
        if !seen.insert(cid) {
            continue;
        }
        let Some(bytes) = store.chunk_descriptor(&cid)? else {
            continue;
        };
        let desc = match crate::format::descriptor::decode(&bytes, &limits) {
            Ok(d) => d,
            Err(_) => continue,
        };
        mark_descriptor_refs(&bytes, store, &mut live, &mut worklist)?;
        use crate::core::representation::Representation;
        let next = match &desc {
            Representation::ExactRef { target, .. } => Some(*target),
            Representation::BaseResidual { base, .. } => Some(*base),
            _ => None,
        };
        if let Some(n) = next {
            if !seen.contains(&n) {
                queue.push(n);
            }
        }
    }
    Ok(LiveMark {
        live,
        referenced: seen,
        live_descriptors,
    })
}

/// How tree leaf values are interpreted during the mark walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeValue {
    InodeId,
    Directory,
    ExtentDescriptor,
    ChunkIndexEntry,
    Snapshot,
    Xattr,
}

fn walk_tree(
    store: &Store,
    node_id: &ChunkId,
    value_kind: TreeValue,
    live: &mut HashSet<ChunkId>,
    worklist: &mut Vec<(ChunkId, MarkKind)>,
    referenced: &mut HashSet<ChunkId>,
    live_descriptors: &mut HashSet<Vec<u8>>,
) -> Result<(), StoreError> {
    if node_id.is_zero() {
        return Ok(());
    }
    let payload = store
        .fetch_object(node_id)?
        .ok_or_else(|| StoreError::Invariant(format!("missing tree node {node_id}")))?;
    let node = crate::store::index::Node::decode(
        &payload,
        crate::store::BTREE_ORDER,
        store.config().limits.max_fanout,
    )
    .map_err(|e| StoreError::Index(e.to_string()))?;
    match node {
        crate::store::index::Node::Internal {
            first_child,
            entries,
        } => {
            let kind = match value_kind {
                TreeValue::InodeId => MarkKind::TreeInodeIndex,
                TreeValue::Directory => MarkKind::TreeDirectory,
                TreeValue::ExtentDescriptor => MarkKind::TreeExtent,
                TreeValue::ChunkIndexEntry => MarkKind::TreeChunkIndex,
                TreeValue::Snapshot => MarkKind::TreeSnapshot,
                TreeValue::Xattr => MarkKind::TreeXattr,
            };
            worklist.push((first_child, kind));
            for e in entries {
                let child = ChunkId::new(e.value.as_slice().try_into().expect("32-byte id"));
                worklist.push((child, kind));
            }
        }
        crate::store::index::Node::Leaf { entries } => {
            for e in entries {
                match value_kind {
                    TreeValue::InodeId => {
                        let inode_id =
                            ChunkId::new(e.value.as_slice().try_into().map_err(|_| {
                                StoreError::Invariant("inode value not 32 bytes".into())
                            })?);
                        worklist.push((inode_id, MarkKind::Inode));
                    }
                    TreeValue::Directory | TreeValue::Xattr | TreeValue::ChunkIndexEntry => {}
                    TreeValue::ExtentDescriptor => {
                        mark_descriptor_refs(&e.value, store, live, worklist)?;
                        collect_descriptor_refs(&e.value, store, referenced)?;
                        // Retain the exact descriptor bytes: the rebuilt
                        // chunk index must keep this content id so future
                        // identical writes still dedup (Phase-8B).
                        live_descriptors.insert(e.value.clone());
                    }
                    TreeValue::Snapshot => {
                        let entry = crate::store::snapshot::SnapshotEntry::decode(&e.value)
                            .map_err(|e| StoreError::Descriptor(e.to_string()))?;
                        worklist.push((entry.root_id, MarkKind::Root));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Collect the content ids a live extent references through its
/// descriptor (EXACT_REF targets, BASE_RESIDUAL bases). These cids pin
/// their chunk-index entries (and objects) during GC.
fn collect_descriptor_refs(
    bytes: &[u8],
    store: &Store,
    referenced: &mut HashSet<ChunkId>,
) -> Result<(), StoreError> {
    let l = store.config().limits;
    let desc = match crate::format::descriptor::decode(bytes, &l) {
        Ok(d) => d,
        Err(_) => return Ok(()),
    };
    use crate::core::representation::Representation;
    match &desc {
        Representation::ExactRef { target, .. } => {
            referenced.insert(*target);
        }
        Representation::BaseResidual { base, .. } => {
            referenced.insert(*base);
        }
        Representation::SequenceDict { dictionary, .. } => {
            referenced.insert(*dictionary);
        }
        Representation::SequenceSharedDict {
            dictionary, shared, ..
        } => {
            if !dictionary.is_zero() {
                referenced.insert(*dictionary);
            }
            referenced.insert(*shared);
        }
        _ => {}
    }
    Ok(())
}

/// Mark the object references of a descriptor (RAW obj, RANS model+enc,
/// residual model+enc). Chunk references (EXACT_REF targets, bases) are
/// resolved through the chunk index, whose nodes are marked separately.
fn mark_descriptor_refs(
    bytes: &[u8],
    store: &Store,
    live: &mut HashSet<ChunkId>,
    worklist: &mut Vec<(ChunkId, MarkKind)>,
) -> Result<(), StoreError> {
    let l = store.config().limits;
    let desc = match crate::format::descriptor::decode(bytes, &l) {
        Ok(d) => d,
        Err(_) => return Ok(()), // not a descriptor (defensive)
    };
    use crate::core::representation::{Representation, Residual};
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
            worklist.push((r, MarkKind::Object));
        }
    }
    Ok(())
}

fn decode_root(store: &Store, id: &ChunkId) -> Result<Root, StoreError> {
    let payload = store
        .fetch_object(id)?
        .ok_or_else(|| StoreError::Invariant(format!("missing root object {id}")))?;
    Root::decode(&payload).map_err(|e| StoreError::Superblock(e.to_string()))
}

fn decode_inode(store: &Store, id: &ChunkId) -> Result<Inode, StoreError> {
    let payload = store
        .fetch_object(id)?
        .ok_or_else(|| StoreError::Invariant(format!("missing inode object {id}")))?;
    Inode::decode(&payload).map_err(|e| StoreError::Descriptor(e.to_string()))
}

/// Compute per-segment live ratios from the DERIVED OBJECT INDEX
/// (the pre-Phase-9H view, kept for the diagnostic comparison).
///
/// Units: PHYSICAL record bytes (`Location::total_size` = header + stored
/// payload); each indexed record is counted exactly once at its single
/// index location. This view CANNOT see index-hidden or unindexed bytes —
/// that is precisely the divergence Phase-9H measured (2.66 MB of
/// rebuild-staged dead `BtreeNode` records on the real tree were invisible
/// to it); use [`physical_ratios`] when a decision depends on actual disk
/// occupancy.
pub fn live_ratios(
    store: &Store,
    live: &HashSet<ChunkId>,
) -> Result<HashMap<u64, (u64, u64)>, StoreError> {
    // seq -> (live_bytes, total_indexed_bytes); both PHYSICAL record bytes
    // at the index's one location per content id.
    let mut map: HashMap<u64, (u64, u64)> = HashMap::new();
    for (id, loc) in store.object_index().iter() {
        let entry = map.entry(loc.segment_seq).or_insert((0, 0));
        entry.1 += loc.total_size();
        if live.contains(&id) {
            entry.0 += loc.total_size();
        }
    }
    Ok(map)
}

/// Per-segment PHYSICAL live ratios (Phase-9H): the denominator comes
/// from scanning the actual segment files — every valid record, including
/// records the object index no longer represents (duplicates shadowed by
/// a newer location, and unindexed bytes). A segment whose physical
/// occupancy is dominated by garbage is selected as a victim even when
/// the index's one-location view makes it look mostly live.
///
/// # Why the scan, not the index
///
/// Phase-9H (campaign `1787688017-0a03ece`) found the derived index can
/// diverge from what is actually on disk: the GC chunk-index REBUILD used
/// repeated COW inserts and physically staged every intermediate path
/// version — 2.66 MB of dead `BtreeNode` records on the real tree — while
/// the index's one-location-per-content-id view understated exactly those
/// dead bytes (and cannot see index-hidden/unindexed records at all). The
/// denominator therefore comes from `physical::scan_physical`, which
/// reconciles every segment byte (live / dead-indexed / index-hidden /
/// unindexed / torn / padding / format).
///
/// Units: PHYSICAL record bytes. The `total` excludes torn, padding, and
/// format bytes (they reclaim without copy-out; the ratio measures the
/// copy cost of compacting the segment) — mirroring
/// `SegmentPhysical::physical_live_ratio`.
pub fn physical_ratios(
    store: &Store,
    live: &HashSet<ChunkId>,
) -> Result<HashMap<u64, (u64, u64)>, StoreError> {
    let report = crate::store::physical::scan_physical(store, live)?;
    let mut map: HashMap<u64, (u64, u64)> = HashMap::new();
    for seg in &report.segments {
        let total = seg
            .live_bytes
            .saturating_add(seg.dead_indexed_bytes)
            .saturating_add(seg.index_hidden_bytes)
            .saturating_add(seg.unindexed_bytes);
        map.insert(seg.seq, (seg.live_bytes, total));
    }
    Ok(map)
}

/// Collect the object ids of every node in a committed B-tree (for the
/// old chunk index, whose nodes the rebuild replaces).
fn collect_tree_node_ids(
    store: &Store,
    root: &ChunkId,
    out: &mut HashSet<ChunkId>,
) -> Result<(), StoreError> {
    if root.is_zero() {
        return Ok(());
    }
    let mut stack = vec![*root];
    while let Some(id) = stack.pop() {
        if !out.insert(id) {
            continue;
        }
        let payload = store
            .fetch_object(&id)?
            .ok_or_else(|| StoreError::Invariant(format!("missing tree node {id}")))?;
        let node = crate::store::index::Node::decode(
            &payload,
            crate::store::BTREE_ORDER,
            store.config().limits.max_fanout,
        )
        .map_err(|e| StoreError::Index(e.to_string()))?;
        match node {
            crate::store::index::Node::Internal {
                first_child,
                entries,
            } => {
                stack.push(first_child);
                for e in entries {
                    let child = ChunkId::new(e.value.as_slice().try_into().expect("32-byte id"));
                    stack.push(child);
                }
            }
            crate::store::index::Node::Leaf { .. } => {}
        }
    }
    Ok(())
}

/// Staging provider for the rebuilt chunk-index B-tree: `put` appends a
/// BtreeNode record to the GC segment (and registers its location); `get`
/// serves the nodes staged earlier in this pass. Content-addressed: a
/// payload already staged this pass is not appended twice.
struct RebuildProvider<'a> {
    writer: &'a mut SegmentWriter,
    new_seq: u64,
    pending: HashMap<ChunkId, Vec<u8>>,
    new_locations: &'a mut Vec<(ChunkId, Location)>,
}

impl crate::store::index::ObjectProvider for RebuildProvider<'_> {
    fn get(&self, id: &ChunkId) -> Result<Option<Vec<u8>>, crate::store::index::BTreeError> {
        Ok(self.pending.get(id).cloned())
    }

    fn put(&mut self, id: ChunkId, bytes: Vec<u8>) {
        if self.pending.contains_key(&id) {
            return;
        }
        let encoded = crate::format::record::encode(
            crate::format::version::RecordTag::BtreeNode,
            0,
            None,
            &bytes,
        );
        let offset = self.writer.durable_end() + self.writer.buffered_len();
        self.writer.append(encoded);
        self.new_locations.push((
            id,
            Location {
                segment_seq: self.new_seq,
                offset,
                stored_len: bytes.len() as u64,
                materialized_len: None,
                tag: crate::format::version::RecordTag::BtreeNode,
            },
        ));
        self.pending.insert(id, bytes);
    }
}

/// The rebuilt chunk index (Phase-8B): its root plus the old-node
/// bookkeeping the compaction loop needs. Produced by
/// `rebuild_chunk_index`, consumed by `collect_impl`: `old_nodes` for the
/// copy-skip, `old_only` for the post-commit prune, `root` for the
/// published root.
struct RebuiltIndex {
    /// New chunk-index tree root (ZERO for an empty index).
    root: ChunkId,
    /// Old index nodes the rebuilt tree does not reuse: dropped from the
    /// object index (their records die with the victim segments).
    old_only: HashSet<ChunkId>,
    /// Every old index node. The copy loop skips all of them: the rebuild
    /// already staged a fresh record in the new segment for every node the
    /// new tree contains, so copying an old index node would duplicate it.
    old_nodes: HashSet<ChunkId>,
}

/// Phase-8B: rebuild the derived chunk index from reachability.
///
/// The chunk index (`content id → descriptor`) is a derived structure
/// (§34): overwritten, unsnapshotted content ids must not accumulate
/// descriptor entries inside root-reachable index nodes forever. The
/// rebuilt tree contains exactly the necessary reachable set:
///
/// - live extents' descriptors (dedup hit-ability for still-live content);
/// - the transitive reference closure (EXACT_REF targets, BASE_RESIDUAL
///   bases — decodability);
///
/// Old index nodes that the rebuilt tree does not reuse become ordinary
/// GC garbage. Returns the new index root and the old-node bookkeeping.
fn rebuild_chunk_index(
    store: &Store,
    writer: &mut SegmentWriter,
    new_seq: u64,
    mark: &LiveMark,
    new_locations: &mut Vec<(ChunkId, Location)>,
) -> Result<RebuiltIndex, StoreError> {
    // ---------------------------------------------------------------------
    // Stage 1: Enumerate the old index's nodes and its surviving entries.
    //
    // `old_nodes` is the FULL old tree (needed later so the copy loop can
    // skip every old index node). `kept` is the old tree's entries in key
    // order, filtered to the reachable set (live descriptors + reference
    // closure); `scan_all` is in-order, so `kept` is sorted.
    // ---------------------------------------------------------------------
    let limits = store.config().limits;
    let old_root = store.current_root().chunk_index_root;
    let mut old_nodes: HashSet<ChunkId> = HashSet::new();
    if !old_root.is_zero() {
        collect_tree_node_ids(store, &old_root, &mut old_nodes)?;
    }
    // Surviving entries in key order (scan_all is in-order), so the new
    // tree is built deterministically from the same content.
    let mut kept: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    if !old_root.is_zero() {
        let entries = crate::store::index::scan_all(
            old_root,
            crate::store::BTREE_ORDER,
            limits.max_fanout,
            store,
        )?;
        for (key, value) in entries {
            let cid = ChunkId::new(
                key.as_slice()
                    .try_into()
                    .map_err(|_| StoreError::Invariant("chunk index key not 32 bytes".into()))?,
            );
            if mark.referenced.contains(&cid) || mark.live_descriptors.contains(&value) {
                kept.push((key, value));
            }
        }
    }
    let mut provider = RebuildProvider {
        writer,
        new_seq,
        pending: HashMap::new(),
        new_locations,
    };

    // ---------------------------------------------------------------------
    // Stage 2: Bulk-load the rebuilt tree — each FINAL node staged exactly
    // once.
    //
    // Phase-9H: bulk-load the rebuilt tree so each final node is staged
    // EXACTLY once. The previous repeated-`insert` build staged every COW
    // intermediate path version (2.66 MB of dead BtreeNode records on the
    // real-tree court — the compaction was physically writing the tree
    // several times over). `bulk_load` requires sorted input; `scan_all`
    // is in key order, so `kept` is already sorted.
    // ---------------------------------------------------------------------
    let new_root = crate::store::index::bulk_load(
        &kept,
        crate::store::BTREE_ORDER,
        limits.max_fanout,
        &mut provider,
    )?;

    // ---------------------------------------------------------------------
    // Stage 3: Compute the old-node bookkeeping for the compaction loop.
    //
    // Every staged node is part of the FINAL new tree (`bulk_load` stages
    // each final node exactly once, bottom-up — no COW intermediates, the
    // pre-9H behavior). `old_only` = old nodes the new tree does not
    // contain: their records die with the victims and their index entries
    // are pruned after commit.
    // ---------------------------------------------------------------------
    let new_nodes: HashSet<ChunkId> = provider.pending.keys().copied().collect();
    let old_only: HashSet<ChunkId> = old_nodes.difference(&new_nodes).copied().collect();
    Ok(RebuiltIndex {
        root: new_root,
        old_only,
        old_nodes,
    })
}

/// Run GC: mark, compact victims, commit, delete old segments.
///
/// Returns the number of PHYSICAL bytes reclaimed. Victim selection uses
/// the scanned PHYSICAL occupancy (`physical_ratios`), not the derived
/// index (Phase-9H).
pub fn collect(
    store: &Store,
    hooks: &crate::store::transaction::CrashHooks,
) -> Result<u64, StoreError> {
    crate::perf::trace::span!("gc.collect", op = "gc_collect");
    // ---------------------------------------------------------------------
    // Stage 1: Flush the active epoch.
    //
    // Phase-10D: GC's reachability walk only sees committed roots; the
    // active epoch's staged objects are referenced only by the log, so a
    // GC during an epoch would treat them as garbage. Flush the epoch
    // (one checkpoint) first.
    // ---------------------------------------------------------------------
    store.ensure_epoch_flushed(hooks)?;

    // ---------------------------------------------------------------------
    // Stage 2: Mark the live object set from all roots.
    // ---------------------------------------------------------------------
    let mark = mark_live_full(store)?;

    // ---------------------------------------------------------------------
    // Stage 3: Select victims by PHYSICAL occupancy.
    //
    // Phase-9H: victim selection uses the PHYSICAL per-segment occupancy
    // (scanned from the segment files), so segments full of index-hidden
    // or unindexed garbage are compacted even when the derived index's
    // one-location view calls them live.
    //
    // WHY PHYSICAL, NOT THE DERIVED INDEX (the evidence-sensitive story):
    // the index maps each content id to ONE location, so it cannot see a
    // re-appended payload's older physical copy (index-hidden) or records
    // with no entry (unindexed). Phase-9H proved the divergence is real:
    // on the real-tree court the post-GC dead bytes (2.66 MB) were
    // `BtreeNode` records staged by the chunk-index REBUILD — the old
    // repeated-COW-insert rebuild physically wrote every intermediate path
    // version — so index-derived occupancy understated the dead bytes.
    // `physical::scan_physical` reconciles every segment byte (live /
    // dead-indexed / index-hidden / unindexed / torn / padding / format);
    // evidence campaign `1787688017-0a03ece`.
    // ---------------------------------------------------------------------
    let ratios = physical_ratios(store, &mark.live)?;
    let target = store.config().gc_target_ratio;
    let victims: Vec<u64> = ratios
        .iter()
        .filter(|(_, (live_b, total))| total > &0 && (*live_b as f64 / *total as f64) < target)
        .map(|(seq, _)| *seq)
        .collect();
    if victims.is_empty() {
        return Ok(0);
    }
    collect_impl(store, hooks, &mark, &victims)
}

/// Phase-9H: FULL compaction — every segment is a victim. Walks the
/// reachable object graph, writes every live record once into fresh
/// compact segments (with the chunk index rebuilt from reachability),
/// publishes the new root, and deletes every old segment. The physical
/// backing converges to the reachable persistent state plus bounded
/// format overhead. Idempotent: a second full compaction reclaims only
/// the new root/format tail.
///
/// Evidence (campaign `1787688017-0a03ece`): tree-court full compact = 4 B
/// format overhead over reachable (0.00% of logical); a second compaction
/// reclaims 0 B. This is the `entropyfs gc --compact` path.
pub fn compact_full(
    store: &Store,
    hooks: &crate::store::transaction::CrashHooks,
) -> Result<u64, StoreError> {
    crate::perf::trace::span!("gc.compact_full", op = "gc_compact_full");
    let mark = mark_live_full(store)?;
    let victims: Vec<u64> = segment::list_segments(store.dir())?;
    if victims.is_empty() {
        return Ok(0);
    }
    collect_impl(store, hooks, &mark, &victims)
}

/// The shared compaction core: rebuild the derived chunk index from
/// reachability, copy the live records of the victim segments into a
/// fresh segment, publish the new root, delete the victims.
///
/// Returns the number of PHYSICAL bytes estimated reclaimable (unreachable
/// record bytes in the victims, from the index view; the fresh root/format
/// tail is not subtracted).
fn collect_impl(
    store: &Store,
    hooks: &crate::store::transaction::CrashHooks,
    mark: &LiveMark,
    victims: &[u64],
) -> Result<u64, StoreError> {
    let live = &mark.live;
    // Phase-9H: the CURRENT root record is superseded by the fresh root
    // this pass appends; copying it would leave a permanent 238 B dead
    // root per compaction. Snapshot roots are Root-tagged records too and
    // MUST be copied — only the current root id is skipped. (The 9H
    // campaign's post-GC unreachable-by-tag table shows exactly this
    // class: `{"BtreeNode": ..., "Root": 238}`.)
    let current_root_id = store.current_root().id();

    // ---------------------------------------------------------------------
    // Stage 1: Rebuild the derived chunk index from reachability, staged
    // into the fresh segment.
    //
    // Phase-8B: rebuild the derived chunk index from reachability BEFORE
    // compacting, so overwritten unsnapshotted content ids stop
    // accumulating descriptor entries inside root-reachable index nodes.
    // The rebuilt tree is staged in the same segment as the copied live
    // records and the new root, so it commits atomically with them.
    // ---------------------------------------------------------------------
    let new_seq = store.current_segment_seq() + 1;
    let mut writer = SegmentWriter::open(store.io(), new_seq)?;
    let mut new_locations: Vec<(ChunkId, Location)> = Vec::new();
    let rebuilt = rebuild_chunk_index(store, &mut writer, new_seq, &mark, &mut new_locations)?;

    // ---------------------------------------------------------------------
    // Stage 2: Estimate the reclaimable bytes (index view).
    //
    // Reclaimable estimate: unreachable bytes inside victim segments,
    // including the index nodes the rebuild replaced. Units: PHYSICAL
    // record bytes (`Location::total_size`). This is an estimate for the
    // return value only — the authoritative census is the physical scan.
    // ---------------------------------------------------------------------
    let mut reclaimable = 0u64;
    for (id, loc) in store.object_index().iter() {
        if victims.contains(&loc.segment_seq)
            && (!live.contains(&id) || rebuilt.old_only.contains(&id))
        {
            reclaimable += loc.total_size();
        }
    }

    // ---------------------------------------------------------------------
    // Stage 3: Copy the victims' live records into the fresh segment.
    //
    // Copy live records from victim segments into a fresh segment. The
    // chunk-index nodes are skipped entirely: the rebuild already staged a
    // fresh record for every node the new tree contains, and the replaced
    // nodes die with the victims. The copy order is deterministic
    // (segment, offset) so the physical layout is reproducible.
    // ---------------------------------------------------------------------
    let mut copy_candidates: Vec<(ChunkId, Location)> = store
        .object_index()
        .iter()
        .into_iter()
        .filter(|(id, loc)| {
            victims.contains(&loc.segment_seq)
                && live.contains(id)
                && !rebuilt.old_nodes.contains(id)
                && *id != current_root_id
        })
        .collect();
    copy_candidates.sort_by_key(|(_, loc)| (loc.segment_seq, loc.offset));
    for (id, loc) in copy_candidates {
        let payload = store.read_payload_at(&loc)?;
        // Preserve the envelope flags/materialized length exactly.
        let flags = if loc.materialized_len.is_some() {
            crate::format::record::FLAG_HAS_MATERIALIZED_LEN
        } else {
            0
        };
        let encoded = crate::format::record::encode(loc.tag, flags, loc.materialized_len, &payload);
        let offset = writer.durable_end() + writer.buffered_len();
        writer.append(encoded);
        new_locations.push((
            id,
            Location {
                segment_seq: new_seq,
                offset,
                stored_len: payload.len() as u64,
                materialized_len: loc.materialized_len,
                tag: loc.tag,
            },
        ));
    }
    writer.flush()?;
    writer.fdatasync()?;
    store.io().sync_segments_dir()?;

    // ---------------------------------------------------------------------
    // Stage 4: Commit the new root (durability barrier).
    //
    // The segment was made durable above (fdatasync + directory fsync for
    // the freshly created segment file) BEFORE the root references it —
    // transaction-model.md §2 ordering. Build the new root and commit it
    // (durability barrier). The rebuilt chunk index becomes part of the
    // published root. The superblock flip + fsync is the persistence
    // linearization point; the crash hooks bracket every boundary.
    // ---------------------------------------------------------------------
    let mut root = store.current_root();
    root.chunk_index_root = rebuilt.root;
    root.segment_seq = new_seq;
    root.index_epoch = root.index_epoch.saturating_add(1);
    root.generation = store.generation() + 1;
    let root_bytes = root.encode();
    let root_id = ChunkId::of(&root_bytes);
    let encoded = crate::format::record::encode(
        crate::format::version::RecordTag::Root,
        0,
        None,
        &root_bytes,
    );
    let offset = writer.durable_end();
    writer.append(encoded);
    writer.flush()?;
    writer.fdatasync()?;
    hooks.hit(crate::store::transaction::CrashPoint::AfterRootWrite)?;
    store.write_superblock(root_id, &root)?;
    hooks.hit(crate::store::transaction::CrashPoint::AfterSuperblockWrite)?;
    store.fsync_superblock()?;
    hooks.hit(crate::store::transaction::CrashPoint::AfterSuperblockFsync)?;

    // ---------------------------------------------------------------------
    // Stage 5: Publish — object index, committed root, current segment.
    //
    // Publish: update the object index, root, current segment. The new
    // locations (copied records + rebuilt chunk-index nodes) become the
    // derived index's view; the root object itself is indexed too.
    // ---------------------------------------------------------------------
    for (id, loc) in new_locations {
        store.object_index().insert(id, loc);
    }
    store.publish_commit(&root, root_id)?;
    let root_loc = Location {
        segment_seq: new_seq,
        offset,
        stored_len: root_bytes.len() as u64,
        materialized_len: None,
        tag: crate::format::version::RecordTag::Root,
    };
    store.object_index().insert(root_id, root_loc);
    store.install_segment(writer);

    // ---------------------------------------------------------------------
    // Stage 6: Delete the victims and prune the derived index — only now
    // that the new root is durable.
    //
    // Delete victims only after the new root is durable. A crash before
    // this point leaves the old segments intact (garbage under the new
    // root, still valid under the old); a crash here or later leaves them
    // partially deleted, which is fine because the new root no longer
    // references them (ADR-0008).
    // ---------------------------------------------------------------------
    hooks.hit(crate::store::transaction::CrashPoint::BeforeOldSegmentDelete)?;
    for seq in victims {
        store.io().delete_segment(*seq)?;
    }
    // Drop derived index entries for dead records in deleted segments
    // (unreachable objects and the replaced chunk-index nodes) so
    // reachability accounting reflects the new physical state.
    let dead: Vec<ChunkId> = store
        .object_index()
        .iter()
        .into_iter()
        .filter(|(id, loc)| {
            victims.contains(&loc.segment_seq)
                && (!live.contains(id) || rebuilt.old_only.contains(id))
        })
        .map(|(id, _)| id)
        .collect();
    for id in dead {
        store.object_index().remove(&id);
    }
    Ok(reclaimable)
}

/// Reclaimable bytes (unreachable record bytes).
///
/// Units: PHYSICAL record bytes (`Location::total_size` = header + stored
/// payload), summed over the derived index's one location per content id.
/// This is the index view; the authoritative census is
/// `physical::scan_physical` (Phase-9H).
pub fn unreachable_bytes(store: &Store) -> Result<u64, StoreError> {
    let live = mark_live(store)?;
    let mut unreachable = 0u64;
    for (id, loc) in store.object_index().iter() {
        if !live.contains(&id) {
            unreachable += loc.total_size();
        }
    }
    Ok(unreachable)
}

/// Unreachable record bytes by record tag (Phase-9A floor diagnosis):
/// which physical record class makes up the reachable → total-backing gap
/// after GC. B-tree intermediates created inside a transaction (superseded
/// COW nodes that were never reachable from the final root) show up here
/// as `BtreeNode`; duplicate payload records from before transaction-local
/// CAS canonicalization would show up as `Data`/`Model`.
///
/// Units: PHYSICAL record bytes (`Location::total_size`), grouped by
/// record tag. The 9H campaign used this to name the floor: post-GC
/// unreachable was `{"BtreeNode": 200795, "Root": 238}` on the GC-traffic
/// H2 store — the rebuild's COW intermediates plus one superseded current
/// root (the class the copy loop's current-root skip removes).
pub fn unreachable_bytes_by_record_tag(
    store: &Store,
) -> Result<std::collections::BTreeMap<String, u64>, StoreError> {
    let live = mark_live(store)?;
    let mut by_tag: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for (id, loc) in store.object_index().iter() {
        if !live.contains(&id) {
            *by_tag.entry(format!("{:?}", loc.tag)).or_insert(0) += loc.total_size();
        }
    }
    Ok(by_tag)
}

/// Workaround for unused CodecError import in some configurations.
#[allow(unused_imports)]
use CodecError as _CodecError;
