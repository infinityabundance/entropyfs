//! Persistent-store integration tests (Phase 2): mkfs, write/read
//! round trips, remount, the crash-court matrix at every durability
//! boundary, GC, and truncate (`docs/recovery/crash-consistency.md`).

#![forbid(unsafe_code)]

use tempfile::TempDir;

use crate::core::candidate::{pick_cheapest, raw_candidate, zero_candidate};
use crate::core::extent::ChunkId;
use crate::core::representation::{RansCodec, Representation};
use crate::entropy::palette::PaletteEncoder;
use crate::entropy::periodic::PeriodicEncoder;
use crate::entropy::sparse::SparseEncoder;
use crate::rans::residual::RansEncoder;
use crate::store::inode::Inode;
use crate::store::transaction::{CrashHooks, CrashPoint};
use crate::store::{ExtentUpdate, Store, StoreConfig};

/// All crash-court injection points (except the GC-only boundary).
fn all_crash_points() -> Vec<CrashPoint> {
    vec![
        CrashPoint::AfterRootWrite,
        CrashPoint::AfterRecordAppend,
        CrashPoint::AfterSegmentFdatasync,
        CrashPoint::AfterSegmentDirFsync,
        CrashPoint::AfterSuperblockWrite,
        CrashPoint::AfterSuperblockFsync,
    ]
}

/// Create a fresh store in the tempdir.
fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x11; 16]).unwrap()
}

/// Create a regular file inode and register it in the index.
fn create_file(store: &mut Store, ino: u64) -> Inode {
    let inode = Inode::new_file(1000, 1000, 0o644);
    let mut tx = store.begin_tx().unwrap();
    Store::put_inode_in_tx(&mut tx, ino, &inode).unwrap();
    tx.commit(&CrashHooks::none()).unwrap();
    inode
}

/// Encode `content` into chunk extents using the engine's candidate
/// families (zero/raw + sparse/palette/periodic/rans where they apply).
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

/// Write `content` to file `ino` through the real commit path.
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

/// The set of (start, descriptor) extent entries for `ino`.
fn extents(store: &Store, ino: u64) -> Vec<(u64, crate::core::representation::Representation)> {
    let limits = store.limits();
    let inode = store.get_inode(ino).unwrap().unwrap();
    let root = match inode.data {
        crate::store::inode::InodeData::File { extent_root } => extent_root,
        _ => unreachable!(),
    };
    crate::store::extent_tree::scan_all(root, 64, limits.max_fanout, store)
        .unwrap()
        .into_iter()
        .map(|(start, bytes)| {
            let d = crate::format::descriptor::decode(
                &bytes,
                limits.max_descriptor_bytes,
                limits.max_inline_bytes,
                limits.max_palette,
                limits.max_period,
                limits.max_chunk_size,
            )
            .unwrap();
            (start, d)
        })
        .collect()
}

#[test]
fn partial_tail_chunk_never_exceeds_eof() {
    // The fsck invariant: every extent's end must be <= the file size.
    // Writing a small file through write_region (the FUSE write path)
    // must encode the trailing chunk at its logical length, not pad it
    // to the full chunk class.
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    create_file(&mut store, 3);
    store.write_region(3, 0, b"hello world").unwrap();
    let size = store.get_inode(3).unwrap().unwrap().size;
    assert_eq!(size, 11);
    let exts = extents(&store, 3);
    assert_eq!(exts.len(), 1);
    assert_eq!(exts[0].0, 0);
    assert_eq!(exts[0].1.len(), 11);
    assert_eq!(exts[0].1.len(), size);

    // Extend in place: the extent grows but stays <= size.
    store.write_region(3, 5, b" and beyond").unwrap();
    let size = store.get_inode(3).unwrap().unwrap().size;
    assert_eq!(size, 16);
    let exts = extents(&store, 3);
    assert_eq!(exts.len(), 1);
    assert_eq!(exts[0].1.len(), size);

    // Write a full mid-file chunk plus a short tail.
    store
        .write_region(3, 0, vec![0x5A; 65536].as_slice())
        .unwrap();
    store.write_region(3, 65536, b"tail").unwrap();
    let size = store.get_inode(3).unwrap().unwrap().size;
    assert_eq!(size, 65540);
    let exts = extents(&store, 3);
    assert_eq!(exts.len(), 2);
    assert!(exts.iter().all(|(s, d)| *s + d.len() <= size));
    // The tail extent is exactly 4 bytes.
    let tail = exts.iter().find(|(s, _)| *s == 65536).unwrap();
    assert_eq!(tail.1.len(), 4);

    // Byte-exact read-back.
    let read = store.read_file(3, 0, size).unwrap();
    let mut expect = vec![0x5Au8; 65536];
    expect.extend_from_slice(b"tail");
    assert_eq!(read, expect);
}

