//! Namespace-operation regression tests (Phase 3): create/unlink/rmdir/
//! rename/link semantics at the store level.
//!
//! These cover the git-clone failure: a same-parent `rename` previously
//! left both the source and destination entries in the directory tree,
//! and `rmdir` leaked the removed directory's inode with nlink 1.

#![forbid(unsafe_code)]

use tempfile::TempDir;

use crate::store::directory::{self};
use crate::store::inode::InodeData;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig, StoreError};

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x22; 16]).unwrap()
}

fn new_file_entry(mode: u32) -> NewEntry {
    NewEntry::file(mode, 1000, 1000)
}

fn new_dir_entry(mode: u32) -> NewEntry {
    NewEntry::dir(mode, 1000, 1000)
}

fn names(store: &Store, ino: u64) -> Vec<Vec<u8>> {
    let (entries, _) = store.dir_scan(ino, None, usize::MAX).unwrap();
    entries.into_iter().map(|(n, _)| n).collect()
}

#[test]
fn same_parent_rename_moves_entry_exactly_once() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    store
        .create_entry(1, b"a.lock", new_file_entry(0o644), &CrashHooks::none())
        .unwrap();
    let outcome = store
        .rename(1, b"a.lock", 1, b"a", &CrashHooks::none())
        .unwrap();
    // The source name must be gone; the destination must exist; the
    // moved inode must be the source inode.
    assert_eq!(outcome.replaced_dst_ino, None);
    assert!(store.dir_lookup(1, b"a.lock").unwrap().is_none());
    let dst = store.dir_lookup(1, b"a").unwrap().expect("dst exists");
    assert_eq!(dst.ino, outcome.src_ino);
    assert_eq!(names(&store, 1), vec![b"a".to_vec()]);
    // The moved inode survives with nlink 1.
    let inode = store.get_inode(dst.ino).unwrap().expect("inode alive");
    assert_eq!(inode.nlink, 1);
    assert!(inode.is_file());
}

#[test]
fn same_parent_rename_over_existing_replaces_and_drops_old_inode() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    let src = store
        .create_entry(1, b"new", new_file_entry(0o644), &CrashHooks::none())
        .unwrap();
    let old = store
        .create_entry(1, b"old", new_file_entry(0o644), &CrashHooks::none())
        .unwrap();
    let outcome = store
        .rename(1, b"new", 1, b"old", &CrashHooks::none())
        .unwrap();
    assert_eq!(outcome.replaced_dst_ino, Some(old));
    assert_eq!(outcome.src_ino, src);
    let entry = store.dir_lookup(1, b"old").unwrap().expect("dst");
    assert_eq!(entry.ino, src);
    assert_eq!(names(&store, 1), vec![b"old".to_vec()]);
    // The replaced destination inode is gone (nlink hit zero).
    assert!(store.get_inode(old).unwrap().is_none());
}

#[test]
fn same_parent_rename_onto_hardlink_preserves_nlink() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    let ino = store
        .create_entry(1, b"a", new_file_entry(0o644), &CrashHooks::none())
        .unwrap();
    store.link(1, b"b", ino, &CrashHooks::none()).unwrap();
    assert_eq!(store.get_inode(ino).unwrap().unwrap().nlink, 2);
    // Rename b onto a (same inode): one name survives, nlink stays 2.
    let outcome = store.rename(1, b"b", 1, b"a", &CrashHooks::none()).unwrap();
    assert_eq!(outcome.replaced_dst_ino, None);
    assert_eq!(names(&store, 1), vec![b"a".to_vec()]);
    assert_eq!(store.get_inode(ino).unwrap().unwrap().nlink, 2);
}

#[test]
fn cross_parent_rename_moves_entry() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    store
        .create_entry(1, b"d1", new_dir_entry(0o755), &CrashHooks::none())
        .unwrap();
    store
        .create_entry(1, b"d2", new_dir_entry(0o755), &CrashHooks::none())
        .unwrap();
    let ino = store
        .create_entry(2, b"f", new_file_entry(0o644), &CrashHooks::none())
        .unwrap();
    let outcome = store.rename(2, b"f", 3, b"g", &CrashHooks::none()).unwrap();
    assert_eq!(outcome.src_ino, ino);
    assert!(store.dir_lookup(2, b"f").unwrap().is_none());
    assert_eq!(store.dir_lookup(3, b"g").unwrap().unwrap().ino, ino);
}

