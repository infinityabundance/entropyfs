//! SequenceSharedDict store-level integration (Phase-9C): the shared
//! amortized dictionary family through the real store, optimizer pass, and
//! GC.
//!
//! These tests prove:
//! - the `shared_dict_pass` selects a directory anchor and rewrites
//!   family-correlated chunk-0s to SEQUENCE_SHARED_DICT, byte-exactly;
//! - the pass is idempotent and byte-exact after remount, and fsck stays
//!   clean;
//! - GC pins the shared dictionary chunk through the reference closure, so
//!   reads stay byte-exact even after the anchor file is deleted;
//! - unrelated directories and incompressible files are left alone (the
//!   group-gain gate is real);
//! - rewritten extents carry depth ≤ 1 (terminal anchors only in v1).

#![forbid(unsafe_code)]

use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 4 * 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x9C; 16]).unwrap()
}

fn create_file_under(store: &Store, parent: u64, name: &[u8]) -> u64 {
    store
        .create_entry(
            parent,
            name,
            NewEntry::file(0o644, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap()
}

fn create_dir_under(store: &Store, parent: u64, name: &[u8]) -> u64 {
    store
        .create_entry(
            parent,
            name,
            NewEntry::dir(0o755, 1000, 1000),
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

/// A structured "source-like" chunk: a common header (shared idiom) + a
/// unique body. Many such chunks in one directory share the header, which
/// a shared dictionary captures; the unique bodies prevent exact dedup.
fn family_chunk(seed: u64, header: &[u8]) -> Vec<u8> {
    let mut out = header.to_vec();
    let body_len = 65536usize.saturating_sub(out.len());
    let mut body = noise(body_len, seed);
    for b in &mut body {
        *b = b.wrapping_add((seed >> 32) as u8);
    }
    out.extend_from_slice(&body);
    out.resize(65536, 0);
    out
}

/// A shared header idiom (like a source-file preamble): deterministic,
/// identical across the family, and RANDOM-LOOKING — locally
/// incompressible, so only a cross-file dictionary can capture it (this
/// is precisely the Phase-9C evidence-gate finding: shared structure is
/// invisible to local matching). ~8 KiB; the rest of the 64 KiB chunk
/// carries a unique body so the files never exact-dedup.
fn family_header() -> Vec<u8> {
    noise(8192, 0x5EED_5EED_0FF1_CE99)
}

fn extent_family(store: &Store, ino: u64, offset: u64) -> String {
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
    let d = crate::format::descriptor::decode(
        &bytes,
        limits.max_descriptor_bytes,
        limits.max_inline_bytes,
        limits.max_palette,
        limits.max_period,
        limits.max_chunk_size,
    )
    .unwrap();
    d.family().to_string()
}

#[test]
fn shared_dict_pass_rewrites_family_correlated_chunks() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let d = create_dir_under(&store, 1, b"proj");
    let header = family_header();
    let a = family_chunk(0x1111_2222, &header);
    let b = family_chunk(0x3333_4444, &header);
    let ia = create_file_under(&store, d, b"a.rs");
    let ib = create_file_under(&store, d, b"b.rs");
    store.write_region(ia, 0, &a).unwrap();
    store.write_region(ib, 0, &b).unwrap();

    // The shared-dict pass must select an anchor and rewrite at least one
    // member to SEQUENCE_SHARED_DICT, byte-exactly.
    let stats =
        crate::optimizer::background::shared_dict_pass(&store, OptimizeOptions::default(), None)
            .unwrap();
    assert!(
        stats.rewritten >= 1,
        "expected a shared-dict rewrite, got {stats:?}"
    );
    let fa = extent_family(&store, ia, 0);
    let fb = extent_family(&store, ib, 0);
    let shared_count = [&fa, &fb]
        .iter()
        .filter(|f| f.as_str() == "SEQUENCE_SHARED_DICT")
        .count();
    assert!(
        shared_count >= 1,
        "expected SEQUENCE_SHARED_DICT among {{a,b}}, got {fa} / {fb}"
    );
    // Byte-exact read-back.
    assert_eq!(store.read_file(ia, 0, 65536).unwrap(), a);
    assert_eq!(store.read_file(ib, 0, 65536).unwrap(), b);
    // Rewritten extents are terminal-anchored: depth exactly 1.
    for ino in [ia, ib] {
        let inode = store.get_inode(ino).unwrap().unwrap();
        let root = match inode.data {
            crate::store::inode::InodeData::File { extent_root } => extent_root,
            _ => unreachable!(),
        };
        let (_, bytes) = crate::store::extent_tree::scan_all(
            root,
            crate::store::BTREE_ORDER,
            store.limits().max_fanout,
            &store,
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
        let d = crate::format::descriptor::decode(
            &bytes,
            store.limits().max_descriptor_bytes,
            store.limits().max_inline_bytes,
            store.limits().max_palette,
            store.limits().max_period,
            store.limits().max_chunk_size,
        )
        .unwrap();
        if d.family() == "SEQUENCE_SHARED_DICT" {
            assert_eq!(
                crate::optimizer::rebase::chain_depth(&store, &d),
                1,
                "v1 anchors are terminal: depth must be 1"
            );
        }
    }
    // fsck stays clean.
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn shared_dict_pass_is_idempotent_and_byte_exact_after_remount() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let d = create_dir_under(&store, 1, b"proj");
    let header = family_header();
    let chunks: Vec<Vec<u8>> = (0..4u64)
        .map(|i| family_chunk(0xaaaa_bbbb + i, &header))
        .collect();
    let mut inos = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        let ino = create_file_under(&store, d, format!("f{i}.rs").as_bytes());
        store.write_region(ino, 0, c).unwrap();
        inos.push(ino);
    }
    crate::optimizer::background::shared_dict_pass(&store, OptimizeOptions::default(), None)
        .unwrap();
    let before: Vec<Vec<u8>> = inos
        .iter()
        .map(|&i| store.read_file(i, 0, 65536).unwrap())
        .collect();
    // A second pass must not regress bytes or keep churning.
    let again =
        crate::optimizer::background::shared_dict_pass(&store, OptimizeOptions::default(), None)
            .unwrap();
    assert_eq!(again.rewritten, 0, "second pass must be a no-op");
    // Remount: reopen the store fresh and verify byte-exactness.
    drop(store);
    let store2 = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    for (i, &ino) in inos.iter().enumerate() {
        assert_eq!(store2.read_file(ino, 0, 65536).unwrap(), before[i]);
        assert_eq!(store2.read_file(ino, 0, 65536).unwrap(), chunks[i]);
    }
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn shared_dict_gc_pins_shared_after_owner_delete() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let d = create_dir_under(&store, 1, b"proj");
    let header = family_header();
    let a = family_chunk(0x5555_6666, &header);
    let b = family_chunk(0x7777_8888, &header);
    let ia = create_file_under(&store, d, b"one.rs");
    let ib = create_file_under(&store, d, b"two.rs");
    store.write_region(ia, 0, &a).unwrap();
    store.write_region(ib, 0, &b).unwrap();
    crate::optimizer::background::shared_dict_pass(&store, OptimizeOptions::default(), None)
        .unwrap();
    // Whichever file the pass chose as the anchor, the OTHER file must now
    // reference it (a file cannot use itself as its own dictionary).
    let (anchor_name, reader_ino, reader_bytes) =
        if extent_family(&store, ia, 0) == "SEQUENCE_SHARED_DICT" {
            (b"two.rs".as_slice(), ia, a)
        } else if extent_family(&store, ib, 0) == "SEQUENCE_SHARED_DICT" {
            (b"one.rs".as_slice(), ib, b)
        } else {
            panic!(
                "neither file was rewritten: {} / {}",
                extent_family(&store, ia, 0),
                extent_family(&store, ib, 0)
            );
        };
    // Delete the ANCHOR file: the shared dictionary chunk's descriptor must
    // survive in the chunk index via the GC reference closure, so the
    // reader stays decodable.
    store
        .unlink(d, anchor_name, false, &CrashHooks::none())
        .unwrap();
    crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
    let back = store.read_file(reader_ino, 0, 65536).unwrap();
    assert_eq!(back, reader_bytes, "reader must stay byte-exact");
    // Remount + fsck stay clean.
    drop(store);
    let store2 = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    assert_eq!(
        store2.read_file(reader_ino, 0, 65536).unwrap(),
        reader_bytes
    );
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn shared_dict_skips_unrelated_files_and_urandom() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    // One directory with genuinely unrelated content (no shared idiom):
    // distinct noise chunks must not be rewritten (the group-gain gate).
    let d = create_dir_under(&store, 1, b"noise");
    let mut inos: Vec<(u64, Vec<u8>)> = Vec::new();
    for i in 0..3u64 {
        let ino = create_file_under(&store, d, format!("n{i}.bin").as_bytes());
        let bytes = noise(65536, 0x1234_0000 + i);
        store.write_region(ino, 0, &bytes).unwrap();
        inos.push((ino, bytes));
    }
    let stats =
        crate::optimizer::background::shared_dict_pass(&store, OptimizeOptions::default(), None)
            .unwrap();
    // Unrelated noise: no shared-dict rewrites may happen (fake density
    // negative control).
    for (ino, bytes) in &inos {
        let f = extent_family(&store, *ino, 0);
        assert_ne!(f, "SEQUENCE_SHARED_DICT", "noise must not be rewritten");
        assert_eq!(store.read_file(*ino, 0, 65536).unwrap(), *bytes);
    }
    let _ = stats;
}

#[test]
fn shared_dict_disabled_by_option() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let d = create_dir_under(&store, 1, b"proj");
    let header = family_header();
    let a = family_chunk(0x9999_aaaa, &header);
    let b = family_chunk(0xbbbb_cccc, &header);
    let ia = create_file_under(&store, d, b"a.rs");
    let ib = create_file_under(&store, d, b"b.rs");
    store.write_region(ia, 0, &a).unwrap();
    store.write_region(ib, 0, &b).unwrap();
    let opts = OptimizeOptions {
        allow_shared_dict: false,
        ..Default::default()
    };
    let stats = crate::optimizer::background::shared_dict_pass(&store, opts, None).unwrap();
    assert_eq!(stats.rewritten, 0, "gated pass must not rewrite");
    assert_ne!(extent_family(&store, ia, 0), "SEQUENCE_SHARED_DICT");
    assert_ne!(extent_family(&store, ib, 0), "SEQUENCE_SHARED_DICT");
}
