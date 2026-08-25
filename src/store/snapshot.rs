//! Snapshot tree: name → snapshot root (ADR-0007, §5).

#![forbid(unsafe_code)]

use crate::core::extent::ChunkId;
use crate::format::codec::{CodecError, Reader, Writer};
use crate::store::index::{self, BTreeError, ObjectProvider};

/// A snapshot entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotEntry {
    /// The pinned root object id.
    pub root_id: ChunkId,
    /// Creation time (unix nanos).
    pub created_unix_ns: u64,
}

impl SnapshotEntry {
    /// Encode the value bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.bytes(self.root_id.as_bytes());
        w.u64(self.created_unix_ns);
        w.into_bytes()
    }

    /// Decode value bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
        let mut r = Reader::new(bytes);
        let root_id = ChunkId::new(r.take(32)?.try_into().unwrap());
        let created_unix_ns = r.u64()?;
        if !r.done() {
            return Err(CodecError::Malformed);
        }
        Ok(Self {
            root_id,
            created_unix_ns,
        })
    }
}

/// Look up a snapshot by name.
pub fn lookup<P: ObjectProvider>(
    tree_root: ChunkId,
    name: &[u8],
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<Option<SnapshotEntry>, BTreeError> {
    match index::get(tree_root, name, order, max_fanout, provider)? {
        Some(bytes) => SnapshotEntry::decode(&bytes)
            .map(Some)
            .map_err(|e| BTreeError::Corrupt(e.to_string())),
        None => Ok(None),
    }
}

/// Insert a snapshot.
pub fn insert<P: ObjectProvider>(
    tree_root: ChunkId,
    name: &[u8],
    entry: SnapshotEntry,
    order: u16,
    max_fanout: u32,
    provider: &mut P,
) -> Result<ChunkId, BTreeError> {
    index::insert(
        tree_root,
        name,
        &entry.encode(),
        order,
        max_fanout,
        provider,
    )
}

/// Remove a snapshot; returns whether present.
pub fn remove<P: ObjectProvider>(
    tree_root: ChunkId,
    name: &[u8],
    order: u16,
    max_fanout: u32,
    provider: &mut P,
) -> Result<(ChunkId, bool), BTreeError> {
    let before = lookup(tree_root, name, order, max_fanout, provider)?.is_some();
    let new_root = index::remove(tree_root, name, order, max_fanout, provider)?;
    Ok((new_root, before))
}

/// List all snapshots in name order.
pub fn list<P: ObjectProvider>(
    tree_root: ChunkId,
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<Vec<(Vec<u8>, SnapshotEntry)>, BTreeError> {
    let entries = index::scan_all(tree_root, order, max_fanout, provider)?;
    let mut out = Vec::with_capacity(entries.len());
    for (k, v) in entries {
        match SnapshotEntry::decode(&v) {
            Ok(e) => out.push((k, e)),
            Err(e) => return Err(BTreeError::Corrupt(e.to_string())),
        }
    }
    Ok(out)
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
    fn snapshot_tree_ops() {
        let mut p = MemProvider::default();
        let mut root = ChunkId::ZERO;
        root = insert(
            root,
            b"before-upgrade",
            SnapshotEntry {
                root_id: ChunkId::of(b"rootA"),
                created_unix_ns: 1,
            },
            ORDER,
            4096,
            &mut p,
        )
        .unwrap();
        root = insert(
            root,
            b"after-upgrade",
            SnapshotEntry {
                root_id: ChunkId::of(b"rootB"),
                created_unix_ns: 2,
            },
            ORDER,
            4096,
            &mut p,
        )
        .unwrap();
        assert_eq!(
            lookup(root, b"before-upgrade", ORDER, 4096, &p)
                .unwrap()
                .unwrap()
                .root_id,
            ChunkId::of(b"rootA")
        );
        let all = list(root, ORDER, 4096, &p).unwrap();
        assert_eq!(all.len(), 2);
        let (new_root, removed) = remove(root, b"before-upgrade", ORDER, 4096, &mut p).unwrap();
        assert!(removed);
        assert_eq!(list(new_root, ORDER, 4096, &p).unwrap().len(), 1);
    }
}
