//! Transactions: the commit protocol with crash-court injection points
//! (ADR-0008, `docs/architecture/transaction-model.md`).

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use crate::core::extent::ChunkId;
use crate::core::representation::Representation;
use crate::format::version::RecordTag;
use crate::store::StoreError;
use crate::store::index::{BTreeError, ObjectProvider};
use crate::store::inode::{Inode, InodeData};
use crate::store::root::Root;

/// Crash-court injection points (`docs/recovery/crash-consistency.md` §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrashPoint {
    /// Records appended to the segment buffer, not flushed.
    AfterRecordAppend,
    /// Flushed and fdatasync'd.
    AfterSegmentFdatasync,
    /// New segment directory entry fsynced.
    AfterSegmentDirFsync,
    /// New root constructed (it is appended with the other records).
    AfterRootWrite,
    /// Inactive superblock slot written, not fsynced.
    AfterSuperblockWrite,
    /// Superblock fsynced (commit durable, before ack).
    AfterSuperblockFsync,
    /// GC: before old segment deletion.
    BeforeOldSegmentDelete,
}

/// Commit hooks for crash courts. When a hook is armed, commit() simulates
/// a crash at that point (returns `CrashSimulated` after performing the
/// intervening durability work, without completing the commit).
#[derive(Debug, Clone, Copy, Default)]
pub struct CrashHooks {
    /// Arm one injection point.
    pub armed: Option<CrashPoint>,
}

impl CrashHooks {
    /// No hooks (normal operation).
    pub fn none() -> Self {
        Self { armed: None }
    }

    /// Arm a crash point.
    pub fn crash_at(point: CrashPoint) -> Self {
        Self { armed: Some(point) }
    }

    /// Test an armed crash point: returns `CrashSimulated` at the armed
    /// boundary (crash-court injection).
    pub fn hit(&self, point: CrashPoint) -> Result<(), StoreError> {
        if self.armed == Some(point) {
            Err(StoreError::CrashSimulated(format!("{point:?}")))
        } else {
            Ok(())
        }
    }
}

/// A pending immutable record: raw payload staged for append.
///
/// The store encodes the envelope (header + CRC + content id) at append
/// time, so the payload here is the raw bytes (ADR-0008).
#[derive(Debug, Clone)]
pub(crate) struct PendingRecord {
    /// Record tag.
    pub(crate) tag: RecordTag,
    /// Raw payload bytes (the record is encoded at append time).
    pub(crate) payload: Vec<u8>,
    /// Materialized length.
    pub(crate) materialized_len: Option<u64>,
}

/// A pending transaction: new immutable records + root construction.
///
/// The `ObjectProvider` impl resolves node fetches from pending records
/// first, then the committed store. The transaction holds the store's
/// commit-coordinator lock from `begin` to commit, so transaction
/// application and root publication are serialized while candidate
/// encoding (the expensive part of a write) runs before `begin_tx`.
pub struct Tx<'a> {
    /// Pending object payloads keyed by content id (for in-tx resolution).
    pending: HashMap<ChunkId, Vec<u8>>,
    /// Pending records to append, in append order.
    records: Vec<PendingRecord>,
    /// Root being built.
    root: Root,
    /// The store this transaction commits to (interior mutability: the
    /// commit protocol appends records and flips the superblock).
    pub(crate) store: &'a crate::store::Store,
    /// The commit-coordinator guard (held until this transaction ends).
    _commit_guard: std::sync::MutexGuard<'a, ()>,
}

impl<'a> Tx<'a> {
    /// Stage an immutable object (Phase-8C transaction-local CAS
    /// canonicalization): append a physical record ONLY when the content
    /// id is not already pending in this transaction and not already
    /// committed. An object that exists costs zero new records — that is
    /// the content-addressed store's whole point.
    ///
    /// Object identity is the PAYLOAD HASH alone: `RecordTag` (Data vs
    /// Model vs BtreeNode) is envelope metadata, not part of identity, so
    /// two equal payloads share one identity regardless of tag. The
    /// materialized length is likewise envelope metadata; identical
    /// payloads always have identical materialized content, so a skipped
    /// re-stage never loses information. This is safe under the
    /// durability ordering: a committed object's record was appended (and
    /// segment-rolled segments were fsync'd) before the superblock slot
    /// could reference this transaction's root, so the object survives
    /// whenever this root does (append-ordered segments can only lose a
    /// tail, never the middle). The commit-coordinator lock (held from
    /// `begin`) serializes against GC's root publication, so the object
    /// index cannot lose the object mid-transaction.
    fn stage(
        &mut self,
        id: ChunkId,
        bytes: Vec<u8>,
        tag: RecordTag,
        materialized_len: Option<u64>,
    ) {
        if self.pending.contains_key(&id) || self.store.object_index().contains(&id) {
            // Already staged or committed: no new physical record.
            return;
        }
        self.pending.insert(id, bytes.clone());
        self.records.push(PendingRecord {
            tag,
            payload: bytes,
            materialized_len,
        });
    }
    /// Begin a transaction from the current root (the caller has taken
    /// the store's commit lock).
    pub(crate) fn begin(
        store: &'a crate::store::Store,
        guard: std::sync::MutexGuard<'a, ()>,
    ) -> Self {
        Self {
            pending: HashMap::new(),
            records: Vec::new(),
            root: store.current_root(),
            store,
            _commit_guard: guard,
        }
    }

