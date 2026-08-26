//! Phase 6 durability tests: deferred commits are process-crash safe,
//! power-durable only after `durability_barrier` (fsync), and recovery
//! falls back to the newest valid root record when a stale superblock slot
//! references a lost root.

#![forbid(unsafe_code)]

use tempfile::TempDir;

use crate::store::io::IoBackendKind;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};

fn create_store(dir: &TempDir, kind: IoBackendKind) -> Store {
    let cfg = StoreConfig {
        segment_size: 1024 * 1024,
        io_backend: kind,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0xBB; 16]).unwrap()
}

fn config_for(kind: IoBackendKind) -> StoreConfig {
    StoreConfig {
        io_backend: kind,
        ..Default::default()
    }
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

/// Truncate every segment that grew past `cutoff` back to `cutoff`
/// (simulates a power loss destroying everything beyond the last fsync).
fn power_loss_truncate(dir: &TempDir, cutoff: u64) {
    let segs: Vec<_> = std::fs::read_dir(dir.path().join("segments"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().map(|e| e == "seg").unwrap_or(false))
        .collect();
    for p in &segs {
        if let Ok(md) = std::fs::metadata(p) {
            if md.len() > cutoff {
                let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
                f.set_len(cutoff).unwrap();
            }
        }
    }
}

#[test]
fn deferred_writes_survive_process_crash() {
    // POSIX: a write must survive process termination (only power loss may
    // lose un-fsynced data). Deferred commits flush to the page cache and
    // write the superblock slot, so a daemon kill + remount sees the data.
    for kind in IoBackendKind::ALL {
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir, kind);
        let f = ino(&store);
        store.write_region(f, 0, b"process-crash-safe").unwrap();
        // Drop WITHOUT a durability barrier — simulates the daemon dying.
        drop(store);
        let store = Store::open(dir.path(), &config_for(kind)).unwrap();
        let read = store.read_file(f, 0, 64).unwrap();
        assert_eq!(read, b"process-crash-safe");
    }
}

#[test]
fn power_loss_keeps_only_barrier_d_data_and_never_wedges() {
    // fsync'd data survives a power loss; un-fsynced deferred writes are
    // lost (POSIX). The store must mount at the last barrier'd state and
    // fsck must be clean.
    for kind in IoBackendKind::ALL {
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir, kind);
        let f = ino(&store);
        store.write_region(f, 0, b"fsynced-data").unwrap();
        store.durability_barrier(&CrashHooks::none()).unwrap();
        // The durable segment size: everything at/after this is un-fsynced.
        let seg0 = dir.path().join("segments/0000000000000000.seg");
        let durable_size = std::fs::metadata(&seg0).unwrap().len();
        // Deferred (un-fsynced) writes.
        store.write_region(f, 0, b"LOST-ON-POWER-FAILURE").unwrap();
        store.write_region(f, 0, b"ALSO-LOST").unwrap();
        // Power loss: clear everything beyond the last fsync.
        power_loss_truncate(&dir, durable_size);
        drop(store);
        let store = Store::open(dir.path(), &config_for(kind)).unwrap();
        let read = store.read_file(f, 0, 64).unwrap();
        assert_eq!(
            read, b"fsynced-data",
            "power loss must revert to the barrier'd state"
        );
        // The store remains usable.
        let store = store;
        store.write_region(f, 0, b"post-power").unwrap();
        store.durability_barrier(&CrashHooks::none()).unwrap();
        let read = store.read_file(f, 0, 64).unwrap();
        assert!(read.starts_with(b"post-power"), "got {read:?}");
        let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
        assert!(
            report.is_clean(),
            "fsck after power-loss recovery: {}",
            report.render()
        );
    }
}

#[test]
fn recovery_falls_back_to_newest_valid_root_record() {
    // Both superblock slots reference lost roots (worst case after power
    // loss with several un-fsynced commits): recovery must scan the
    // segments for the newest valid ROOT record.
    for kind in IoBackendKind::ALL {
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir, kind);
        let f = ino(&store);
        store.write_region(f, 0, b"root-a").unwrap();
        store.durability_barrier(&CrashHooks::none()).unwrap();
        let seg0 = dir.path().join("segments/0000000000000000.seg");
        let barrier_size = std::fs::metadata(&seg0).unwrap().len();
        // Two deferred commits (no barrier) — both slots now reference
        // non-durable roots.
        store.write_region(f, 0, b"root-b").unwrap();
        store.write_region(f, 0, b"root-c").unwrap();
        // Simulate power loss: truncate the segment back to the barrier size.
        power_loss_truncate(&dir, barrier_size);
        // Also corrupt both superblock slots so only the segment fallback can
        // recover.
        let sb_path = dir.path().join("superblock");
        for off in [0u64, 4096] {
            use std::io::{Seek, SeekFrom, Write};
            let mut fh = std::fs::OpenOptions::new()
                .write(true)
                .open(&sb_path)
                .unwrap();
            fh.seek(SeekFrom::Start(off)).unwrap();
            fh.write_all(&[0xFF; 512]).unwrap();
        }
        drop(store);
        let store = Store::open(dir.path(), &config_for(kind)).unwrap();
        let read = store.read_file(f, 0, 64).unwrap();
        assert_eq!(
            read, b"root-a",
            "fallback must recover the last barrier'd root"
        );
        // The store remains writable after the fallback recovery.
        let store = store;
        store.write_region(f, 0, b"root-d").unwrap();
        store.durability_barrier(&CrashHooks::none()).unwrap();
        let read = store.read_file(f, 0, 64).unwrap();
        assert_eq!(read, b"root-d");
        let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
        assert!(
            report.is_clean(),
            "fsck after fallback recovery: {}",
            report.render()
        );
    }
}