#[test]
fn cross_parent_dir_rename_adjusts_parent_nlinks() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    store
        .create_entry(1, b"d1", new_dir_entry(0o755), &CrashHooks::none())
        .unwrap();
    store
        .create_entry(1, b"d2", new_dir_entry(0o755), &CrashHooks::none())
        .unwrap();
    // A subdirectory under d1; move it under d2.
    store
        .create_entry(2, b"sub", new_dir_entry(0o755), &CrashHooks::none())
        .unwrap();
    let nlink_d1_before = store.get_inode(2).unwrap().unwrap().nlink;
    let nlink_d2_before = store.get_inode(3).unwrap().unwrap().nlink;
    store
        .rename(2, b"sub", 3, b"sub", &CrashHooks::none())
        .unwrap();
    let nlink_d1_after = store.get_inode(2).unwrap().unwrap().nlink;
    let nlink_d2_after = store.get_inode(3).unwrap().unwrap().nlink;
    assert_eq!(nlink_d1_after, nlink_d1_before - 1);
    assert_eq!(nlink_d2_after, nlink_d2_before + 1);
    // The moved directory itself keeps its nlink (it is not a link).
    let sub = store.dir_lookup(3, b"sub").unwrap().unwrap();
    assert_eq!(store.get_inode(sub.ino).unwrap().unwrap().nlink, 2);
}

#[test]
fn rmdir_removes_directory_inode() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    let ino = store
        .create_entry(1, b"d", new_dir_entry(0o755), &CrashHooks::none())
        .unwrap();
    assert!(store.get_inode(ino).unwrap().is_some());
    store.unlink(1, b"d", true, &CrashHooks::none()).unwrap();
    assert!(store.dir_lookup(1, b"d").unwrap().is_none());
    // The directory inode must be gone, not leaked with nlink 1.
    assert!(store.get_inode(ino).unwrap().is_none());
    // The parent's subdirectory count returns to baseline (2 for root).
    assert_eq!(store.get_inode(1).unwrap().unwrap().nlink, 2);
}

#[test]
fn unlink_file_drops_inode_at_zero_links() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    let ino = store
        .create_entry(1, b"f", new_file_entry(0o644), &CrashHooks::none())
        .unwrap();
    store.unlink(1, b"f", false, &CrashHooks::none()).unwrap();
    assert!(store.get_inode(ino).unwrap().is_none());
}

#[test]
fn unlink_one_hardlink_keeps_inode() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    let ino = store
        .create_entry(1, b"a", new_file_entry(0o644), &CrashHooks::none())
        .unwrap();
    store.link(1, b"b", ino, &CrashHooks::none()).unwrap();
    store.unlink(1, b"a", false, &CrashHooks::none()).unwrap();
    let inode = store.get_inode(ino).unwrap().expect("still linked");
    assert_eq!(inode.nlink, 1);
    assert_eq!(store.dir_lookup(1, b"b").unwrap().unwrap().ino, ino);
}

#[test]
fn rename_rejects_nonempty_dir_over_dir() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    store
        .create_entry(1, b"src", new_dir_entry(0o755), &CrashHooks::none())
        .unwrap();
    store
        .create_entry(1, b"dst", new_dir_entry(0o755), &CrashHooks::none())
        .unwrap();
    // The destination must contain a child to be non-empty.
    store
        .create_entry(3, b"kid", new_file_entry(0o644), &CrashHooks::none())
        .unwrap();
    let err = store
        .rename(1, b"src", 1, b"dst", &CrashHooks::none())
        .unwrap_err();
    assert!(matches!(err, StoreError::Invariant(m) if m == "directory not empty"));
}

#[test]
fn rename_rejects_type_mismatches() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    store
        .create_entry(1, b"f", new_file_entry(0o644), &CrashHooks::none())
        .unwrap();
    store
        .create_entry(1, b"d", new_dir_entry(0o755), &CrashHooks::none())
        .unwrap();
    let err = store
        .rename(1, b"f", 1, b"d", &CrashHooks::none())
        .unwrap_err();
    assert!(matches!(err, StoreError::Invariant(m) if m == "cannot rename file over dir"));
    let err = store
        .rename(1, b"d", 1, b"f", &CrashHooks::none())
        .unwrap_err();
    assert!(matches!(err, StoreError::Invariant(m) if m == "cannot rename dir over file"));
}

#[test]
fn rename_missing_source_errors() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    let err = store
        .rename(1, b"nope", 1, b"x", &CrashHooks::none())
        .unwrap_err();
    assert!(matches!(err, StoreError::Invariant(m) if m == "no such entry"));
}

#[test]
fn rename_noop_is_successful_and_preserves_state() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    store
        .create_entry(1, b"a", new_file_entry(0o644), &CrashHooks::none())
        .unwrap();
    let outcome = store.rename(1, b"a", 1, b"a", &CrashHooks::none()).unwrap();
    assert_eq!(outcome.replaced_dst_ino, None);
    assert_eq!(names(&store, 1), vec![b"a".to_vec()]);
    assert!(store.dir_lookup(1, b"a").unwrap().is_some());
}