    /// The working root (mutated as nodes are added).
    pub fn root(&self) -> &Root {
        &self.root
    }

    /// Mutable root access (for inode/dir/extent tree root updates).
    pub fn root_mut(&mut self) -> &mut Root {
        &mut self.root
    }

    /// Resolve a chunk id to its descriptor through this transaction's
    /// pending records or the committed store.
    pub fn resolve_descriptor(&self, cid: &ChunkId) -> Result<Option<Representation>, StoreError> {
        // Chunk descriptors live in the chunk index B-tree; resolution
        // goes through the provider, so pending descriptors are visible.
        let bytes = self.fetch_pending_or_store(cid)?;
        match bytes {
            Some(b) => {
                let rep = crate::format::descriptor::decode(
                    &b,
                    self.store.limits().max_descriptor_bytes,
                    self.store.limits().max_inline_bytes,
                    self.store.limits().max_palette,
                    self.store.limits().max_period,
                    self.store.limits().max_chunk_size,
                )?;
                Ok(Some(rep))
            }
            None => Ok(None),
        }
    }

    /// Fetch an object id from pending records or the committed store.
    pub(crate) fn fetch_pending_or_store(
        &self,
        id: &ChunkId,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        if let Some(b) = self.pending.get(id) {
            return Ok(Some(b.clone()));
        }
        self.store.fetch_object(id)
    }

    /// Commit: full durability — append records, sync the segment, flip
    /// the superblock, fsync it (ADR-0008). Used by CLI/batch paths and
    /// the crash-court tests.
    pub fn commit(self, hooks: &CrashHooks) -> Result<(), StoreError> {
        let store = self.commit_deferred(hooks)?;
        store.durability_barrier(hooks)?;
        Ok(())
    }

