//! Aggressive simulated power-failure tests (§38, `docs/recovery/
//! crash-consistency.md`): kill at every durability boundary — for both
//! regular commits and GC — then verify the recovered store is an
//! admissible pre/post state, passes fsck, and remains writable.

#![forbid(unsafe_code)]

use tempfile::TempDir;

use crate::core::candidate::{pick_cheapest, raw_candidate, zero_candidate};
use crate::core::extent::ChunkId;
use crate::entropy::palette::PaletteEncoder;
use crate::entropy::periodic::PeriodicEncoder;
use crate::entropy::sparse::SparseEncoder;
use crate::fsck::{FsckOptions, fsck};
use crate::rans::residual::RansEncoder;
use crate::store::transaction::{CrashHooks, CrashPoint};
use crate::store::{ExtentUpdate, Store, StoreConfig};

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x33; 16]).unwrap()
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

fn write_file(store: &Store, ino: u64, content: &[u8]) {
    let updates = encode_chunks(content, store);
    store
        .commit_file_extents(
            ino,
            updates,
            Some(content.len() as u64),
            &CrashHooks::none(),
        )
        .unwrap();
}

fn create_file(store: &Store, ino: u64) {
    let inode = crate::store::inode::Inode::new_file(1000, 1000, 0o644);
    let mut tx = store.begin_tx().unwrap();
    Store::put_inode_in_tx(&mut tx, ino, &inode).unwrap();
    tx.commit(&CrashHooks::none()).unwrap();
}

/// Every commit boundary (regular path).
fn commit_crash_points() -> Vec<CrashPoint> {
    vec![
        CrashPoint::AfterRootWrite,
        CrashPoint::AfterRecordAppend,
        CrashPoint::AfterSegmentFdatasync,
        CrashPoint::AfterSegmentDirFsync,
        CrashPoint::AfterSuperblockWrite,
        CrashPoint::AfterSuperblockFsync,
    ]
}

/// Every GC boundary.
fn gc_crash_points() -> Vec<CrashPoint> {
    vec![
        CrashPoint::AfterRootWrite,
        CrashPoint::AfterSuperblockWrite,
        CrashPoint::AfterSuperblockFsync,
        CrashPoint::BeforeOldSegmentDelete,
    ]
}

/// Reopen + fsck + read + rewrite after a crash: the full recovery
/// contract.
fn assert_recovered(dir: &TempDir, expected: &[u8]) -> Store {
    let report = fsck(dir.path(), &FsckOptions::default())
        .unwrap_or_else(|e| panic!("fsck after crash: {e}"));
    assert!(
        report.is_clean(),
        "fsck after crash must be clean:\n{}",
        report.render()
    );
    let store = Store::open(dir.path(), &StoreConfig::default())
        .unwrap_or_else(|e| panic!("reopen after crash: {e}"));
    let read = store
        .read_file(3, 0, expected.len() as u64)
        .unwrap_or_else(|e| panic!("read after crash: {e}"));
    assert_eq!(read, expected, "recovered content must be exact");
    // The recovered store must accept new writes.
    write_file(
        &store,
        3,
        b"post-crash-recovery-write".repeat(64).as_slice(),
    );
    store
}

#[test]
fn commit_crash_matrix_then_fsck_and_rewrite() {
    for point in commit_crash_points() {
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir);
        create_file(&store, 3);
        let pre: Vec<u8> = b"pre-crash-payload".repeat(512);
        write_file(&store, 3, &pre);
        let pre_len = pre.len() as u64;

        let post: Vec<u8> = b"post-crash-payload-DIFFERENT".repeat(512);
        let updates = encode_chunks(&post, &store);
        let res = store.commit_file_extents(
            3,
            updates,
            Some(post.len() as u64),
            &CrashHooks::crash_at(point),
        );
        assert!(res.is_err(), "crash point {point:?} must report");
        drop(store);

        // The recovered state is the pre-state or the post-state — never a
        // hybrid — and must pass fsck and remain writable.
        let store2 = Store::open(dir.path(), &StoreConfig::default())
            .unwrap_or_else(|e| panic!("reopen at {point:?}: {e}"));
        let after = store2
            .read_file(3, 0, pre_len.max(post.len() as u64))
            .unwrap();
        let pre_ok = after == pre;
        let post_ok = after == post;
        assert!(
            pre_ok || post_ok,
            "point {point:?}: hybrid or corrupt state (len {})",
            after.len()
        );
        let report = fsck(dir.path(), &FsckOptions::default()).unwrap();
        assert!(
            report.is_clean(),
            "point {point:?}: fsck must be clean:\n{}",
            report.render()
        );
        write_file(&store2, 3, b"post-recovery".repeat(128).as_slice());
    }
}

