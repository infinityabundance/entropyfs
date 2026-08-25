//! Bounded decoded rANS model cache (ADR-0014).
//!
//! Model objects are immutable and content-addressed, so a decoded model
//! is a pure memo of its bytes. Decoding a model is comparatively
//! expensive (cumulative table build), so this cache is a real hot-path
//! win for repeated reads. Never authoritative.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::core::extent::ChunkId;
use crate::rans::model::RansModel;

/// A bounded LRU cache of decoded models.
#[derive(Debug)]
pub struct ModelCache {
    entries: HashMap<ChunkId, (RansModel, u64)>,
    capacity: usize,
    clock: u64,
}

impl ModelCache {
    /// A cache holding up to `capacity` models.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
            clock: 0,
        }
    }

    /// Look up a decoded model.
    pub fn get(&mut self, id: &ChunkId) -> Option<RansModel> {
        let entry = self.entries.get_mut(id)?;
        self.clock += 1;
        entry.1 = self.clock;
        Some(entry.0.clone())
    }

    /// Insert a decoded model.
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
