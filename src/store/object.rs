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

/// The derived object index.
#[derive(Debug, Default, Clone)]
pub struct ObjectIndex {
    map: HashMap<ChunkId, Location>,
}

impl ObjectIndex {
    /// Empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a location.
    pub fn insert(&mut self, id: ChunkId, loc: Location) {
        self.map.insert(id, loc);
    }

    /// Look up a location.
    pub fn get(&self, id: &ChunkId) -> Option<&Location> {
        self.map.get(id)
    }

    /// Whether an id is present.
    pub fn contains(&self, id: &ChunkId) -> bool {
        self.map.contains_key(id)
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Iterate all (id, location) pairs (for GC and fsck).
    pub fn iter(&self) -> impl Iterator<Item = (&ChunkId, &Location)> {
        self.map.iter()
    }

    /// Remove an entry (compaction).
    pub fn remove(&mut self, id: &ChunkId) -> Option<Location> {
        self.map.remove(id)
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.map.clear();
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
        let mut idx = ObjectIndex::new();
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
        assert_eq!(idx.get(&id), Some(&loc));
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.remove(&id), Some(loc));
        assert!(idx.is_empty());
    }
}
