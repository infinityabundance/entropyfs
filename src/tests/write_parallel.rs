//! Phase-10C: parallel chunk preparation regressions.
//!
//! `prepare_write` now composes chunk bytes serially, encodes the chunks
//! CONCURRENTLY (scoped threads; each chunk validates its candidates
//! against a synthetic view of its in-batch dictionary), and applies the
//! batch semantics — in-batch dedup canonicalization, real chain-depth
//! enforcement, pending registration — serially in offset order.
//!
//! These tests pin the invariants the parallel path must never break:
//! - byte-exactness for multi-chunk writes through BOTH the single-call
//!   (`write_region`) and group-commit (`write_region_batch`) paths;
//! - determinism: identical content produces identical descriptors;
//! - the synthetic RAW-reuse hazard: consecutive identical chunks must not
//!   persist a descriptor whose object exists only in the synthetic view
//!   (the phase-3 validation backstop re-encodes against the real pending
//!   state, exactly as the serial search would);
//! - in-batch dictionary chains never exceed `max_reference_depth` even
//!   though the parallel search assumed depth 0.

#![forbid(unsafe_code)]

use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 128 * 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x5c; 16]).unwrap()
}

fn create_file(store: &Store, name: &str) -> u64 {
    store
        .create_entry(
            store.current_root().root_dir_ino,
            name.as_bytes(),
            NewEntry::file(0o644, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap()
}

/// Deterministic byte-uniform noise (SplitMix64).
fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let b = z.to_le_bytes();
        let take = (n - out.len()).min(8);
        out.extend_from_slice(&b[..take]);
    }
    out
}

/// Source-like sequential text: 64 KiB chunks of C-shaped code with
/// per-chunk edits, so each chunk shares long matches with the previous
/// one (the in-batch SequenceDict chain forms and must stay within the
/// depth cap).
fn drift_text(n_chunks: usize) -> Vec<u8> {
    let chunk = 65536usize;
    let mut out = Vec::with_capacity(n_chunks * chunk);
    for c in 0..n_chunks {
        for i in 0..chunk {
            let mut b = b'a' + ((i / 7) % 23) as u8;
            if i % 97 == 0 {
                b = b"fn main() { return 0; }"[i % 23];
            }
            if i == c * 1009 % chunk {
                b = b'X'; // per-chunk unique edit
            }
            out.push(b);
        }
    }
    out
}

/// The raw descriptor bytes of the extent covering `offset`.
fn extent_descriptor(store: &Store, ino: u64, offset: u64) -> Vec<u8> {
    let limits = store.limits();
    let inode = store.get_inode(ino).unwrap().unwrap();
    let root = match inode.data {
        crate::store::inode::InodeData::File { extent_root } => extent_root,
        _ => panic!("not a file"),
    };
    let (_, bytes) = crate::store::extent_tree::covering(
        root,
        offset,
        crate::store::BTREE_ORDER,
        limits.max_fanout,
        store,
    )
    .unwrap()
    .expect("extent covers offset");
    bytes
}

/// The family string of the extent covering `offset`.
fn extent_family(store: &Store, ino: u64, offset: u64) -> String {
    let limits = store.limits();
    let bytes = extent_descriptor(store, ino, offset);
    let desc = crate::format::descriptor::decode(&bytes, limits).unwrap();
    desc.family().to_string()
}

#[test]
fn parallel_multi_chunk_writes_are_byte_exact() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let ino = create_file(&store, "a");

    // 16 chunks (1 MiB) of each corpus through the single-call path: the
    // concurrent encode must reproduce the bytes exactly.
    let corpora: Vec<(String, Vec<u8>)> = vec![
        ("noise".into(), noise(16 * 65536, 0x1a2b_3c4d)),
        ("drift-text".into(), drift_text(16)),
        ("zeros".into(), vec![0u8; 16 * 65536]),
        ("periodic".into(), {
            let mut v = Vec::with_capacity(16 * 65536);
            for i in 0..(16 * 65536) {
                v.push((i % 256) as u8);
            }
            v
        }),
    ];
    for (label, data) in &corpora {
        let f = create_file(&store, label);
        store.write_region(f, 0, data).unwrap();
        let back = store.read_file(f, 0, data.len() as u64).unwrap();
        assert_eq!(&back, data, "{label} single-call byte-exactness");
    }

    // Group-commit path: overlapping/adjacent partial chunks composed via
    // the overlay.
    let data = drift_text(8);
    let mut writes: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut off = 0u64;
    while off < data.len() as u64 {
        let len = ((off * 7) % 30000 + 1) as usize;
        let len = len.min(data.len() - off as usize);
        writes.push((off, data[off as usize..off as usize + len].to_vec()));
        off += len as u64;
    }
    store
        .write_region_batch(ino, &writes, OptimizeOptions::default())
        .unwrap();
    let back = store.read_file(ino, 0, data.len() as u64).unwrap();
    assert_eq!(&back[..], &data[..], "group-commit byte-exactness");

    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn identical_content_encodes_deterministically_across_stores() {
    // Same content, same context (two fresh stores) -> byte-identical
    // extent descriptors: the parallel encode is deterministic.
    let dir1 = TempDir::new().unwrap();
    let dir2 = TempDir::new().unwrap();
    let store1 = create_store(&dir1);
    let store2 = create_store(&dir2);
    let data = drift_text(4);

    let a1 = create_file(&store1, "a");
    store1.write_region(a1, 0, &data).unwrap();
    let a2 = create_file(&store2, "a");
    store2.write_region(a2, 0, &data).unwrap();

    for i in 0..4 {
        let off = (i * 65536) as u64;
        let d1 = extent_descriptor(&store1, a1, off);
        let d2 = extent_descriptor(&store2, a2, off);
        assert_eq!(
            d1, d2,
            "extent {i} descriptor must be deterministic across stores"
        );
    }
    // Read-back both byte-exact.
    assert_eq!(store1.read_file(a1, 0, data.len() as u64).unwrap(), data);
    assert_eq!(store2.read_file(a2, 0, data.len() as u64).unwrap(), data);
}

