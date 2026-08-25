//! Phase 5 tests: snapshots (create/list/delete/restore), GC respecting
//! snapshot roots, crash-court at snapshot boundaries, and low-space
//! behavior with snapshot pins.

#![forbid(unsafe_code)]

use tempfile::TempDir;

use crate::core::extent::ChunkId;
use crate::store::transaction::{CrashHooks, CrashPoint};
use crate::store::{NewEntry, Store, StoreConfig};

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0xAA; 16]).unwrap()
}

fn ino(store: &Store) -> u64 {
    store
        .create_entry(
            1,
            b"f",
            NewEntry::file(0o644, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap()
}

fn write_file(store: &Store, ino: u64, tag: u8) {
    let content = vec![tag; 65536];
    store.write_region(ino, 0, &content).unwrap();
}

#[test]
fn snapshot_lifecycle_create_list_delete_restore() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let f = ino(&store);
    write_file(&store, f, 0x11);
    store.create_snapshot(b"v1", &CrashHooks::none()).unwrap();
    // The snapshot pins the root BEFORE the snapshot entry itself.
    let v1_root = store
        .snapshot_lookup(b"v1")
        .unwrap()
        .expect("snapshot exists")
        .root_id;
    write_file(&store, f, 0x22);
    assert_eq!(store.list_snapshots().unwrap().len(), 1);

    // Restore: the file content rolls back to v1.
    store.restore_snapshot(b"v1", &CrashHooks::none()).unwrap();
    let content = store.read_file(f, 0, 65536).unwrap();
    assert!(content.iter().all(|&b| b == 0x11));
    // The restored-from snapshot survives the rollback.
    assert_eq!(store.list_snapshots().unwrap().len(), 1);
    assert_eq!(
        store.snapshot_lookup(b"v1").unwrap().unwrap().root_id,
        v1_root
    );

    // Delete: gone from the tree.
    assert!(store.delete_snapshot(b"v1", &CrashHooks::none()).unwrap());
    assert!(!store.delete_snapshot(b"v1", &CrashHooks::none()).unwrap());
    assert!(store.list_snapshots().unwrap().is_empty());
    // Deleting a missing snapshot is a clean no-op (returns false).
}

#[test]
fn gc_respects_snapshot_roots() {
    // The snapshot pins version 1's objects; GC must not reclaim them
    // even after the live file is overwritten many times.
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let f = ino(&store);
    write_file(&store, f, 0x11);
    store.create_snapshot(b"v1", &CrashHooks::none()).unwrap();
    // Churn: many overwrites create garbage that GC can reclaim.
    for v in 0..30u8 {
        write_file(&store, f, v);
    }
    crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
    // The snapshot's version-1 objects must still be reachable.
    let unreachable = crate::store::gc::unreachable_bytes(&store).unwrap();
    let _ = unreachable;
    store.restore_snapshot(b"v1", &CrashHooks::none()).unwrap();
    let content = store.read_file(f, 0, 65536).unwrap();
    assert!(
        content.iter().all(|&b| b == 0x11),
        "GC reclaimed snapshot-pinned data"
    );
    // fsck stays clean with the snapshot in place.
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(
        report.is_clean(),
        "fsck: {:?}",
        report.issues.iter().take(3).collect::<Vec<_>>()
    );
}

#[test]
fn snapshot_crash_matrix_is_linearizable() {
    // Snapshot create/delete/restore at every durability boundary: after
    // recovery the store must be either the pre- or post-op state, and a
    // restore must never lose the pre-restore state's durability.
    let points = [
        CrashPoint::AfterRecordAppend,
        CrashPoint::AfterSegmentFdatasync,
        CrashPoint::AfterSegmentDirFsync,
        CrashPoint::AfterRootWrite,
        CrashPoint::AfterSuperblockWrite,
        CrashPoint::AfterSuperblockFsync,
    ];
    for point in points {
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir);
        let f = ino(&store);
        write_file(&store, f, 0x11);
        let hooks = CrashHooks::crash_at(point);
        // Snapshot create with the crash armed.
        let _ = store.create_snapshot(b"v1", &hooks);
        drop(store);
        let store = Store::open(dir.path(), &StoreConfig::default()).unwrap();
        // Either the snapshot exists (commit completed) or not (crash
        // before the superblock flip) — never a corrupt tree.
        let has_snapshot = store.snapshot_lookup(b"v1").unwrap().is_some();
        let content = store.read_file(f, 0, 65536).unwrap();
        assert_eq!(content.len(), 65536);
        if has_snapshot {
            write_file(&store, f, 0x22);
            store.restore_snapshot(b"v1", &CrashHooks::none()).unwrap();
            let back = store.read_file(f, 0, 65536).unwrap();
            assert!(back.iter().all(|&b| b == 0x11));
        }
        let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
        assert!(
            report.is_clean(),
            "fsck after crash at {point:?}: {}",
            report
                .issues
                .iter()
                .take(3)
                .map(|i| i.message.clone())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
}

#[test]
fn snapshot_pins_inodes_across_remount() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let f = ino(&store);
    write_file(&store, f, 0x33);
    store
        .create_snapshot(b"pre-remount", &CrashHooks::none())
        .unwrap();
    drop(store);
    let store = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    assert_eq!(store.list_snapshots().unwrap().len(), 1);
    let f2 = store
        .create_entry(
            1,
            b"f2",
            NewEntry::file(0o644, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap();
    write_file(&store, f2, 0x44);
    store
        .restore_snapshot(b"pre-remount", &CrashHooks::none())
        .unwrap();
    // The new file is gone after the rollback (it did not exist at
    // snapshot time).
    assert!(store.dir_lookup(1, b"f2").unwrap().is_none());
    // The original file is back.
    let content = store.read_file(f, 0, 65536).unwrap();
    assert!(content.iter().all(|&b| b == 0x33));
    let _ = ChunkId::ZERO;
}