#[test]
fn create_commit_remount_roundtrip() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    create_file(&mut store, 3);
    // Structured content: a repeating pattern (periodic) + a zero run +
    // low-cardinality region, plus a rANS-friendly region.
    let mut content = Vec::new();
    content.extend_from_slice(b"entropyfs-entropyfs-entropyfs-".repeat(50).as_slice()); // periodic
    content.extend_from_slice(&[0u8; 4096]); // zero
    content.extend_from_slice(&[0xAB; 2048]); // fill
    for i in 0..8192 {
        content.push((i % 17) as u8); // rANS-friendly
    }
    write_file(&mut store, 3, &content);

    // Read back in the live store.
    let read = store.read_file(3, 0, content.len() as u64).unwrap();
    assert_eq!(read, content);

    // Remount.
    drop(store);
    let mut store2 = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    let read2 = store2.read_file(3, 0, content.len() as u64).unwrap();
    assert_eq!(read2, content);
    // The store is still writable after remount.
    write_file(&mut store2, 3, b"post-remount write".repeat(100).as_slice());
    let read3 = store2
        .read_file(3, 0, (b"post-remount write".len() * 100) as u64)
        .unwrap();
    assert_eq!(read3, b"post-remount write".repeat(100));
}

#[test]
fn sparse_file_holes_read_as_zeros() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    create_file(&mut store, 3);
    // Write only at offset 65536 (a hole before it).
    let content = b"data-at-64k".to_vec();
    let updates = encode_chunks(&content, &store);
    let mut shifted = updates;
    for u in &mut shifted {
        u.offset += 65536;
    }
    store
        .commit_file_extents(
            3,
            shifted,
            Some(65536 + content.len() as u64),
            &CrashHooks::none(),
        )
        .unwrap();
    // Read the whole range: the hole must be zeros.
    let read = store.read_file(3, 0, 65536 + content.len() as u64).unwrap();
    assert_eq!(read.len(), 65536 + content.len());
    eprintln!(
        "SPARSE-DEBUG len={} nonzero_prefix={}",
        read.len(),
        read[..read.len().min(65536)]
            .iter()
            .filter(|&&b| b != 0)
            .count()
    );
    eprintln!(
        "SPARSE-DEBUG first_nonzero={:?}",
        read.iter().position(|&b| b != 0)
    );
    assert_eq!(read.len(), 65536 + content.len());
    assert!(read[..65536].iter().all(|&b| b == 0));
    assert_eq!(&read[65536..], &content[..]);
}

#[test]
fn crash_matrix_at_every_durability_boundary() {
    for point in all_crash_points() {
        let dir = TempDir::new().unwrap();
        let mut store = create_store(&dir);
        create_file(&mut store, 3);
        let pre: Vec<u8> = b"pre-crash-state".repeat(200);
        write_file(&mut store, 3, &pre);
        let pre_len = pre.len() as u64;

        // Attempt a second commit with the crash armed.
        let post: Vec<u8> = b"post-crash-state-DIFFERENT".repeat(200);
        let updates = encode_chunks(&post, &store);
        let hooks = CrashHooks::crash_at(point);
        let res = store.commit_file_extents(3, updates, Some(post.len() as u64), &hooks);
        match point {
            CrashPoint::AfterSuperblockFsync => {
                // The commit IS durable (superblock fsynced) before the
                // error is reported: the post-state must be visible.
                assert!(res.is_err(), "crash point {point:?} must report");
                drop(store); // release the mount lock before reopening
                let store2 = Store::open(dir.path(), &StoreConfig::default()).unwrap();
                let after = store2.read_file(3, 0, post.len() as u64).unwrap();
                assert_eq!(after, post, "point {point:?} should expose the post-state");
            }
            _ => {
                assert!(res.is_err(), "crash point {point:?} must report");
                // The store must reopen and expose exactly the pre-state or
                // the post-state — never a hybrid.
                drop(store); // release the mount lock before reopening
                let store2 = Store::open(dir.path(), &StoreConfig::default()).unwrap();
                let after = store2
                    .read_file(3, 0, pre_len.max(post.len() as u64))
                    .unwrap();
                let pre_ok = after == pre[..after.len().min(pre.len())] && after.len() == pre.len();
                let post_ok =
                    after == post[..after.len().min(post.len())] && after.len() == post.len();
                assert!(
                    pre_ok || post_ok,
                    "point {point:?}: hybrid or corrupt state (len {} pre {} post {})",
                    after.len(),
                    pre.len(),
                    post.len()
                );
                // The recovered store must remain writable.
                let mut store2 = store2;
                write_file(&mut store2, 3, b"post-recovery write".repeat(50).as_slice());
            }
        }
    }
}

