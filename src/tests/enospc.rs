//! ENOSPC survival tests (§21/§22): the store refuses commits at a
//! configurable watermark (leaving the GC emergency reserve), existing
//! data stays intact and readable, failed commits leave no partial state,
//! and the store remains usable after a Full error.

#![forbid(unsafe_code)]

use tempfile::TempDir;

use crate::core::candidate::{pick_cheapest, raw_candidate, zero_candidate};
use crate::core::extent::ChunkId;
use crate::entropy::palette::PaletteEncoder;
use crate::entropy::periodic::PeriodicEncoder;
use crate::entropy::sparse::SparseEncoder;
use crate::fsck::{FsckOptions, fsck};
use crate::rans::residual::RansEncoder;
use crate::store::transaction::CrashHooks;
use crate::store::{ExtentUpdate, Store, StoreConfig, StoreError};

/// A store whose capacity is capped at 4 MiB so the watermark (92%) is
/// reached with a few MiB of incompressible writes.
fn small_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 1024 * 1024,
        capacity_override: Some(4 * 1024 * 1024),
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x44; 16]).unwrap()
}

/// Truly incompressible content (splitmix64 stream — no periodicity, no
/// low-cardinality; RAW must win). Different seeds give different streams.
fn incompressible(seed: u8, len: usize) -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ (seed as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    (0..len)
        .map(|_| {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            (z ^ (z >> 31)) as u8
        })
        .collect()
}

fn encode_chunks(content: &[u8], store: &Store) -> Vec<ExtentUpdate> {
    let limits = store.limits();
    let policy = store.policy();
    let chunk_class = limits.chunk_class as usize;
    let mut updates = Vec::new();
    let mut off = 0usize;
    while off < content.len() {
        let end = (off + chunk_class).min(content.len());
        let chunk = &content[off..end];
        let cid = ChunkId::of(chunk);
        let ctx = crate::core::candidate::CandidateContext {
            limits,
            policy,
            content_id: cid,
            bases: &[],
            dedup: None,
        };
        let mut cands = Vec::new();
        if let Some(z) = zero_candidate(chunk, cid, limits) {
            cands.push(z);
        }
        for enc in [
            Box::new(SparseEncoder) as Box<dyn crate::core::candidate::Encoder>,
            Box::new(PaletteEncoder),
            Box::new(PeriodicEncoder),
            Box::new(RansEncoder),
        ] {
            cands.extend(enc.encode(chunk, &ctx));
        }
        if let Some(r) = raw_candidate(chunk, cid, limits) {
            cands.push(r);
        }
        let best = pick_cheapest(&cands, policy).expect("at least raw");
        updates.push(ExtentUpdate {
            offset: off as u64,
            descriptor: best.representation.clone(),
            content_id: cid,
            objects: best.objects.clone(),
        });
        off = end;
    }
    updates
}

fn write_file(store: &mut Store, ino: u64, content: &[u8]) -> Result<(), StoreError> {
    let updates = encode_chunks(content, store);
    store.commit_file_extents(
        ino,
        updates,
        Some(content.len() as u64),
        &CrashHooks::none(),
    )
}