    /// Commit logically: the transaction becomes the in-memory current
    /// root and its records are flushed to the backing file's page cache
    /// and the inactive superblock slot is written (page cache only, no
    /// fsync). A process crash preserves the writes; a power loss may lose
    /// everything since the last [`Store::durability_barrier`] (POSIX:
    /// only fsync'd data is power-durable). Recovery validates the chosen
    /// slot's root and falls back to the newest valid root record in the
    /// segments, so a stale slot can never wedge the filesystem. Returns
    /// the store (for the caller to run the barrier when needed). The
    /// commit-coordinator lock is released when `self` is consumed.
    pub fn commit_deferred(
        mut self,
        hooks: &CrashHooks,
    ) -> Result<&'a crate::store::Store, StoreError> {
        // 1. finalize the root and stage its record WITH the other records
        //    so the whole commit is one flush (the superblock must never
        //    reference a root record that is not at least page-cache
        //    durable).
        self.root.generation = self.store.generation() + 1;
        self.root.segment_seq = self.store.current_segment_seq();
        let root_bytes = self.root.encode();
        let root_id = ChunkId::of(&root_bytes);
        self.records.push(PendingRecord {
            tag: RecordTag::Root,
            payload: root_bytes,
            materialized_len: None,
        });
        // 1b. Phase-9A: drop transaction-local COW intermediates. B-tree
        //     nodes and inode objects superseded while this transaction
        //     built its trees (each extent/chunk-index insert rewrites a
        //     path, and `put_extent_in_tx` rewrites the inode per insert)
        //     are unreachable from the final root: they are garbage-on-
        //     arrival and would occupy segments until a future GC
        //     compacted them (and if the segment stays live, forever —
        //     the measured ~10% incompressible backing floor). Records
        //     unreachable from the final root are pruned before append.
        self.prune_unreachable_records(&root_id)?;
        // 2. ENOSPC guard on the PRUNED footprint (refuse before staging
        //    anything; the watermark keeps the GC emergency reserve).
        self.store.ensure_commit_space(&self.records)?;
        hooks.hit(CrashPoint::AfterRootWrite)?;
        // 3. append all new immutable records (including the root).
        self.store.append_records(&mut self.records)?;
        // 4. flush the segment to the file's page cache (process-crash
        //    durable; the power barrier is the durability barrier).
        self.store.flush_segment()?;
        // 5. write the inactive superblock slot (page cache; fsync at the
        //    barrier). Torn slot writes are detected at recovery.
        self.store.write_superblock(root_id, &self.root)?;
        // 6. publish in-memory state.
        self.store.publish_commit(&self.root, root_id)?;
        Ok(self.store)
    }

    /// Phase-9A: prune transaction-local COW intermediates before append.
    ///
    /// While a transaction builds its trees, every insert rewrites a
    /// B-tree path and `put_extent_in_tx` rewrites the inode object; the
    /// superseded nodes and inodes are staged in `records` but are
    /// unreachable from the final root — garbage-on-arrival. They would
    /// occupy segments until a future GC compacted them (and if the
    /// containing segment stays live, forever: the measured ~10%
    /// incompressible backing floor). Walk the final root's graph over the
    /// pending+committed state and keep only reachable records.
    ///
    /// Correctness: records are immutable and content-addressed; a record
    /// dropped here was never referenced by the final root, and if a later
    /// transaction needs it, it stages it fresh (it is not in the object
    /// index). The root record itself is always kept.
    fn prune_unreachable_records(&mut self, root_id: &ChunkId) -> Result<(), StoreError> {
        let mut reachable: HashSet<ChunkId> = HashSet::new();
        reachable.insert(*root_id);
        let mut work: Vec<(ChunkId, TreeKind)> = Vec::new();
        work.push((self.root.inode_index_root, TreeKind::InodeIndex));
        work.push((self.root.chunk_index_root, TreeKind::ChunkIndex));
        if !self.root.snapshot_tree_root.is_zero() {
            work.push((self.root.snapshot_tree_root, TreeKind::Snapshot));
        }
        if !self.root.model_index_root.is_zero() {
            work.push((self.root.model_index_root, TreeKind::ChunkIndex));
        }
        let limits = *self.store.limits();
        while let Some((id, kind)) = work.pop() {
            if id.is_zero() || !reachable.insert(id) {
                continue;
            }
            if kind == TreeKind::Root {
                // A filesystem ROOT object, not a tree node.
                if let Some(bytes) = self.fetch_pending_or_store(&id)? {
                    if let Ok(root) = Root::decode(&bytes) {
                        work.push((root.inode_index_root, TreeKind::InodeIndex));
                        work.push((root.chunk_index_root, TreeKind::ChunkIndex));
                        if !root.snapshot_tree_root.is_zero() {
                            work.push((root.snapshot_tree_root, TreeKind::Snapshot));
                        }
                        if !root.model_index_root.is_zero() {
                            work.push((root.model_index_root, TreeKind::ChunkIndex));
                        }
                    }
                }
                continue;
            }
            let Some(payload) = self.fetch_pending_or_store(&id)? else {
                continue;
            };
            let node = crate::store::index::Node::decode(
                &payload,
                crate::store::BTREE_ORDER,
                limits.max_fanout,
            )
            .map_err(|e| StoreError::Index(e.to_string()))?;
            match node {
                crate::store::index::Node::Internal {
                    first_child,
                    entries,
                } => {
                    work.push((first_child, kind));
                    for e in entries {
                        let child =
                            ChunkId::new(e.value.as_slice().try_into().expect("32-byte child id"));
                        work.push((child, kind));
                    }
                }
                crate::store::index::Node::Leaf { entries } => {
                    for e in entries {
                        match kind {
                            TreeKind::InodeIndex => {
                                let inode_id =
                                    ChunkId::new(e.value.as_slice().try_into().map_err(|_| {
                                        StoreError::Invariant("inode value not 32 bytes".into())
                                    })?);
                                if reachable.insert(inode_id) {
                                    if let Some(bytes) = self.fetch_pending_or_store(&inode_id)? {
                                        if let Ok(inode) = Inode::decode(&bytes) {
                                            if !inode.xattr_root.is_zero() {
                                                work.push((inode.xattr_root, TreeKind::Xattr));
                                            }
                                            match inode.data {
                                                InodeData::Directory { dir_root }
                                                    if !dir_root.is_zero() =>
                                                {
                                                    work.push((dir_root, TreeKind::Directory));
                                                }
                                                InodeData::File { extent_root }
                                                    if !extent_root.is_zero() =>
                                                {
                                                    work.push((extent_root, TreeKind::Extent));
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                            }
                            TreeKind::Extent | TreeKind::ChunkIndex => {
                                // Descriptor values: the objects they
                                // reference must stay (their records may be
                                // staged in this transaction).
                                for oid in descriptor_object_ids(&e.value, &limits) {
                                    reachable.insert(oid);
                                }
                            }
                            TreeKind::Directory | TreeKind::Xattr => {}
                            TreeKind::Snapshot => {
                                if let Ok(entry) =
                                    crate::store::snapshot::SnapshotEntry::decode(&e.value)
                                {
                                    work.push((entry.root_id, TreeKind::Root));
                                }
                            }
                            TreeKind::Root => {}
                        }
                    }
                }
            }
        }
        let mut kept: Vec<PendingRecord> = Vec::with_capacity(self.records.len());
        for r in self.records.drain(..) {
            let id = ChunkId::of(&r.payload);
            if reachable.contains(&id) {
                kept.push(r);
            }
        }
        self.records = kept;
        Ok(())
    }
}

/// Tree kinds for the transaction-local reachability walk (Phase-9A).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeKind {
    /// Inode-index tree: leaf values are inode object ids.
    InodeIndex,
    /// Chunk-index / model-index tree: leaf values are descriptors.
    ChunkIndex,
    /// Extent tree: leaf values are descriptors.
    Extent,
    /// Directory tree: leaf values are directory entries (inline).
    Directory,
    /// Snapshot tree: leaf values are snapshot entries.
    Snapshot,
    /// Xattr tree: leaf values are inline.
    Xattr,
    /// A filesystem root object.
    Root,
}

/// The object ids a descriptor references (payloads/models/residual
/// streams/bases-as-objects). The chunk-index and extent-tree walks use
/// this to keep the records those objects are staged under.
fn descriptor_object_ids(bytes: &[u8], limits: &crate::core::limits::Limits) -> Vec<ChunkId> {
    let Ok(desc) = crate::format::descriptor::decode(
        bytes,
        limits.max_descriptor_bytes,
        limits.max_inline_bytes,
        limits.max_palette,
        limits.max_period,
        limits.max_chunk_size,
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let residual_objs = |r: &crate::core::representation::Residual, out: &mut Vec<ChunkId>| {
        use crate::core::representation::Residual;
        match r {
            Residual::RansCoded { enc_obj, model, .. }
            | Residual::BaseSequence { enc_obj, model, .. } => {
                out.push(*enc_obj);
                out.push(*model);
            }
            _ => {}
        }
    };
    match &desc {
        Representation::Raw { obj, .. } => out.push(*obj),
        Representation::Rans { model, enc_obj, .. }
        | Representation::SequenceRans { model, enc_obj, .. }
        | Representation::SparseBlock64 { model, enc_obj, .. } => {
            out.push(*model);
            out.push(*enc_obj);
        }
        Representation::BaseResidual { base, residual, .. } => {
            out.push(*base);
            residual_objs(residual, &mut out);
        }
        Representation::EntropyRef { residual, .. } => {
            residual_objs(residual, &mut out);
        }
        _ => {}
    }
    out
}

impl<'a> ObjectProvider for Tx<'a> {
    fn get(&self, id: &ChunkId) -> Result<Option<Vec<u8>>, BTreeError> {
        self.fetch_pending_or_store(id)
            .map_err(|e| BTreeError::Provider(e.to_string()))
    }

    fn put(&mut self, id: ChunkId, bytes: Vec<u8>) {
        self.stage(id, bytes, RecordTag::BtreeNode, None);
    }
}

/// Register a data object (payload) in the transaction; returns its id.
pub fn put_object(
    tx: &mut Tx<'_>,
    tag: RecordTag,
    payload: Vec<u8>,
    materialized_len: Option<u64>,
) -> ChunkId {
    let id = ChunkId::of(&payload);
    tx.stage(id, payload, tag, materialized_len);
    id
}

/// Register an inode object.
pub fn put_inode(tx: &mut Tx<'_>, inode: &crate::store::inode::Inode) -> ChunkId {
    put_object(tx, RecordTag::Inode, inode.encode(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_points_are_distinct() {
        let points = [
            CrashPoint::AfterRecordAppend,
            CrashPoint::AfterSegmentFdatasync,
            CrashPoint::AfterSegmentDirFsync,
            CrashPoint::AfterRootWrite,
            CrashPoint::AfterSuperblockWrite,
            CrashPoint::AfterSuperblockFsync,
            CrashPoint::BeforeOldSegmentDelete,
        ];
        let mut seen = std::collections::HashSet::new();
        for p in points {
            assert!(seen.insert(p));
        }
        assert_eq!(points.len(), 7);
    }
}
