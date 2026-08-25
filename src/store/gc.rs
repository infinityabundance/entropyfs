//! Reachability GC (ADR-0009, `docs/architecture/gc.md`).
//!
//! Mark from all roots (current + snapshots) through the object graph;
//! compute per-segment live ratios; copy live records from low-utilization
//! segments; commit the new root; delete obsolete segments only after the
//! new root is durable (`BEFORE_OLD_SEGMENT_DELETE` is a crash-court
//! boundary).

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

/// Mark the live object set from all roots.
///
/// The chunk index is a *derived* structure (§34): its tree nodes are
/// root-reachable and stay live, but the objects its entries reference are
/// pinned only when the content id is actually referenced by a live extent
/// (an EXACT_REF target or a BASE_RESIDUAL base). Without this, deleted
/// data stays pinned by the ever-growing index and GC could never reclaim
/// it.
pub fn mark_live(store: &Store) -> Result<HashSet<ChunkId>, StoreError> {
    let mut live: HashSet<ChunkId> = HashSet::new();
    let mut worklist: Vec<(ChunkId, MarkKind)> = Vec::new();
    // Content ids referenced by live extents (through EXACT_REF targets
    // and BASE_RESIDUAL bases). Resolved through the chunk index after the
    // main walk.
    let mut referenced: HashSet<ChunkId> = HashSet::new();

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
            )?,
            MarkKind::TreeDirectory => walk_tree(
                store,
                &id,
                TreeValue::Directory,
                &mut live,
                &mut worklist,
                &mut referenced,
            )?,
            MarkKind::TreeExtent => walk_tree(
                store,
                &id,
                TreeValue::ExtentDescriptor,
                &mut live,
                &mut worklist,
                &mut referenced,
            )?,
            MarkKind::TreeChunkIndex => walk_tree(
                store,
                &id,
                TreeValue::ChunkIndexEntry,
                &mut live,
                &mut worklist,
                &mut referenced,
            )?,
            MarkKind::TreeSnapshot => walk_tree(
                store,
                &id,
                TreeValue::Snapshot,
                &mut live,
                &mut worklist,
                &mut referenced,
            )?,
            MarkKind::TreeXattr => walk_tree(
                store,
                &id,
                TreeValue::Xattr,
                &mut live,
                &mut worklist,
                &mut referenced,
            )?,
            MarkKind::Object => {}
        }
    }

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
        let desc = match crate::format::descriptor::decode(
            &bytes,
            limits.max_descriptor_bytes,
            limits.max_inline_bytes,
            limits.max_palette,
            limits.max_period,
            limits.max_chunk_size,
        ) {
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
    Ok(live)
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
    let desc = match crate::format::descriptor::decode(
        bytes,
        l.max_descriptor_bytes,
        l.max_inline_bytes,
        l.max_palette,
        l.max_period,
        l.max_chunk_size,
    ) {
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
    let desc = match crate::format::descriptor::decode(
        bytes,
        l.max_descriptor_bytes,
        l.max_inline_bytes,
        l.max_palette,
        l.max_period,
        l.max_chunk_size,
    ) {
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

/// Compute per-segment live ratios.
pub fn live_ratios(
    store: &Store,
    live: &HashSet<ChunkId>,
) -> Result<HashMap<u64, (u64, u64)>, StoreError> {
    let mut map: HashMap<u64, (u64, u64)> = HashMap::new(); // seq -> (live, total)
    for (id, loc) in store.object_index().iter() {
        let entry = map.entry(loc.segment_seq).or_insert((0, 0));
        entry.1 += loc.total_size();
        if live.contains(id) {
            entry.0 += loc.total_size();
        }
    }
    Ok(map)
}

/// Run GC: mark, compact victims, commit, delete old segments.
///
/// Returns the number of bytes reclaimed.
pub fn collect(
    store: &mut Store,
    hooks: &crate::store::transaction::CrashHooks,
) -> Result<u64, StoreError> {
    let live = mark_live(store)?;
    let ratios = live_ratios(store, &live)?;
    let target = store.config().gc_target_ratio;
    let victims: Vec<u64> = ratios
        .iter()
        .filter(|(_, (live_b, total))| total > &0 && (*live_b as f64 / *total as f64) < target)
        .map(|(seq, _)| *seq)
        .collect();
    if victims.is_empty() {
        return Ok(0);
    }
    // Reclaimable estimate: unreachable bytes inside victim segments.
    let mut reclaimable = 0u64;
    for (id, loc) in store.object_index().iter() {
        if victims.contains(&loc.segment_seq) && !live.contains(id) {
            reclaimable += loc.total_size();
        }
    }

    // Copy live records from victim segments into a fresh segment.
    let new_seq = store.current_segment_seq() + 1;
    let mut writer = SegmentWriter::open(store.dir(), new_seq)?;
    let mut new_locations: Vec<(ChunkId, Location)> = Vec::new();
    for (id, loc) in store.object_index().iter() {
        if !victims.contains(&loc.segment_seq) || !live.contains(id) {
            continue;
        }
        let payload = store.read_payload_at(loc)?;
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
            *id,
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
    SegmentWriter::sync_dir(store.dir())?;

    // Build the new root and commit it (durability barrier).
    let mut root = store.current_root().clone();
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

    // Publish: update the object index, root, current segment.
    for (id, loc) in new_locations {
        store.object_index_mut().insert(id, loc);
    }
    store.publish_commit(&root, root_id)?;
    let root_loc = Location {
        segment_seq: new_seq,
        offset,
        stored_len: root_bytes.len() as u64,
        materialized_len: None,
        tag: crate::format::version::RecordTag::Root,
    };
    store.object_index_mut().insert(root_id, root_loc);
    store.current_segment = Some(writer);

    // Delete victims only after the new root is durable.
    hooks.hit(crate::store::transaction::CrashPoint::BeforeOldSegmentDelete)?;
    for seq in &victims {
        segment::delete_segment(store.dir(), *seq)?;
    }
    // Drop derived index entries for dead records in deleted segments so
    // reachability accounting reflects the new physical state.
    let dead: Vec<ChunkId> = store
        .object_index()
        .iter()
        .filter(|(id, loc)| victims.contains(&loc.segment_seq) && !live.contains(*id))
        .map(|(id, _)| *id)
        .collect();
    for id in dead {
        store.object_index_mut().remove(&id);
    }
    Ok(reclaimable)
}

/// Reclaimable bytes (unreachable record bytes).
pub fn unreachable_bytes(store: &Store) -> Result<u64, StoreError> {
    let live = mark_live(store)?;
    let mut unreachable = 0u64;
    for (id, loc) in store.object_index().iter() {
        if !live.contains(id) {
            unreachable += loc.total_size();
        }
    }
    Ok(unreachable)
}

/// Workaround for unused CodecError import in some configurations.
#[allow(unused_imports)]
use CodecError as _CodecError;
