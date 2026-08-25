//! Extent tree operations: persistent B-tree mapping logical offset →
//! extent descriptor bytes (`docs/architecture/read-path.md`).
//!
//! Extents never overlap and are strictly ordered; fsck verifies this.
//! Gaps are holes (materialize as ZERO).

#![forbid(unsafe_code)]

use crate::core::extent::ChunkId;
use crate::store::index::{self, BTreeError, ObjectProvider};

/// Key for an extent: 8-byte big-endian logical offset (lexicographic
/// order == numeric order).
pub fn extent_key(offset: u64) -> [u8; 8] {
    offset.to_be_bytes()
}

/// Find the first extent start strictly greater than `pos` (hole skip).
pub fn next_extent_start<P: ObjectProvider>(
    extent_root: ChunkId,
    pos: u64,
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<Option<u64>, BTreeError> {
    let (entries, _) = scan_range(
        extent_root,
        pos.saturating_add(1),
        u64::MAX,
        1,
        order,
        max_fanout,
        provider,
    )?;
    Ok(entries.into_iter().next().map(|(start, _)| start))
}

/// Look up the extent covering `offset` (the greatest extent with
/// start <= offset). Returns (extent_start, descriptor bytes).
pub fn covering<P: ObjectProvider>(
    extent_root: ChunkId,
    offset: u64,
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<Option<(u64, Vec<u8>)>, BTreeError> {
    // The predecessor of offset+1 is the extent with the largest start
    // <= offset (extent keys are exact starts).
    let key = extent_key(offset.saturating_add(1));
    match index::predecessor(extent_root, &key, order, max_fanout, provider)? {
        Some((k, v)) => {
            let start = u64::from_be_bytes(k.as_slice().try_into().expect("8-byte key"));
            Ok(Some((start, v)))
        }
        None => Ok(None),
    }
}

/// Insert (or replace) the extent at `offset`.
pub fn insert<P: ObjectProvider>(
    extent_root: ChunkId,
    offset: u64,
    descriptor_bytes: &[u8],
    order: u16,
    max_fanout: u32,
    provider: &mut P,
) -> Result<ChunkId, BTreeError> {
    index::insert(
        extent_root,
        &extent_key(offset),
        descriptor_bytes,
        order,
        max_fanout,
        provider,
    )
}

/// Remove the extent at exactly `offset`; returns whether present.
pub fn remove<P: ObjectProvider>(
    extent_root: ChunkId,
    offset: u64,
    order: u16,
    max_fanout: u32,
    provider: &mut P,
) -> Result<(ChunkId, bool), BTreeError> {
    let before = index::get(
        extent_root,
        &extent_key(offset),
        order,
        max_fanout,
        provider,
    )?
    .is_some();
    let new_root = index::remove(
        extent_root,
        &extent_key(offset),
        order,
        max_fanout,
        provider,
    )?;
    Ok((new_root, before))
}

/// Scan output: (start, descriptor bytes) pairs in offset order plus
/// whether more extents remain beyond `limit`.
pub type ExtentScan = (Vec<(u64, Vec<u8>)>, bool);

/// Scan extents in `[start_offset, end_offset)` (inclusive lower, exclusive
/// upper). Returns (start, descriptor bytes) pairs in order.
pub fn scan_range<P: ObjectProvider>(
    extent_root: ChunkId,
    start_offset: u64,
    end_offset: u64,
    limit: usize,
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<ExtentScan, BTreeError> {
    let (entries, has_more, _) = index::scan(
        extent_root,
        Some(&extent_key(start_offset)),
        Some(&extent_key(end_offset)),
        limit,
        order,
        max_fanout,
        provider,
    )?;
    let out = entries
        .into_iter()
        .map(|(k, v)| {
            let start = u64::from_be_bytes(k.as_slice().try_into().expect("8-byte key"));
            (start, v)
        })
        .collect();
    Ok((out, has_more))
}

/// Scan all extents (for fsck / background optimizer).
pub fn scan_all<P: ObjectProvider>(
    extent_root: ChunkId,
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<Vec<(u64, Vec<u8>)>, BTreeError> {
    let (entries, _, _) = index::scan(
        extent_root,
        None,
        None,
        usize::MAX,
        order,
        max_fanout,
        provider,
    )?;
    Ok(entries
        .into_iter()
        .map(|(k, v)| {
            let start = u64::from_be_bytes(k.as_slice().try_into().expect("8-byte key"));
            (start, v)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::index::{BTreeError, ObjectProvider};
    use std::collections::HashMap;

    #[derive(Default)]
    struct MemProvider {
        nodes: HashMap<ChunkId, Vec<u8>>,
    }

    impl ObjectProvider for MemProvider {
        fn get(&self, id: &ChunkId) -> Result<Option<Vec<u8>>, BTreeError> {
            Ok(self.nodes.get(id).cloned())
        }
        fn put(&mut self, id: ChunkId, bytes: Vec<u8>) {
            self.nodes.insert(id, bytes);
        }
    }

    const ORDER: u16 = 8;

    #[test]
    fn insert_covering_scan_remove() {
        let mut p = MemProvider::default();
        let mut root = ChunkId::ZERO;
        for (off, tag) in [(0u64, 1u8), (65536, 2), (131072, 3)] {
            root = insert(root, off, &[tag, 0, 0, 0], ORDER, 4096, &mut p).unwrap();
        }
        // covering lookups
        let (start, v) = covering(root, 100, ORDER, 4096, &p).unwrap().unwrap();
        assert_eq!(start, 0);
        assert_eq!(v, vec![1, 0, 0, 0]);
        let (start, v) = covering(root, 65536, ORDER, 4096, &p).unwrap().unwrap();
        assert_eq!(start, 65536);
        assert_eq!(v, vec![2, 0, 0, 0]);
        // range scan: extents whose START is in [100, 200000) => two of
        // them (the extent at 0 starts before the range).
        let (range, _) = scan_range(root, 100, 200000, 100, ORDER, 4096, &p).unwrap();
        assert_eq!(range.len(), 2);
        let (all, _) = scan_range(root, 0, 200000, 100, ORDER, 4096, &p).unwrap();
        assert_eq!(all.len(), 3);
        // remove middle
        let (new_root, removed) = remove(root, 65536, ORDER, 4096, &mut p).unwrap();
        assert!(removed);
        let (start, v) = covering(new_root, 70000, ORDER, 4096, &p).unwrap().unwrap();
        // 70000 now falls in the hole between 0-extent and 131072-extent;
        // the predecessor is the 0 extent.
        assert_eq!(start, 0);
        assert_eq!(v, vec![1, 0, 0, 0]);
    }
}
