//! Object identity and the derived object index (ADR-0007).
//!
//! The object index maps content id → segment location. It is a *derived,
//! disposable* index rebuilt from segment records at mount; authoritative
//! information is always reconstructable from segments + root.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::core::extent::ChunkId;
use crate::format::version::RecordTag;

/// Physical location of a record inside a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    /// Segment sequence number.
    pub segment_seq: u64,
    /// Record start offset within the segment file.
    pub offset: u64,
    /// Stored payload length.
    pub stored_len: u64,
    /// Materialized length when recorded.
    pub materialized_len: Option<u64>,
    /// Record tag.
    pub tag: RecordTag,
}

impl Location {
    /// Total on-disk size (header + payload).
    pub fn total_size(&self) -> u64 {
        crate::format::record::HEADER_SIZE + self.stored_len
    }
}

/// The derived object index (ADR-0007), sharded for concurrent access.
///
/// Sharded by the low bits of the content id behind independent `RwLock`
/// shards (ADR-0013/Phase-8 concurrency): reads take one shard's read
/// lock, inserts/removals take one shard's write lock. While mounted the
/// index is append-only (GC is offline), so readers of an older root
/// snapshot always find the objects they need.
#[derive(Debug)]
pub struct ObjectIndex {
    shards: Box<[std::sync::RwLock<HashMap<ChunkId, Location>>]>,
}

/// Number of shards (a power of two; more shards = more read parallelism).
const SHARDS: usize = 64;

impl Default for ObjectIndex {
    fn default() -> Self {
        let mut shards = Vec::with_capacity(SHARDS);
        for _ in 0..SHARDS {
            shards.push(std::sync::RwLock::new(HashMap::new()));
        }
        Self {
            shards: shards.into_boxed_slice(),
        }
    }
}

impl ObjectIndex {
    /// Empty index.
    pub fn new() -> Self {
        Self::default()
    }

    fn shard_of(id: &ChunkId) -> usize {
        // Low bits of the first content-id word: uniformly distributed for
        // BLAKE3 output.
        (u64::from_le_bytes(id.as_bytes()[..8].try_into().expect("8 bytes")) as usize)
            & (SHARDS - 1)
    }

    /// Insert a location.
    pub fn insert(&self, id: ChunkId, loc: Location) {
        self.shards[Self::shard_of(&id)]
            .write()
            .expect("object index shard poisoned")
            .insert(id, loc);
    }

    /// Look up a location (copied: `Location` is `Copy`, so callers never
    /// hold a shard lock across a segment read).
    pub fn get(&self, id: &ChunkId) -> Option<Location> {
        self.shards[Self::shard_of(id)]
            .read()
            .expect("object index shard poisoned")
            .get(id)
            .copied()
    }

    /// Whether an id is present.
    pub fn contains(&self, id: &ChunkId) -> bool {
        self.shards[Self::shard_of(id)]
            .read()
            .expect("object index shard poisoned")
            .contains_key(id)
    }

    /// Number of entries (sums shard lengths; O(shards)).
    pub fn len(&self) -> usize {
        self.shards
            .iter()
            .map(|s| s.read().expect("object index shard poisoned").len())
            .sum()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate all (id, location) pairs (for GC and fsck; snapshots each
    /// shard under a read lock).
    pub fn iter(&self) -> Vec<(ChunkId, Location)> {
        let mut out = Vec::new();
        for s in self.shards.iter() {
            let guard = s.read().expect("object index shard poisoned");
            out.extend(guard.iter().map(|(k, v)| (*k, *v)));
        }
        out
    }

    /// Remove an entry (offline GC compaction).
    pub fn remove(&self, id: &ChunkId) -> Option<Location> {
        self.shards[Self::shard_of(id)]
            .write()
            .expect("object index shard poisoned")
            .remove(id)
    }

    /// Clear all entries (index rebuild).
    pub fn clear(&self) {
        for s in self.shards.iter() {
            s.write().expect("object index shard poisoned").clear();
        }
    }
}

/// Aggregated store accounting (`docs/theory/information-accounting.md` §2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreStats {
    /// Physical capacity of the backing store.
    pub physical_capacity: u64,
    /// Total bytes in segment files.
    pub physical_used: u64,
    /// Logical bytes stored (Σ materialized lengths of reachable data).
    pub logical_bytes: u64,
    /// Reachable persisted bytes (Σ record sizes of reachable objects).
    pub reachable_bytes: u64,
    /// Records not reachable from any root (GC reclaimable).
    pub unreachable_bytes: u64,
    /// Snapshot-pinned bytes.
    pub snapshot_pinned_bytes: u64,
    /// GC reserve.
    pub gc_reserve_bytes: u64,
    /// Number of live objects.
    pub object_count: u64,
    /// Number of reachable data records.
    pub data_record_count: u64,
}

impl StoreStats {
    /// Effective ratio (logical / physical reachable).
    pub fn effective_ratio(&self) -> f64 {
        if self.reachable_bytes == 0 {
            0.0
        } else {
            self.logical_bytes as f64 / self.reachable_bytes as f64
        }
    }

    /// Reclaimable capacity.
    pub fn reclaimable(&self) -> u64 {
        self.unreachable_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_basics() {
        let idx = ObjectIndex::new();
        let id = ChunkId::of(b"obj");
        let loc = Location {
            segment_seq: 0,
            offset: 64,
            stored_len: 16,
            materialized_len: Some(16),
            tag: RecordTag::Data,
        };
        assert!(!idx.contains(&id));
        idx.insert(id, loc);
        assert!(idx.contains(&id));
        assert_eq!(idx.get(&id), Some(loc));
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.remove(&id), Some(loc));
        assert!(idx.is_empty());
    }
}
