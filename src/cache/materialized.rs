//! Bounded materialized-chunk cache (ADR-0014).
//!
//! Keys are immutable logical content ids; values are exact materialized
//! bytes. The cache is a strict LRU with a byte budget. It is never
//! authoritative: dropping it affects only performance, never correctness.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::core::extent::ChunkId;

/// A bounded LRU cache of materialized chunks.
#[derive(Debug)]
pub struct MaterializedCache {
    /// Content id → (bytes, recency counter).
    entries: HashMap<ChunkId, (Vec<u8>, u64)>,
    /// Byte budget (total materialized bytes retained).
    budget: u64,
    /// Current total bytes retained.
    used: u64,
    /// Monotonic recency counter.
    clock: u64,
}

impl MaterializedCache {
    /// A cache with the given byte budget.
    pub fn new(budget: u64) -> Self {
        Self {
            entries: HashMap::new(),
            budget,
            used: 0,
            clock: 0,
        }
    }

    /// Look up a chunk; refreshes recency on hit.
    pub fn get(&mut self, id: &ChunkId) -> Option<&[u8]> {
        let entry = self.entries.get_mut(id)?;
        self.clock += 1;
        entry.1 = self.clock;
        Some(entry.0.as_slice())
    }

    /// Insert a chunk, evicting least-recently-used entries beyond the
    /// budget. Duplicate inserts replace. Entries larger than the budget
    /// are not cached at all (they would immediately thrash).
    pub fn insert(&mut self, id: ChunkId, bytes: Vec<u8>) {
        if bytes.len() as u64 > self.budget {
            // Oversized: remove any stale copy, do not cache.
            if let Some((old, _)) = self.entries.remove(&id) {
                self.used = self.used.saturating_sub(old.len() as u64);
            }
            return;
        }
        self.clock += 1;
        if let Some((old, _)) = self.entries.get(&id) {
            self.used = self.used.saturating_sub(old.len() as u64);
        }
        self.used = self.used.saturating_add(bytes.len() as u64);
        self.entries.insert(id, (bytes, self.clock));
        while self.used > self.budget && !self.entries.is_empty() {
            let victim = self
                .entries
                .iter()
                .min_by_key(|(_, (_, rec))| *rec)
                .map(|(k, _)| *k)
                .expect("non-empty");
            if let Some((bytes, _)) = self.entries.remove(&victim) {
                self.used = self.used.saturating_sub(bytes.len() as u64);
            }
        }
    }

    /// Whether the id is present.
    pub fn contains(&self, id: &ChunkId) -> bool {
        self.entries.contains_key(id)
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total retained bytes.
    pub fn used_bytes(&self) -> u64 {
        self.used
    }

    /// Drop everything (performance only).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.used = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_eviction() {
        let mut c = MaterializedCache::new(64);
        c.insert(ChunkId::of(b"a"), vec![0u8; 32]);
        c.insert(ChunkId::of(b"b"), vec![1u8; 32]);
        assert_eq!(c.len(), 2);
        // Touch a; inserting c (32 bytes) must evict b.
        let a = ChunkId::of(b"a");
        assert!(c.get(&a).is_some());
        c.insert(ChunkId::of(b"c"), vec![2u8; 32]);
        assert!(c.contains(&a));
        assert!(!c.contains(&ChunkId::of(b"b")));
        assert!(c.contains(&ChunkId::of(b"c")));
        assert!(c.used_bytes() <= 64);
    }

    #[test]
    fn oversized_single_entry() {
        let mut c = MaterializedCache::new(16);
        let big = ChunkId::of(b"big");
        c.insert(big, vec![7u8; 4096]);
        // Entries larger than the budget are not cached at all.
        assert!(!c.contains(&big));
        assert!(c.is_empty());
        assert_eq!(c.used_bytes(), 0);
    }

    #[test]
    fn replace_updates_size() {
        let mut c = MaterializedCache::new(128);
        let id = ChunkId::of(b"x");
        c.insert(id, vec![0u8; 64]);
        c.insert(id, vec![1u8; 32]);
        assert_eq!(c.len(), 1);
        assert_eq!(c.used_bytes(), 32);
    }
}
