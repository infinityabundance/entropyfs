//! Bounded inode-attribute cache (ADR-0014).
//!
//! The store's inode tree is authoritative; this cache memoizes decoded
//! inodes by inode number for the FUSE attribute TTL window. Eviction
//! affects only performance.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::store::inode::Inode;

/// A bounded LRU cache of decoded inodes.
#[derive(Debug)]
pub struct MetadataCache {
    entries: HashMap<u64, (Inode, u64)>,
    capacity: usize,
    clock: u64,
}

impl MetadataCache {
    /// A cache holding up to `capacity` inodes.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
            clock: 0,
        }
    }

    /// Look up by inode number.
    pub fn get(&mut self, ino: u64) -> Option<Inode> {
        let entry = self.entries.get_mut(&ino)?;
        self.clock += 1;
        entry.1 = self.clock;
        Some(entry.0.clone())
    }

    /// Insert or replace an inode.
    pub fn insert(&mut self, ino: u64, inode: Inode) {
        self.clock += 1;
        self.entries.insert(ino, (inode, self.clock));
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

    /// Invalidate one inode.
    pub fn invalidate(&mut self, ino: u64) {
        self.entries.remove(&ino);
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
    fn bounded_and_lru() {
        let mut c = MetadataCache::new(2);
        fn mk() -> Inode {
            Inode::new_file(0, 0, 0o644)
        }
        let a = mk();
        let b = mk();
        c.insert(1, a.clone());
        c.insert(2, b.clone());
        // Touch 1; insert 3 evicts 2.
        assert!(c.get(1).is_some());
        c.insert(3, mk());
        assert_eq!(c.len(), 2);
        assert!(c.get(1).is_some());
        assert!(c.get(2).is_none());
        assert!(c.get(3).is_some());
        c.invalidate(1);
        assert!(c.get(1).is_none());
    }
}
