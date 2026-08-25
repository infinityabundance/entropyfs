//! Concurrency tests (Phase 8, ADR-0013): the store must serve concurrent
//! reads with no global lock, serialize per-inode read-modify-write, and
//! keep parallel writers to different inodes correct.
//!
//! These tests validate the interior-mutability refactor: reads snapshot
//! the committed root, the object index is sharded and append-only while
//! mounted, and only the short commit application is serialized.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::thread;

use crate::store::transaction::CrashHooks;
use crate::store::{Store, StoreConfig};

fn create_store(dir: &tempfile::TempDir) -> Arc<Store> {
    let cfg = StoreConfig {
        segment_size: 8 * 1024 * 1024,
        ..StoreConfig::default()
    };
    let store = Store::create(dir.path(), &cfg, [0x77; 16]).unwrap();
    // Root inode (ino 1) exists; allocate a few file inodes up front.
    for ino in [3u64, 5, 7, 9, 11] {
        let inode = crate::store::inode::Inode::new_file(1000, 1000, 0o644);
        let mut tx = store.begin_tx().unwrap();
        Store::put_inode_in_tx(&mut tx, ino, &inode).unwrap();
        tx.commit(&CrashHooks::none()).unwrap();
    }
    Arc::new(store)
}

fn deterministic(i: u64) -> Vec<u8> {
    // Deterministic per-index payload (incompressible-looking, unique).
    let mut out = Vec::with_capacity(65536);
    let mut s = 0x9e37_79b9_7f4a_7c15u64.wrapping_add(i.wrapping_mul(0xbf58_476d_1ce4_e5b9));
    for _ in 0..65536 {
        s = s.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        out.push((z ^ (z >> 31)) as u8);
    }
    out
}

#[test]
fn concurrent_reads_during_writes() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = create_store(&dir);
    let n_writers = 4;
    let n_readers = 4;
    // Writers: each writes a distinct file (ino 3, 5, 7, 9) repeatedly.
    let mut handles = Vec::new();
    for w in 0..n_writers {
        let store = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            let ino = 3 + 2 * w;
            for round in 0..16u64 {
                let data = deterministic(ino * 1000 + round);
                store.write_region(ino, 0, &data).unwrap();
            }
        }));
    }
    // Readers: read all files in a loop, verifying the content matches a
    // *committed* version (any round's data is acceptable; torn state is
    // not — a read must return a consistent materialization).
    for _ in 0..n_readers {
        let store = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            for _ in 0..64 {
                for ino in [3u64, 5, 7, 9] {
                    let read = store.read_file(ino, 0, 65536).unwrap();
                    // A read must return a *committed* state: the empty
                    // file (before the first commit), one round's full
                    // payload, or a hole (all zeros) — never a torn mix.
                    let ok = read.is_empty()
                        || (read.len() == 65536
                            && ((0..16u64).any(|r| read == deterministic(ino * 1000 + r))
                                || read.iter().all(|&b| b == 0)));
                    assert!(ok, "torn read on ino {ino}: len {}", read.len());
                }
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    // Final state: every file holds its last round.
    for (wi, ino) in [3u64, 5, 7, 9].into_iter().enumerate() {
        let expect = deterministic(ino * 1000 + 15);
        assert_eq!(store.read_file(ino, 0, 65536).unwrap(), expect);
        let _ = wi;
    }
}

#[test]
fn parallel_writers_different_files_exact() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = create_store(&dir);
    let inos = [3u64, 5, 7, 9, 11];
    let mut handles = Vec::new();
    for (i, ino) in inos.into_iter().enumerate() {
        let store = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            let data = deterministic(ino * 777 + 1);
            for off in (0..(4 * 65536)).step_by(65536) {
                store.write_region(ino, off, &data).unwrap();
            }
            let _ = i;
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    for ino in inos {
        let read = store.read_file(ino, 0, 4 * 65536).unwrap();
        assert_eq!(read, deterministic(ino * 777 + 1).repeat(4));
    }
}

#[test]
fn same_file_disjoint_region_writes() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = create_store(&dir);
    // Four threads write disjoint 64 KiB regions of one file. Per-inode
    // serialization must not corrupt the union.
    let mut handles = Vec::new();
    for w in 0..4u64 {
        let store = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            let data = deterministic(w * 31 + 1);
            for round in 0..8u64 {
                let off = w * 65536 + round * 4 * 65536;
                store.write_region(3, off, &data).unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    for w in 0..4u64 {
        for round in 0..8u64 {
            let off = w * 65536 + round * 4 * 65536;
            let read = store.read_file(3, off, 65536).unwrap();
            assert_eq!(read, deterministic(w * 31 + 1));
        }
    }
}

#[test]
fn group_commit_batch_composes_partial_writes() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = create_store(&dir);
    // Four overlapping/adjacent partial writes into one 64 KiB chunk,
    // submitted as ONE batch (group commit). The overlay must compose
    // them correctly regardless of submission order.
    let a = vec![0xAAu8; 1024];
    let b = vec![0xBBu8; 2048];
    let c = vec![0xCCu8; 512];
    let d = vec![0xDDu8; 4096];
    store
        .write_region_batch(
            3,
            &[
                (0, a.clone()),
                (4096, b.clone()),
                (1000, c.clone()),
                (8192, d.clone()),
            ],
            crate::optimizer::policy::OptimizeOptions::default(),
        )
        .unwrap();
    let read = store.read_file(3, 0, 12288).unwrap();
    let mut expect = vec![0u8; 12288];
    expect[..1024].copy_from_slice(&a);
    expect[1000..1512].copy_from_slice(&c);
    expect[4096..6144].copy_from_slice(&b);
    expect[8192..12288].copy_from_slice(&d);
    assert_eq!(read, expect, "batch overlay must compose in offset order");
}

#[test]
fn group_commit_is_one_root() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = create_store(&dir);
    let g0 = store.generation();
    let writes: Vec<(u64, Vec<u8>)> = (0..8u64)
        .map(|i| (i * 65536, deterministic(i + 1)))
        .collect();
    store
        .write_region_batch(
            3,
            &writes,
            crate::optimizer::policy::OptimizeOptions::default(),
        )
        .unwrap();
    // A single batch commit advances the generation exactly once.
    assert_eq!(store.generation(), g0 + 1);
    for (i, (off, data)) in writes.iter().enumerate() {
        let read = store.read_file(3, *off, 65536).unwrap();
        assert_eq!(read, *data, "chunk {i}");
    }
}

#[test]
fn concurrent_fsync_and_writes() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = create_store(&dir);
    let mut handles = Vec::new();
    for w in 0..3u64 {
        let store = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            let ino = 3 + 2 * w;
            for round in 0..8u64 {
                store
                    .write_region(ino, 0, &deterministic(w * 100 + round))
                    .unwrap();
            }
        }));
    }
    // A barrier thread hammers fsync while writers run.
    let store2 = Arc::clone(&store);
    handles.push(thread::spawn(move || {
        for _ in 0..16 {
            store2.durability_barrier(&CrashHooks::none()).unwrap();
        }
    }));
    for h in handles {
        h.join().unwrap();
    }
    // Store remains mountable and consistent.
    drop(store);
    let reopened = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    for w in 0..3u64 {
        let ino = 3 + 2 * w;
        let read = reopened.read_file(ino, 0, 65536).unwrap();
        assert_eq!(read, deterministic(w * 100 + 7));
    }
}