#[test]
fn committed_content_reused_in_second_file_is_byte_exact() {
    // Same content written twice in ONE store: the second file's extents
    // may legitimately alias the committed content (EXACT_REF / canonical
    // reuse) — the representation differs from the first file's fresh
    // encodes, but every descriptor must resolve and reproduce the bytes.
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let data = drift_text(4);

    let a = create_file(&store, "a");
    store.write_region(a, 0, &data).unwrap();
    let b = create_file(&store, "b");
    store.write_region(b, 0, &data).unwrap();

    // Read-back both byte-exact.
    assert_eq!(store.read_file(a, 0, data.len() as u64).unwrap(), data);
    assert_eq!(store.read_file(b, 0, data.len() as u64).unwrap(), data);

    // Overwriting the same file with identical bytes must be a no-op
    // content-wise: read-back stays exact, fsck clean.
    store.write_region(a, 0, &data).unwrap();
    assert_eq!(store.read_file(a, 0, data.len() as u64).unwrap(), data);
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn consecutive_identical_chunks_in_one_batch() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let ino = create_file(&store, "a");

    // Eight IDENTICAL noise chunks in one batch. The parallel search sees
    // each duplicate's in-batch dictionary (the previous chunk's content,
    // same bytes) through its synthetic view; the phase-3 canonicalization
    // must still reuse the first occurrence, and the persisted descriptor
    // must resolve through the committed store — never through the
    // synthetic view (whose object is never persisted).
    let chunk = noise(65536, 0xfeed_face);
    let mut data = Vec::with_capacity(8 * 65536);
    for _ in 0..8 {
        data.extend_from_slice(&chunk);
    }
    store.write_region(ino, 0, &data).unwrap();
    let back = store.read_file(ino, 0, data.len() as u64).unwrap();
    assert_eq!(back, data, "identical-chunk batch byte-exactness");

    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn duplicate_chunks_after_aliased_first_occurrence() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);

    // File A stores compressible content X (SequenceRans/SequenceDict —
    // NO raw object for X exists in the store).
    let x = drift_text(2);
    let a = create_file(&store, "a");
    store.write_region(a, 0, &x).unwrap();
    assert_ne!(extent_family(&store, a, 0), "RAW", "X must be compressed");

    // File B writes TWO duplicate chunks of X in one batch. Chunk 0 of B
    // aliases X (EXACT_REF — cheaper than the committed descriptor), so it
    // is NOT registered in the batch pending state. Chunk 1 of B must not
    // persist the synthetic RAW descriptor (its object exists only in the
    // synthetic view): the phase-3 validation backstop re-encodes it
    // against the real pending state. Read-back exercises the real
    // descriptor graph and would fail on the synthetic object.
    let b = create_file(&store, "b");
    store.write_region(b, 0, &x).unwrap();
    let back = store.read_file(b, 0, x.len() as u64).unwrap();
    assert_eq!(back, x, "duplicate-of-aliased content byte-exactness");

    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn in_batch_dict_chain_never_exceeds_decode_cap() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let ino = create_file(&store, "a");

    // Six drift chunks in ONE batch: the parallel search assumes each
    // in-batch dictionary is depth 0, so chunk 5's SequenceDict candidate
    // is generated; the phase-3 real-depth check must re-encode it without
    // the dictionary family (exactly what the serial search did when it
    // refused the too-deep chain).
    let data = drift_text(6);
    store.write_region(ino, 0, &data).unwrap();

    let max_depth = store.limits().max_reference_depth;
    let back = store.read_file(ino, 0, data.len() as u64).unwrap();
    assert_eq!(back, data);

    for i in 0..6 {
        let (_, bytes) = {
            let limits = store.limits();
            let inode = store.get_inode(ino).unwrap().unwrap();
            let root = match inode.data {
                crate::store::inode::InodeData::File { extent_root } => extent_root,
                _ => panic!("not a file"),
            };
            crate::store::extent_tree::covering(
                root,
                (i * 65536) as u64,
                crate::store::BTREE_ORDER,
                limits.max_fanout,
                &store,
            )
            .unwrap()
            .expect("extent covers offset")
        };
        let desc = crate::format::descriptor::decode(&bytes, store.limits()).unwrap();
        let depth = crate::optimizer::rebase::chain_depth(&store, &desc);
        assert!(
            depth <= max_depth,
            "chunk {i}: chain depth {depth} exceeds the cap {max_depth}"
        );
    }
    // Chunk 5 must be a terminal re-anchor (RAW or another terminal
    // family), not a 5-deep dictionary chain.
    let family = extent_family(&store, ino, 5 * 65536);
    assert_ne!(
        family, "SEQUENCE_DICT",
        "chunk 5 must re-anchor at the depth cap (got {family})"
    );
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}