#[test]
fn gc_crash_matrix_preserves_live_data() {
    for point in gc_crash_points() {
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir);
        create_file(&store, 3);
        // Generate garbage + a well-defined final state.
        for i in 0..8 {
            let content = format!("gc-version-{i}:{}", "g".repeat(1500)).into_bytes();
            write_file(&store, 3, &content);
        }
        let final_content = format!("gc-version-{}:{}", 8, "g".repeat(1500)).into_bytes();
        write_file(&store, 3, &final_content);
        assert_eq!(
            store.read_file(3, 0, final_content.len() as u64).unwrap(),
            final_content
        );
        // Arm the crash at a GC boundary.
        let res = crate::store::gc::collect(&store, &CrashHooks::crash_at(point));
        assert!(res.is_err(), "GC crash point {point:?} must report");
        drop(store);
        // The live data must survive exactly; fsck clean; writable.
        assert_recovered(&dir, &final_content);
    }
}

#[test]
fn gc_crash_at_delete_still_reclaims_later() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    create_file(&store, 3);
    for i in 0..10 {
        let content = format!("v{i}:{}", "h".repeat(1200)).into_bytes();
        write_file(&store, 3, &content);
    }
    let final_content = format!("v{}:{}", 10, "h".repeat(1200)).into_bytes();
    write_file(&store, 3, &final_content);
    // Crash AFTER the new root is durable but BEFORE old segment deletion.
    let res = crate::store::gc::collect(
        &store,
        &CrashHooks::crash_at(CrashPoint::BeforeOldSegmentDelete),
    );
    assert!(res.is_err());
    drop(store);
    // Reopen, verify content, and run GC to completion: it must reclaim
    // the garbage left by the interrupted run (both old and new).
    let store = assert_recovered(&dir, &final_content);
    let reclaimed = crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
    assert!(reclaimed > 0, "interrupted GC must leave reclaimable data");
    // `assert_recovered` wrote this exact content after recovery; it must
    // still be intact after the completed GC.
    let recovery_content: Vec<u8> = b"post-crash-recovery-write".repeat(64);
    assert_eq!(
        store
            .read_file(3, 0, recovery_content.len() as u64)
            .unwrap(),
        recovery_content
    );
}

#[test]
fn crash_between_commits_is_linearizable() {
    // A sequence of commits with a crash mid-sequence: the store must
    // expose exactly one of the committed prefixes (write-atomicity).
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    create_file(&store, 3);
    let mut committed: Vec<Vec<u8>> = Vec::new();
    for i in 0..6 {
        let content = format!("commit-{i}:{}", "q".repeat(800)).into_bytes();
        write_file(&store, 3, &content);
        committed.push(content.clone());
        assert_eq!(
            store.read_file(3, 0, content.len() as u64).unwrap(),
            content
        );
    }
    // Crash on the 7th commit at the superblock-write boundary.
    let content7 = format!("commit-{}:{}", 6, "q".repeat(800)).into_bytes();
    let updates = encode_chunks(&content7, &store);
    let res = store.commit_file_extents(
        3,
        updates,
        Some(content7.len() as u64),
        &CrashHooks::crash_at(CrashPoint::AfterSuperblockWrite),
    );
    assert!(res.is_err());
    drop(store);
    // Recovered content must be one of the committed prefixes.
    let store2 = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    let read = store2
        .read_file(
            3,
            0,
            committed.last().unwrap().len().max(content7.len()) as u64,
        )
        .unwrap();
    let admissible = committed.contains(&read)
        || read == content7
        || (read.len() == committed.last().unwrap().len() && read == *committed.last().unwrap());
    assert!(admissible, "recovered state is not a committed prefix");
}
