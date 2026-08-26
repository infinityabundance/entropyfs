//! Bounded decoded rANS model cache (ADR-0014).
//!
//! Model objects are immutable and content-addressed, so a decoded model
//! is a pure memo of its bytes. Decoding a model is comparatively
//! expensive (cumulative table build), so this cache is a real hot-path
//! win for repeated reads. Never authoritative.
//!
//! # PURPOSE
//!
//! A bounded LRU cache of decoded rANS models, keyed by the model
//! object's [`ChunkId`]. Reading an extent whose descriptor references a
//! model pays the decode once and then hits here.
//!
//! # BOUNDARY
//!
//! Knows only `(ChunkId → RansModel)`; no descriptors, no store, no
//! format. It must never participate in correctness: dropping every entry
//! changes only latency (`docs/security/resource-bounds.md` §3 gives the
//! models cache a 32 MiB budget).
//!
//! # MODEL
//!
//! A pure memo: model objects are immutable and content-addressed, so the
//! same id always decodes to the same model — memoization is sound by
//! construction. Each entry carries a monotonically increasing recency
//! clock tick; insertion beyond `capacity` evicts the least-recent entry
//! (an LRU-ish policy, ADR-0014: "eviction is LRU-ish and never affects
//! correctness"). [`ModelCache::get`] returns a *clone*, so callers can
//! never corrupt cached state.
//!
//! # PERSISTENT AUTHORITY
//!
//! None. The model bytes live in the store; the cache is rebuilt on
//! demand. This is the ADR-0014 contract: dropping every cache must leave
//! the filesystem fully correct.
//!
//! # CORRECTNESS INVARIANTS
//!
//! - memoization is sound exactly because models are immutable and
//!   content-addressed: same id ⇒ same bytes ⇒ same decoded model;
//! - a miss returns `None`, which the caller treats as "decode and
//!   insert" — never as an error;
//! - eviction is performance-only; a re-decode repairs the entry.
//!
//! # CONCURRENCY
//!
//! `&mut self` on every operation: the caller serializes access (the
//! cache has no internal locking and is not `Sync`).
//!
//! # RESOURCE BOUNDS
//!
//! `capacity` bounds the entry count; the store sizes it against the
//! models memory budget. Eviction scans all entries for the minimum
//! recency tick — `O(n)` per insert, acceptable because the budget caps
//! `n`.
//!
//! # PERFORMANCE
//!
//! Model decode is comparatively expensive (cumulative table build), so
//! repeated reads that share a model object win here. Sharing is common
//! after Phase-9G: one amortized cohort model object is referenced by N
//! extents (ADR-0005), so this cache is where that sharing pays off on
//! the read path.
//!
//! # FAILURE MODES
//!
//! No fallible paths. `get` → `None` on miss; evicting a still-wanted
//! model costs exactly one re-decode.
//!
//! # HISTORY / EVIDENCE
//!
//! ADR-0014 (caches are performance-only, never authoritative);
//! `docs/security/resource-bounds.md` §3 (the budget); Phase-9G model
//! amortization (shared model objects across extents).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::core::extent::ChunkId;
use crate::rans::model::RansModel;

/// A bounded LRU cache of decoded models.
///
/// `entries: ChunkId → (RansModel, last-touch clock tick)`; `clock` is a
/// monotonically increasing counter that never resets while the cache
/// lives. Insertion evicts the entry with the smallest tick when over
/// `capacity` — an `O(n)` victim scan, fine for the small bounded budgets
/// this cache is sized against.
#[derive(Debug)]
pub struct ModelCache {
    entries: HashMap<ChunkId, (RansModel, u64)>,
    capacity: usize,
    clock: u64,
}

impl ModelCache {
    /// A cache holding up to `capacity` entries (models, not bytes; the
    /// caller sizes it against the models memory budget —
    /// `docs/security/resource-bounds.md` §3).
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
            clock: 0,
        }
    }

    /// Look up a decoded model: marks the entry most-recently-used and
    /// returns a clone. `None` on miss — the caller decodes and inserts;
    /// a miss is a performance event, never an error.
    pub fn get(&mut self, id: &ChunkId) -> Option<RansModel> {
        let entry = self.entries.get_mut(id)?;
        self.clock += 1;
        entry.1 = self.clock;
        Some(entry.0.clone())
    }

    /// Insert a decoded model (bumping the recency clock) and evict the
    /// least-recently-used entry if the cache is over `capacity`. A
    /// re-insert of an existing id updates its recency, not its model —
    /// the bytes are immutable, so there is nothing new to learn.
    pub fn insert(&mut self, id: ChunkId, model: RansModel) {
        self.clock += 1;
        self.entries.insert(id, (model, self.clock));
        while self.entries.len() > self.capacity {
            let victim = self
                .entries
                .iter()
                .min_by_key(|(_, (_, rec))| *rec)
                .map(|(k, _)| *k)
                .expect("non-empty");
            self.entries.remove(&victim);
        }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memoizes_decoded_models() {
        let mut c = ModelCache::new(4);
        // Two distinct non-degenerate models.
        let data1: Vec<u8> = (0..4096u32).map(|i| (i % 3) as u8).collect();
        let data2: Vec<u8> = (0..4096u32).map(|i| (i % 7) as u8).collect();
        let m1 = crate::rans::model::normalize_histogram(
            &crate::tests::helpers::histogram_of(&data1),
            8,
            crate::core::representation::RansCodec::Single,
        )
        .unwrap();
        let m2 = crate::rans::model::normalize_histogram(
            &crate::tests::helpers::histogram_of(&data2),
            8,
            crate::core::representation::RansCodec::Single,
        )
        .unwrap();
        let id1 = crate::rans::metadata::model_id(&m1);
        let id2 = crate::rans::metadata::model_id(&m2);
        c.insert(id1, m1);
        c.insert(id2, m2);
        assert!(c.get(&id1).is_some());
        assert!(c.get(&id2).is_some());
        assert_eq!(c.len(), 2);
    }
}