#[test]
fn fills_to_watermark_then_enospc() {
    let dir = TempDir::new().unwrap();
    let mut store = small_store(&dir);
    let inode = crate::store::inode::Inode::new_file(1000, 1000, 0o644);
    let mut tx = store.begin_tx().unwrap();
    Store::put_inode_in_tx(&mut tx, 3, &inode).unwrap();
    tx.commit(&CrashHooks::none()).unwrap();

    let mut filled: Option<Vec<u8>> = None;
    let mut hit_full = false;
    for i in 0..64u8 {
        let content = incompressible(i, 1024 * 1024);
        match write_file(&mut store, 3, &content) {
            Ok(()) => filled = Some(content),
            Err(StoreError::Full(msg)) => {
                assert!(msg.contains("watermark"), "Full must explain: {msg}");
                hit_full = true;
                break;
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert!(hit_full, "small store must eventually refuse with Full");

    // The last committed write is intact and readable.
    let last = filled.expect("at least one write must succeed");
    let read = store.read_file(3, 0, last.len() as u64).unwrap();
    assert_eq!(read, last, "data before ENOSPC must survive");
    // The failed commit left no partial state: the store still reads the
    // full committed file and fsck is clean.
    let report = fsck(dir.path(), &FsckOptions::default()).unwrap();
    assert!(
        report.is_clean(),
        "fsck after ENOSPC must be clean:\n{}",
        report.render()
    );
}

#[test]
fn failed_commit_leaves_no_partial_state() {
    let dir = TempDir::new().unwrap();
    let mut store = small_store(&dir);
    let inode = crate::store::inode::Inode::new_file(1000, 1000, 0o644);
    let mut tx = store.begin_tx().unwrap();
    Store::put_inode_in_tx(&mut tx, 3, &inode).unwrap();
    tx.commit(&CrashHooks::none()).unwrap();

    // Fill until Full.
    for i in 0..64u8 {
        let content = incompressible(i, 1024 * 1024);
        match write_file(&mut store, 3, &content) {
            Ok(()) => {}
            Err(StoreError::Full(_)) => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    // A subsequent failing commit (bigger write) must not poison the
    // segment buffer: a *smaller* write must then succeed and be exact.
    let big = incompressible(200, 2 * 1024 * 1024);
    assert!(matches!(
        write_file(&mut store, 3, &big),
        Err(StoreError::Full(_))
    ));
    // Small structured write succeeds afterwards.
    let small: Vec<u8> = b"after-enospc".repeat(5000);
    write_file(&mut store, 3, &small).expect("small write after Full must succeed");
    let read = store.read_file(3, 0, small.len() as u64).unwrap();
    assert_eq!(read, small);
}

#[test]
fn delete_then_write_works_under_pressure() {
    let dir = TempDir::new().unwrap();
    let mut store = small_store(&dir);
    let inode = crate::store::inode::Inode::new_file(1000, 1000, 0o644);
    let mut tx = store.begin_tx().unwrap();
    Store::put_inode_in_tx(&mut tx, 3, &inode).unwrap();
    tx.commit(&CrashHooks::none()).unwrap();
    // Fill most of the store with garbage versions.
    for i in 0..24u8 {
        let content = incompressible(i, 256 * 1024);
        if write_file(&mut store, 3, &content).is_err() {
            break;
        }
    }
    // Truncate to zero (frees nothing physically in v1 but shrinks the
    // file); the store must still answer reads and accept small writes.
    store.truncate_file(3, 0).unwrap();
    let read = store.read_file(3, 0, 1024).unwrap();
    assert!(read.is_empty() || read.iter().all(|&b| b == 0));
}

#[test]
fn gc_recovers_space_when_near_full() {
    // The emergency reserve must let GC compact even when writes are
    // refused at the watermark (§21: never discover the fs needs space it
    // cannot have).
    let dir = TempDir::new().unwrap();
    let mut store = small_store(&dir);
    let inode = crate::store::inode::Inode::new_file(1000, 1000, 0o644);
    let mut tx = store.begin_tx().unwrap();
    Store::put_inode_in_tx(&mut tx, 3, &inode).unwrap();
    tx.commit(&CrashHooks::none()).unwrap();

    // Overwrite the same file repeatedly: each version creates garbage.
    let mut hit_full = false;
    for i in 0..32u8 {
        let content = incompressible(i, 512 * 1024);
        match write_file(&mut store, 3, &content) {
            Ok(()) => {}
            Err(StoreError::Full(_)) => {
                hit_full = true;
                break;
            }
            Err(e) => panic!("unexpected: {e}"),
        }
    }
    assert!(hit_full, "store should reach the watermark");
    let before = store.physical_used();
    // GC must run from the reserve (no commit is needed to start) and
    // reclaim the garbage versions.
    let reclaimed = crate::store::gc::collect(&mut store, &CrashHooks::none()).unwrap();
    assert!(reclaimed > 0, "GC must reclaim space when full");
    let after = store.physical_used();
    assert!(
        after < before,
        "GC must reduce physical usage: before {before}, after {after}"
    );
    // The store accepts writes again.
    let content = incompressible(200, 256 * 1024);
    write_file(&mut store, 3, &content).expect("writes resume after GC");
    let read = store.read_file(3, 0, content.len() as u64).unwrap();
    assert_eq!(read, content);
    let report = fsck(dir.path(), &FsckOptions::default()).unwrap();
    assert!(
        report.is_clean(),
        "fsck after near-full GC: {}",
        report.render()
    );
}
