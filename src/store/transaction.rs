//! Transactions: the commit protocol with crash-court injection points
//! (ADR-0008, `docs/architecture/transaction-model.md`).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::core::extent::ChunkId;
use crate::core::representation::Representation;
use crate::format::version::RecordTag;
use crate::store::StoreError;
use crate::store::index::{BTreeError, ObjectProvider};
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
        // 0. ENOSPC guard: refuse before staging anything (the watermark
        //    keeps the GC emergency reserve untouched).
        self.store.ensure_commit_space(&self.records)?;
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
        hooks.hit(CrashPoint::AfterRootWrite)?;
        // 2. append all new immutable records (including the root).
        self.store.append_records(&mut self.records)?;
        // 3. flush the segment to the file's page cache (process-crash
        //    durable; the power barrier is the durability barrier).
        self.store.flush_segment()?;
        // 4. write the inactive superblock slot (page cache; fsync at the
        //    barrier). Torn slot writes are detected at recovery.
        self.store.write_superblock(root_id, &self.root)?;
        // 5. publish in-memory state.
        self.store.publish_commit(&self.root, root_id)?;
        Ok(self.store)
    }
}

impl<'a> ObjectProvider for Tx<'a> {
    fn get(&self, id: &ChunkId) -> Result<Option<Vec<u8>>, BTreeError> {
        self.fetch_pending_or_store(id)
            .map_err(|e| BTreeError::Provider(e.to_string()))
    }

    fn put(&mut self, id: ChunkId, bytes: Vec<u8>) {
        self.pending.insert(id, bytes.clone());
        self.records.push(PendingRecord {
            tag: RecordTag::BtreeNode,
            payload: bytes,
            materialized_len: None,
        });
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
    tx.pending.insert(id, payload.clone());
    tx.records.push(PendingRecord {
        tag,
        payload,
        materialized_len,
    });
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
