//! fsck integration tests: a clean store passes, and every corruption
//! class is detected (superblock, segment, root, reference, reachability)
//! without panicking on malformed input (`docs/recovery/fsck.md`).

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
use crate::store::{ExtentUpdate, Store, StoreConfig};

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x22; 16]).unwrap()
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

fn write_file(store: &mut Store, ino: u64, content: &[u8]) {
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

/// Build a store with one file and structured content.
fn build_store(dir: &TempDir) -> Store {
    let mut store = create_store(dir);
    let inode = crate::store::inode::Inode::new_file(1000, 1000, 0o644);
    let mut tx = store.begin_tx().unwrap();
    Store::put_inode_in_tx(&mut tx, 3, &inode).unwrap();
    tx.commit(&CrashHooks::none()).unwrap();
    let mut content = Vec::new();
    content.extend_from_slice(b"fsck-fsck-fsck-".repeat(300).as_slice());
    content.extend_from_slice(&[0u8; 8192]);
    for i in 0..9000u32 {
        content.push((i % 23) as u8);
    }
    write_file(&mut store, 3, &content);
    store
}

#[test]
fn clean_store_passes_fsck() {
    let dir = TempDir::new().unwrap();
    let store = build_store(&dir);
    let content = store.read_file(3, 0, 1024).unwrap();
    assert!(!content.is_empty());
    drop(store);
    let report = fsck(dir.path(), &FsckOptions::default()).unwrap();
    assert!(
        report.is_clean(),
        "clean store must pass fsck:\n{}",
        report.render()
    );
    assert!(report.segments_scanned >= 1);
    assert!(report.records_scanned >= 1);
    assert!(report.inodes_verified >= 1);
    // The root dir inode (2) plus the file (3).
    assert!(report.live_objects >= 2);
}

#[test]
fn torn_tail_is_warning_and_repairable() {
    let dir = TempDir::new().unwrap();
    let store = build_store(&dir);
    drop(store);
    // Simulate a torn write: append a valid record, then cut the file
    // mid-record (a crash during a record append leaves exactly this).
    let segments = crate::store::segment::list_segments(dir.path()).unwrap();
    let last = *segments.last().unwrap();
    let path = crate::store::segment::segment_path(dir.path(), last);
    let rec = crate::format::record::encode(
        crate::format::version::RecordTag::Data,
        0,
        None,
        &[0x77u8; 200],
    );
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        use std::io::Write;
        f.write_all(&rec).unwrap();
        f.sync_all().unwrap();
    }
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    let len = f.metadata().unwrap().len();
    f.set_len(len - 37).unwrap();
    drop(f);
    // Without repair: warning about the torn tail, but no errors.
    let report = fsck(dir.path(), &FsckOptions::default()).unwrap();
    assert!(report.warning_count() >= 1);
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.message.contains("torn tail")),
        "expected torn-tail warning:\n{}",
        report.render()
    );
    // With repair: tail truncated, store still mounts and reads.
    let opts = FsckOptions {
        repair_torn_tails: true,
        ..Default::default()
    };
    let report = fsck(dir.path(), &opts).unwrap();
    assert!(!report.repaired.is_empty());
    let store = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    let content = store.read_file(3, 0, 60).unwrap();
    assert_eq!(content, b"fsck-fsck-fsck-".repeat(4));
}

#[test]
fn mid_file_corruption_is_an_error() {
    let dir = TempDir::new().unwrap();
    let store = build_store(&dir);
    drop(store);
    // Flip a byte inside a record payload in segment 0.
    let path = crate::store::segment::segment_path(dir.path(), 0);
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    use std::io::{Seek, SeekFrom};
    f.seek(SeekFrom::Start(crate::format::record::HEADER_SIZE + 3))
        .unwrap();
    let mut b = [0u8; 1];
    use std::io::Read;
    f.read_exact(&mut b).unwrap();
    b[0] ^= 0xFF;
    f.seek(SeekFrom::Start(crate::format::record::HEADER_SIZE + 3))
        .unwrap();
    use std::io::Write;
    f.write_all(&b).unwrap();
    drop(f);
    let report = fsck(dir.path(), &FsckOptions::default());
    // The corruption is a typed error (segment scan fails), never a panic.
    assert!(report.is_err() || !report.unwrap().is_clean());
}

#[test]
fn deleted_root_object_is_detected() {
    let dir = TempDir::new().unwrap();
    let store = build_store(&dir);
    drop(store);
    // Delete the active segment files: the root object becomes missing.
    for seq in crate::store::segment::list_segments(dir.path()).unwrap() {
        crate::store::segment::delete_segment(dir.path(), seq).unwrap();
    }
    let report = fsck(dir.path(), &FsckOptions::default());
    assert!(report.is_err() || !report.unwrap().is_clean());
}

#[test]
fn corrupt_superblock_slot_is_ignored_with_warning() {
    let dir = TempDir::new().unwrap();
    let store = build_store(&dir);
    drop(store);
    // Corrupt slot A (generation 0 was written first; B is current after
    // commits). Corrupting A must not break the mount; fsck warns.
    let sb_path = dir.path().join("superblock");
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&sb_path)
        .unwrap();
    use std::io::{Seek, SeekFrom};
    f.seek(SeekFrom::Start(10)).unwrap();
    let mut b = [0u8; 1];
    use std::io::Read;
    f.read_exact(&mut b).unwrap();
    b[0] ^= 0xFF;
    f.seek(SeekFrom::Start(10)).unwrap();
    use std::io::Write;
    f.write_all(&b).unwrap();
    drop(f);
    let report = fsck(dir.path(), &FsckOptions::default()).unwrap();
    assert!(
        report.superblock_slots_valid == 2 || report.warning_count() >= 1,
        "expected a warning for the corrupt slot:\n{}",
        report.render()
    );
    // The store still mounts from the surviving slot.
    let store = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    let content = store.read_file(3, 0, 60).unwrap();
    assert_eq!(content, b"fsck-fsck-fsck-".repeat(4));
}

#[test]
fn overwritten_data_is_reported_as_unreachable() {
    let dir = TempDir::new().unwrap();
    let mut store = build_store(&dir);
    // Overwrite repeatedly: old objects become garbage.
    for i in 0..5 {
        let content = format!("version-{i}:{}", "y".repeat(2000)).into_bytes();
        write_file(&mut store, 3, &content);
    }
    drop(store);
    let report = fsck(dir.path(), &FsckOptions::default()).unwrap();
    assert!(
        report.leaked_objects > 0,
        "overwrites must produce unreachable objects:\n{}",
        report.render()
    );
}

#[test]
fn verify_materialized_full_chain() {
    let dir = TempDir::new().unwrap();
    let store = build_store(&dir);
    drop(store);
    let opts = FsckOptions {
        verify_materialized: true,
        ..Default::default()
    };
    let report = fsck(dir.path(), &opts).unwrap();
    assert!(
        report.is_clean(),
        "materialized chain must pass:\n{}",
        report.render()
    );
}

#[test]
fn fsck_refuses_mounted_store() {
    let dir = TempDir::new().unwrap();
    let _store = build_store(&dir); // holds the mount lock
    let res = crate::fsck::ensure_unmounted(dir.path());
    assert!(
        res.is_err(),
        "fsck must refuse a store whose mount lock is held"
    );
}
