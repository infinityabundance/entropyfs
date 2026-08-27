//! SequenceDict store-level integration (Phase-9B): the cross-chunk
//! dictionary family through the real write path.
//!
//! The dictionary is the previous same-file chunk. These tests prove:
//! - a dictionary-correlated second chunk wins as SEQUENCE_DICT through
//!   the single-write path (dictionary from the committed store);
//! - in-batch dictionary chaining (chunk N using N−1 written in the same
//!   group commit) respects `max_reference_depth`, with terminal anchors
//!   emerging automatically at the depth cap;
//! - GC retains the dictionary chunk (via the reference closure) after the
//!   source chunk is overwritten, so reads stay byte-exact;
//! - the background optimizer can rebase a RAW chunk to SEQUENCE_DICT;
//! - fsck stays clean with SEQUENCE_DICT descriptors present.

#![forbid(unsafe_code)]

use crate::core::candidate::Encoder;
use crate::core::representation::Representation;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x81; 16]).unwrap()
}

fn create_file(store: &Store) -> u64 {
    store
        .create_entry(
            1,
            b"f",
            NewEntry::file(0o644, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap()
}

/// Deterministic byte-uniform noise (SplitMix64): no exploitable local
/// structure, so the LZ floor cannot compress it — only the dictionary can.
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

/// The chunk at `offset` of `ino` (descriptor decode + family).
fn extent_family(store: &Store, ino: u64, offset: u64, len: u64) -> (String, Representation) {
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
    let desc = crate::format::descriptor::decode(&bytes, limits).unwrap();
    assert_eq!(
        desc.len(),
        len,
        "extent at {offset} must cover exactly {len}"
    );
    (desc.family().to_string(), desc)
}

/// A dictionary-correlated variant: `base` with `n` scattered single-byte
/// edits (deterministic positions). The result is noise with sparse XOR
/// differences — locally incompressible, dictionary-compressible. The
/// position is drawn from the MIXED state so different salts yield
/// different positions (and thus different chunks).
fn edited(base: &[u8], n: usize, salt: u64) -> Vec<u8> {
    let mut out = base.to_vec();
    let mut x = 0x0ddc_0ffe_e15e_5eedu64.wrapping_add(salt);
    let mut placed = 0usize;
    while placed < n {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let pos = ((z >> 32) as usize) % out.len();
        if out[pos] != 0 {
            out[pos] ^= ((placed % 251) as u8) + 1;
            placed += 1;
        }
    }
    out
}

#[test]
fn dict_correlated_second_chunk_wins_sequencedict() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let ino = create_file(&store);
    // Chunk 0: noise (stored RAW — no local structure). Chunk 1: the same
    // noise with light edits — locally incompressible, but nearly all of
    // it is DICT-copyable from chunk 0.
    let chunk0 = noise(65536, 0x1111_2222);
    let chunk1 = edited(&chunk0, 500, 1);
    store.write_region(ino, 0, &chunk0).unwrap();
    store.write_region(ino, 65536, &chunk1).unwrap();

    let (f0, _d0) = extent_family(&store, ino, 0, 65536);
    assert_eq!(f0, "RAW", "noise chunk must stay RAW, got {f0}");
    let (f1, d1) = extent_family(&store, ino, 65536, 65536);
    assert_eq!(
        f1, "SEQUENCE_DICT",
        "dict-correlated chunk must win as SEQUENCE_DICT, got {f1}"
    );
    // The descriptor's dictionary must be chunk 0's content id.
    if let Representation::SequenceDict { dictionary, .. } = d1 {
        assert_eq!(dictionary, crate::core::extent::ChunkId::of(&chunk0));
    } else {
        panic!("expected SequenceDict");
    }
    // Depth must be exactly 1 (chunk 0 is terminal).
    let depth = crate::optimizer::rebase::chain_depth(&store, &d1);
    assert_eq!(depth, 1);
    // Read-back byte-exact, and fsck clean.
    let back = store.read_file(ino, 0, 131072).unwrap();
    assert_eq!(&back[..65536], &chunk0[..]);
    assert_eq!(&back[65536..], &chunk1[..]);
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn in_batch_dictionary_chaining_respects_depth_cap() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let ino = create_file(&store);
    // Six 64 KiB chunks: chunk 0 is noise; chunks 1..5 are edited variants
    // of chunk 0 (unique, dict-correlated). Written as ONE group-commit
    // batch so each chunk can use the previous (uncommitted) chunk as its
    // in-batch dictionary.
    let chunk_class = 65536usize;
    let chunk0 = noise(chunk_class, 0x3333_4444);
    let mut data = Vec::new();
    data.extend_from_slice(&chunk0);
    for i in 1..6 {
        data.extend_from_slice(&edited(&chunk0, 500, i as u64));
    }
    store
        .write_region_batch(ino, &[(0, data.clone())], Default::default())
        .unwrap();

    // Every chunk must round-trip byte-exactly.
    let back = store.read_file(ino, 0, data.len() as u64).unwrap();
    assert_eq!(back, data);

    // Chunk 1 uses the in-batch chunk 0 as its dictionary (depth 1); the
    // chain grows one per chunk until the depth cap forces an anchor.
    let limits = store.limits();
    let max_depth = limits.max_reference_depth as usize;
    let mut chain: Vec<(String, usize)> = Vec::new();
    for i in 0..6 {
        let (f, d) = extent_family(&store, ino, (i * chunk_class) as u64, chunk_class as u64);
        let depth = crate::optimizer::rebase::chain_depth(&store, &d) as usize;
        chain.push((f, depth));
        assert!(
            depth <= max_depth,
            "chunk {i}: chain depth {depth} exceeds the cap {max_depth}"
        );
    }
    // 0 RAW (anchor), 1..4 SequenceDict chains, 5 forced anchor again.
    assert_eq!(chain[0].0, "RAW");
    for (i, (family, depth)) in chain.iter().enumerate().skip(1).take(4) {
        assert_eq!(family, "SEQUENCE_DICT", "chunk {i} must chain");
        assert_eq!(*depth, i, "chunk {i} depth");
    }
    assert_eq!(
        chain[5].0, "RAW",
        "chunk 5 must re-anchor at the depth cap (got {})",
        chain[5].0
    );
    assert_eq!(chain[5].1, 0);

    // A later in-batch chunk can start a fresh chain off the anchor: write
    // one more chunk (6) that references the anchor (chunk 5, depth 0).
    let anchor_bytes = store
        .read_file(ino, 5 * chunk_class as u64, chunk_class as u64)
        .unwrap();
    let mut ch6 = edited(&anchor_bytes, 500, 7);
    ch6.resize(chunk_class, 0);
    store
        .write_region_batch(
            ino,
            &[(6 * chunk_class as u64, ch6.clone())],
            Default::default(),
        )
        .unwrap();
    let (f6, d6) = extent_family(&store, ino, (6 * chunk_class) as u64, chunk_class as u64);
    assert_eq!(f6, "SEQUENCE_DICT", "fresh chain off the anchor, got {f6}");
    assert_eq!(crate::optimizer::rebase::chain_depth(&store, &d6), 1);
    let back = store
        .read_file(ino, 6 * chunk_class as u64, chunk_class as u64)
        .unwrap();
    assert_eq!(back, ch6);
}

#[test]
fn gc_retains_dictionary_after_source_overwrite() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let ino = create_file(&store);
    let chunk0 = noise(65536, 0x5555_6666);
    let chunk1 = edited(&chunk0, 500, 3);
    store.write_region(ino, 0, &chunk0).unwrap();
    store.write_region(ino, 65536, &chunk1).unwrap();
    // Overwrite chunk 0 with unrelated garbage: chunk 0's content dies, but
    // chunk 1's SEQUENCE_DICT descriptor still references it — the
    // reference closure must keep the dictionary decodable across GC.
    let garbage = noise(65536, 0x7777_8888);
    store.write_region(ino, 0, &garbage).unwrap();
    let (f0, _) = extent_family(&store, ino, 0, 65536);
    assert_eq!(f0, "RAW");
    crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
    // Chunk 1 must still decode byte-exactly after GC.
    let back = store.read_file(ino, 65536, 65536).unwrap();
    assert_eq!(back, chunk1, "dictionary-dependent chunk after GC");
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn background_optimizer_rebases_raw_to_sequencedict() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let ino = create_file(&store);
    let chunk0 = noise(65536, 0x9999_aaaa);
    let chunk1 = edited(&chunk0, 500, 11);
    // Write with SequenceDict disabled: chunk 1 stays RAW.
    let opts = crate::optimizer::policy::OptimizeOptions {
        allow_sequence_dict: false,
        ..Default::default()
    };
    store.write_region(ino, 0, &chunk0).unwrap();
    store.write_region_with(ino, 65536, &chunk1, opts).unwrap();
    let (f1_before, _) = extent_family(&store, ino, 65536, 65536);
    assert_eq!(f1_before, "RAW", "pre-optimizer chunk 1 must be RAW");
    // Background pass with SequenceDict enabled must rebase chunk 1.
    // Temporal base channels are off so the adjacent-channel machinery
    // cannot rewrite chunk 0 against chunk 1 (the pair is mutually
    // correlated, which would make chunk 1's dictionary a cycle); the test
    // isolates the SequenceDict path.
    let pass_opts = crate::optimizer::policy::OptimizeOptions {
        allow_temporal_bases: false,
        ..Default::default()
    };
    let stats = crate::optimizer::background::optimize_pass(&store, pass_opts, None, None).unwrap();
    assert!(
        stats.rewritten >= 1,
        "background pass must rewrite at least one extent: scanned {} rewritten {} no_gain {} errors {} stale {}",
        stats.scanned,
        stats.rewritten,
        stats.no_gain,
        stats.errors,
        stats.stale_skips
    );
    let (f1_after, _) = extent_family(&store, ino, 65536, 65536);
    assert_eq!(f1_after, "SEQUENCE_DICT", "rebased chunk 1");
    let back = store.read_file(ino, 0, 131072).unwrap();
    assert_eq!(&back[..65536], &chunk0[..]);
    assert_eq!(&back[65536..], &chunk1[..]);
}

#[test]
fn dictionary_depth_accounted_in_costs() {
    // A SequenceDict candidate built on a depth-2 dictionary must carry
    // depth 3 in its cost (λ_depth penalizes deep dictionary chains).
    let limits = crate::core::limits::Limits::default();
    let policy = crate::core::cost::Policy::default();
    let dict = vec![0xABu8; 65536];
    let mut input = dict.clone();
    for i in (0..65536).step_by(31) {
        input[i] ^= 0x05;
    }
    let ctx = crate::core::candidate::CandidateContext {
        limits: &limits,
        policy: &policy,
        content_id: crate::core::extent::ChunkId::of(&input),
        bases: &[],
        dedup: None,
    };
    let enc = crate::rans::sequence::SequenceDictEncoder {
        dictionary: crate::core::extent::ChunkId::of(&dict),
        dict_bytes: dict,
        dict_depth: 2,
    };
    let cands = enc.encode(&input, &ctx);
    assert!(!cands.is_empty());
    assert_eq!(cands[0].cost.depth, 3, "dict_depth 2 + the reference");
}
