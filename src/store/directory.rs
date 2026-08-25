//! Directory operations over the persistent B-tree (name → (ino, d_type)).
//!
//! Names are raw bytes (never assumed UTF-8, `docs/adr/0002` §POSIX
//! semantics). `.` and `..` are synthesized by the VFS layer, never stored.

#![forbid(unsafe_code)]

use crate::core::extent::ChunkId;
use crate::format::codec::{CodecError, Reader, Writer};
use crate::store::index::{self, BTreeError, ObjectProvider};

/// A decoded directory entry value: inode number + d_type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirEntry {
    /// Target inode number.
    pub ino: u64,
    /// `d_type` (DT_DIR/DT_REG/DT_LNK/...).
    pub d_type: u8,
}

impl DirEntry {
    /// Encode the value bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u64(self.ino);
        w.u8(self.d_type);
        w.into_bytes()
    }

    /// Decode value bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
        let mut r = Reader::new(bytes);
        let ino = r.u64()?;
        let d_type = r.u8()?;
        if !r.done() {
            return Err(CodecError::Malformed);
        }
        Ok(Self { ino, d_type })
    }
}

/// d_type constants (linux `dirent.h`).
pub mod dt {
    /// Unknown type (kernel will call getattr).
    pub const DT_UNKNOWN: u8 = 0;
    /// Directory.
    pub const DT_DIR: u8 = 4;
    /// Regular file.
    pub const DT_REG: u8 = 8;
    /// Symbolic link.
    pub const DT_LNK: u8 = 10;
    /// Character device.
    pub const DT_CHR: u8 = 2;
    /// Block device.
    pub const DT_BLK: u8 = 6;
    /// FIFO.
    pub const DT_FIFO: u8 = 1;
    /// Socket.
    pub const DT_SOCK: u8 = 12;
}

/// Look up a name in a directory.
pub fn lookup<P: ObjectProvider>(
    dir_root: ChunkId,
    name: &[u8],
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<Option<DirEntry>, BTreeError> {
    match index::get(dir_root, name, order, max_fanout, provider)? {
        Some(bytes) => DirEntry::decode(&bytes)
            .map(Some)
            .map_err(|e| BTreeError::Corrupt(e.to_string())),
        None => Ok(None),
    }
}

/// Insert a name (replaces an existing entry).
pub fn insert<P: ObjectProvider>(
    dir_root: ChunkId,
    name: &[u8],
    entry: DirEntry,
    order: u16,
    max_fanout: u32,
    provider: &mut P,
) -> Result<ChunkId, BTreeError> {
    index::insert(dir_root, name, &entry.encode(), order, max_fanout, provider)
}

/// Remove a name; returns the new root and whether anything was removed.
pub fn remove<P: ObjectProvider>(
    dir_root: ChunkId,
    name: &[u8],
    order: u16,
    max_fanout: u32,
    provider: &mut P,
) -> Result<(ChunkId, bool), BTreeError> {
    let before = lookup(dir_root, name, order, max_fanout, provider)?;
    let new_root = index::remove(dir_root, name, order, max_fanout, provider)?;
    Ok((new_root, before.is_some()))
}

/// Scan output: (name, entry) pairs in name order plus whether more
/// entries remain beyond `limit`.
pub type DirScan = (Vec<(Vec<u8>, DirEntry)>, bool);

/// Scan directory entries in name order (bounded by `limit`).
pub fn scan<P: ObjectProvider>(
    dir_root: ChunkId,
    start_after: Option<&[u8]>,
    limit: usize,
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<DirScan, BTreeError> {
    // `start_after` is exclusive; the B-tree scan is inclusive, so use
    // successor semantics: scan from start_after and skip the first entry
    // if it equals start_after.
    let start = start_after.map(|s| s.to_vec());
    let (entries, has_more, _) = index::scan(
        dir_root,
        start.as_deref(),
        None,
        limit.saturating_add(1),
        order,
        max_fanout,
        provider,
    )?;
    let mut out = Vec::new();
    let mut more = has_more;
    for (k, v) in entries {
        if let Some(sa) = &start_after {
            if k.as_slice() == *sa {
                continue;
            }
        }
        if out.len() >= limit {
            more = true;
            break;
        }
        match DirEntry::decode(&v) {
            Ok(e) => out.push((k, e)),
            Err(e) => return Err(BTreeError::Corrupt(e.to_string())),
        }
    }
    Ok((out, more))
}

/// Count entries (for stat).
pub fn count<P: ObjectProvider>(
    dir_root: ChunkId,
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<u64, BTreeError> {
    index::count(dir_root, order, max_fanout, provider)
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
    fn insert_lookup_remove_scan() {
        let mut p = MemProvider::default();
        let mut root = ChunkId::ZERO;
        root = insert(
            root,
            b"alpha",
            DirEntry {
                ino: 10,
                d_type: dt::DT_REG,
            },
            ORDER,
            4096,
            &mut p,
        )
        .unwrap();
        root = insert(
            root,
            b"beta",
            DirEntry {
                ino: 11,
                d_type: dt::DT_DIR,
            },
            ORDER,
            4096,
            &mut p,
        )
        .unwrap();
        root = insert(
            root,
            b"gamma",
            DirEntry {
                ino: 12,
                d_type: dt::DT_LNK,
            },
            ORDER,
            4096,
            &mut p,
        )
        .unwrap();
        assert_eq!(
            lookup(root, b"beta", ORDER, 4096, &p).unwrap(),
            Some(DirEntry {
                ino: 11,
                d_type: dt::DT_DIR
            })
        );
        assert_eq!(lookup(root, b"missing", ORDER, 4096, &p).unwrap(), None);
        let (entries, more) = scan(root, None, 100, ORDER, 4096, &p).unwrap();
        assert!(!more);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].0, b"alpha");
        // bounded scan with start_after
        let (page2, more2) = scan(root, Some(b"alpha"), 1, ORDER, 4096, &p).unwrap();
        assert!(more2);
        assert_eq!(page2[0].0, b"beta");
        // remove
        let (new_root, removed) = remove(root, b"beta", ORDER, 4096, &mut p).unwrap();
        assert!(removed);
        assert_eq!(lookup(new_root, b"beta", ORDER, 4096, &p).unwrap(), None);
        assert_eq!(count(new_root, ORDER, 4096, &p).unwrap(), 2);
    }

    #[test]
    fn raw_bytes_names() {
        let mut p = MemProvider::default();
        let mut root = ChunkId::ZERO;
        // Names with invalid UTF-8 must work.
        let weird: &[u8] = &[0xFF, 0xFE, 0x00, 0x41];
        root = insert(
            root,
            weird,
            DirEntry {
                ino: 7,
                d_type: dt::DT_REG,
            },
            ORDER,
            4096,
            &mut p,
        )
        .unwrap();
        assert_eq!(
            lookup(root, weird, ORDER, 4096, &p).unwrap(),
            Some(DirEntry {
                ino: 7,
                d_type: dt::DT_REG
            })
        );
    }
}
