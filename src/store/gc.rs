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

/// The result of the mark walk: the live object set plus the two derived
/// sets needed to rebuild the chunk index from reachability (Phase-8B,
/// §34):
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

/// Compute per-segment live ratios.
pub fn live_ratios(
    store: &Store,
    live: &HashSet<ChunkId>,
) -> Result<HashMap<u64, (u64, u64)>, StoreError> {
    let mut map: HashMap<u64, (u64, u64)> = HashMap::new(); // seq -> (live, total)
    for (id, loc) in store.object_index().iter() {
        let entry = map.entry(loc.segment_seq).or_insert((0, 0));
        entry.1 += loc.total_size();
        if live.contains(&id) {
            entry.0 += loc.total_size();
        }
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

/// The rebuilt chunk index (Phase-8B): its root plus the old-node bookkeeping
/// the compaction loop needs.
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
    let mut new_root = ChunkId::ZERO;
    for (key, value) in &kept {
        new_root = crate::store::index::insert(
            new_root,
            key,
            value,
            crate::store::BTREE_ORDER,
            limits.max_fanout,
            &mut provider,
        )?;
    }
    // Every staged node is part of the new tree (insert only stages nodes
    // on the new root's path).
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
/// Returns the number of bytes reclaimed.
pub fn collect(
    store: &Store,
    hooks: &crate::store::transaction::CrashHooks,
) -> Result<u64, StoreError> {
    let mark = mark_live_full(store)?;
    let live = &mark.live;
    let ratios = live_ratios(store, live)?;
    let target = store.config().gc_target_ratio;
    let victims: Vec<u64> = ratios
        .iter()
        .filter(|(_, (live_b, total))| total > &0 && (*live_b as f64 / *total as f64) < target)
        .map(|(seq, _)| *seq)
        .collect();
    if victims.is_empty() {
        return Ok(0);
    }

    // Phase-8B: rebuild the derived chunk index from reachability BEFORE
    // compacting, so overwritten unsnapshotted content ids stop
    // accumulating descriptor entries inside root-reachable index nodes.
    // The rebuilt tree is staged in the same segment as the copied live
    // records and the new root, so it commits atomically with them.
    let new_seq = store.current_segment_seq() + 1;
    let mut writer = SegmentWriter::open(store.dir(), new_seq)?;
    let mut new_locations: Vec<(ChunkId, Location)> = Vec::new();
    let rebuilt = rebuild_chunk_index(store, &mut writer, new_seq, &mark, &mut new_locations)?;

    // Reclaimable estimate: unreachable bytes inside victim segments,
    // including the index nodes the rebuild replaced.
    let mut reclaimable = 0u64;
    for (id, loc) in store.object_index().iter() {
        if victims.contains(&loc.segment_seq)
            && (!live.contains(&id) || rebuilt.old_only.contains(&id))
        {
            reclaimable += loc.total_size();
        }
    }

    // Copy live records from victim segments into a fresh segment. The
    // chunk-index nodes are skipped entirely: the rebuild already staged a
    // fresh record for every node the new tree contains, and the replaced
    // nodes die with the victims. The copy order is deterministic
    // (segment, offset) so the physical layout is reproducible.
    let mut copy_candidates: Vec<(ChunkId, Location)> = store
        .object_index()
        .iter()
        .into_iter()
        .filter(|(id, loc)| {
            victims.contains(&loc.segment_seq)
                && live.contains(id)
                && !rebuilt.old_nodes.contains(id)
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
    SegmentWriter::sync_dir(store.dir())?;

    // Build the new root and commit it (durability barrier). The rebuilt
    // chunk index becomes part of the published root.
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

    // Publish: update the object index, root, current segment.
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

    // Delete victims only after the new root is durable.
    hooks.hit(crate::store::transaction::CrashPoint::BeforeOldSegmentDelete)?;
    for seq in &victims {
        segment::delete_segment(store.dir(), *seq)?;
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
