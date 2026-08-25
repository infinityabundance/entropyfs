//! Mount-time recovery: rebuild the derived index, load the root, verify
//! structural reachability (`docs/recovery/crash-consistency.md`).

#![forbid(unsafe_code)]

use crate::store::Store;
use crate::store::StoreError;
use crate::store::index::{self, BTreeError};
use crate::store::inode::{Inode, InodeData};

/// Verify the chosen root's structural reachability (bounded, depth-capped
/// walk). This is the mount-time "is the tree walkable" check; fsck does
/// the full deep verification.
pub fn verify_root(store: &Store) -> Result<(), StoreError> {
    let root = store.current_root();
    // Inode index walk.
    let count = index::verify(
        root.inode_index_root,
        crate::store::BTREE_ORDER,
        store.config().limits.max_fanout,
        store,
    )
    .map_err(|e| StoreError::Index(e.to_string()))?;
    // Chunk index walk.
    let chunk_count = index::verify(
        root.chunk_index_root,
        crate::store::BTREE_ORDER,
        store.config().limits.max_fanout,
        store,
    )
    .map_err(|e| StoreError::Index(e.to_string()))?;
    // Snapshot tree walk (zero root is fine).
    if !root.snapshot_tree_root.is_zero() {
        index::verify(
            root.snapshot_tree_root,
            crate::store::BTREE_ORDER,
            store.config().limits.max_fanout,
            store,
        )
        .map_err(|e| StoreError::Index(e.to_string()))?;
    }
    // Root directory must exist.
    if root.root_dir_ino == 0 {
        return Err(StoreError::Superblock("root dir inode is zero".into()));
    }
    let root_inode = store
        .get_inode(root.root_dir_ino)?
        .ok_or_else(|| StoreError::Superblock("root dir inode missing".into()))?;
    if !root_inode.is_dir() {
        return Err(StoreError::Superblock(
            "root inode is not a directory".into(),
        ));
    }
    // Every inode's extent/dir tree must be walkable.
    let inodes = store.all_inodes()?;
    for ino in inodes {
        let inode = store
            .get_inode(ino)?
            .ok_or_else(|| StoreError::Invariant(format!("inode {ino} referenced but missing")))?;
        verify_inode_trees(store, &inode)?;
    }
    let _ = (count, chunk_count);
    Ok(())
}

/// Verify one inode's tree roots.
pub fn verify_inode_trees(store: &Store, inode: &Inode) -> Result<(), StoreError> {
    match &inode.data {
        InodeData::Directory { dir_root } if !dir_root.is_zero() => {
            index::verify(
                *dir_root,
                crate::store::BTREE_ORDER,
                store.config().limits.max_fanout,
                store,
            )
            .map_err(|e| StoreError::Index(e.to_string()))?;
        }
        InodeData::File { extent_root } if !extent_root.is_zero() => {
            index::verify(
                *extent_root,
                crate::store::BTREE_ORDER,
                store.config().limits.max_fanout,
                store,
            )
            .map_err(|e| StoreError::Index(e.to_string()))?;
        }
        _ => {}
    }
    Ok(())
}

/// BTreeError conversion helper.
pub fn btree_err(e: BTreeError) -> StoreError {
    StoreError::Index(e.to_string())
}