#[test]
fn gc_reclaims_and_preserves_live_data() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    create_file(&mut store, 3);
    // Many overwrites create garbage records.
    for i in 0..12 {
        let content = format!("version-{i}:{}", "x".repeat(1000)).into_bytes();
        write_file(&mut store, 3, &content);
    }
    let final_content = format!("version-{}:{}", 12, "x".repeat(1000)).into_bytes();
    write_file(&mut store, 3, &final_content);
    let read = store.read_file(3, 0, final_content.len() as u64).unwrap();
    assert_eq!(read, final_content);

    let before_reclaim = crate::store::gc::unreachable_bytes(&store).unwrap();
    assert!(before_reclaim > 0, "overwrites must create garbage");
    let reclaimed = crate::store::gc::collect(&mut store, &CrashHooks::none()).unwrap();
    assert!(reclaimed > 0);
    let after_reclaim = crate::store::gc::unreachable_bytes(&store).unwrap();
    assert!(after_reclaim < before_reclaim);

    // Live data intact and the store remounts.
    let read2 = store.read_file(3, 0, final_content.len() as u64).unwrap();
    assert_eq!(read2, final_content);
    drop(store);
    let store2 = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    let read3 = store2.read_file(3, 0, final_content.len() as u64).unwrap();
    assert_eq!(read3, final_content);
}

#[test]
fn truncate_and_rewrite() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    create_file(&mut store, 3);
    let content: Vec<u8> = (0..30000u32).map(|i| (i % 61) as u8).collect();
    write_file(&mut store, 3, &content);
    store.truncate_file(3, 1000).unwrap();
    let read = store.read_file(3, 0, 5000).unwrap();
    assert_eq!(read.len(), 1000);
    assert_eq!(read, &content[..1000]);
    // Extend again: the new region is a hole.
    write_file(&mut store, 3, b"extension".repeat(200).as_slice());
    let read2 = store.read_file(3, 0, 5000).unwrap();
    assert_eq!(read2, b"extension".repeat(200));
}

#[test]
fn rans_extent_survives_remount() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    create_file(&mut store, 3);
    // 61-symbol near-uniform stream (5.93 bits/symbol): rANS beats RAW
    // decisively, but the i/4096 drift breaks exact periodicity so the
    // PERIODIC family cannot win, and 61 > 16 symbols excludes PALETTE.
    let content: Vec<u8> = (0..65536u32)
        .map(|i| ((((i * 3) % 61) + (i / 4096)) % 61) as u8)
        .collect();
    write_file(&mut store, 3, &content);
    // The chunk must have been encoded as RANS (structured enough).
    let inode = store.get_inode(3).unwrap().unwrap();
    if let crate::store::inode::InodeData::File { extent_root } = &inode.data {
        let all = crate::store::extent_tree::scan_all(
            *extent_root,
            crate::store::BTREE_ORDER,
            store.limits().max_fanout,
            &store,
        )
        .unwrap();
        assert!(!all.is_empty());
        let desc = crate::format::descriptor::decode(
            &all[0].1,
            store.limits().max_descriptor_bytes,
            store.limits().max_inline_bytes,
            store.limits().max_palette,
            store.limits().max_period,
            store.limits().max_chunk_size,
        )
        .unwrap();
        assert!(
            matches!(desc, Representation::Rans { .. }),
            "expected RANS, got {desc:?}"
        );
    } else {
        panic!("ino 3 not a file");
    }
    // Remount and re-read.
    let read = store.read_file(3, 0, content.len() as u64).unwrap();
    assert_eq!(read, content);
    drop(store);
    let store2 = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    let read2 = store2.read_file(3, 0, content.len() as u64).unwrap();
    assert_eq!(read2, content);
}

#[test]
fn uuid_and_features_persist() {
    let dir = TempDir::new().unwrap();
    let mut store = create_store(&dir);
    create_file(&mut store, 3);
    let content: Vec<u8> = (0..65536u32)
        .map(|i| ((i * 7 + i / 32) % 211) as u8)
        .collect();
    write_file(&mut store, 3, &content);
    drop(store);
    let store2 = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    assert_eq!(store2.current_root().uuid, [0x11; 16]);
    // The store reports a nonzero physical capacity.
    assert!(store2.physical_capacity() > 0);
    let _ = RansCodec::Interleaved2; // keep import referenced
}