#[test]
fn rename_over_empty_dir_succeeds() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    store
        .create_entry(1, b"src", new_dir_entry(0o755), &CrashHooks::none())
        .unwrap();
    store
        .create_entry(1, b"dst", new_dir_entry(0o755), &CrashHooks::none())
        .unwrap();
    let outcome = store
        .rename(1, b"src", 1, b"dst", &CrashHooks::none())
        .unwrap();
    assert_eq!(outcome.src_ino, 2);
    assert_eq!(outcome.replaced_dst_ino, Some(3));
    let entry = store.dir_lookup(1, b"dst").unwrap().unwrap();
    assert_eq!(entry.ino, 2);
    // The replaced empty dir inode is dropped.
    assert!(store.get_inode(3).unwrap().is_none());
}

#[test]
fn git_lock_dance_reproduces_cleanly() {
    // The exact git config-file pattern: O_EXCL create of config.lock,
    // then rename config.lock -> config, repeated twice (git sets several
    // config values during clone).
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    store
        .create_entry(1, b".git", new_dir_entry(0o755), &CrashHooks::none())
        .unwrap();
    // git init writes a bare config first.
    store
        .create_entry(2, b"config", new_file_entry(0o644), &CrashHooks::none())
        .unwrap();
    for _ in 0..2 {
        let lock = store
            .create_entry(
                2,
                b"config.lock",
                new_file_entry(0o644),
                &CrashHooks::none(),
            )
            .unwrap();
        let outcome = store
            .rename(2, b"config.lock", 2, b"config", &CrashHooks::none())
            .unwrap();
        assert_eq!(outcome.replaced_dst_ino, Some(lock - 1));
        // No ghost entry, exactly one name, inode alive.
        assert!(store.dir_lookup(2, b"config.lock").unwrap().is_none());
        let names = names(&store, 2);
        assert_eq!(names, vec![b"config".to_vec()]);
        let cfg = store.dir_lookup(2, b"config").unwrap().unwrap();
        assert!(store.get_inode(cfg.ino).unwrap().is_some());
    }
    // Now the rm -rf pattern: unlink config, rmdir .git.
    store
        .unlink(2, b"config", false, &CrashHooks::none())
        .unwrap();
    assert!(store.get_inode(3).unwrap().is_none());
    store.unlink(1, b".git", true, &CrashHooks::none()).unwrap();
    assert!(store.get_inode(2).unwrap().is_none());
    assert_eq!(names(&store, 1), Vec::<Vec<u8>>::new());
}

#[test]
fn directory_tree_invariants_after_rename_stress() {
    // Interleave same-parent and cross-parent renames; verify the
    // directory B-tree is consistent and all entries resolve.
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    let a = store
        .create_entry(1, b"a", new_file_entry(0o644), &CrashHooks::none())
        .unwrap();
    let b = store
        .create_entry(1, b"b", new_file_entry(0o644), &CrashHooks::none())
        .unwrap();
    let d = store
        .create_entry(1, b"d", new_dir_entry(0o755), &CrashHooks::none())
        .unwrap();
    let _ = b;
    store.rename(1, b"a", 1, b"c", &CrashHooks::none()).unwrap();
    // b replaces c; the replaced file (a's inode) dies, b's inode moves.
    store.rename(1, b"b", 1, b"c", &CrashHooks::none()).unwrap();
    assert!(store.get_inode(a).unwrap().is_none());
    // Move c into directory d (cross-parent).
    store
        .rename(1, b"c", d, b"moved", &CrashHooks::none())
        .unwrap();
    assert!(store.dir_lookup(1, b"c").unwrap().is_none());
    // And back out.
    store
        .rename(d, b"moved", 1, b"c", &CrashHooks::none())
        .unwrap();
    let names = names(&store, 1);
    assert_eq!(names, vec![b"c".to_vec(), b"d".to_vec()]);
    let c = store.dir_lookup(1, b"c").unwrap().unwrap();
    assert!(store.get_inode(c.ino).unwrap().unwrap().is_file());
    let d2 = store.dir_lookup(1, b"d").unwrap().unwrap();
    assert!(store.get_inode(d2.ino).unwrap().unwrap().is_dir());
    // Walk the root directory tree to confirm structure integrity.
    let root = match store.get_inode(1).unwrap().unwrap().data {
        InodeData::Directory { dir_root } => dir_root,
        _ => unreachable!(),
    };
    let limit = store.limits();
    let (scan, _) = directory::scan(root, None, 100, 64, limit.max_fanout, &store).unwrap();
    assert_eq!(scan.len(), 2);
    assert_eq!(scan[0].0, b"c");
    assert_eq!(scan[1].0, b"d");
}
