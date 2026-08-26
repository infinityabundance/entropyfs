//! SequenceDeep store-level integration (Phase-9E): the deep-match family
//! (repcodes + extended lengths + deep background matcher) through the
//! real store, background optimizer, GC, and fsck.
//!
//! These tests prove:
//! - the background optimizer rewrites long-match/RLE corpora to
//!   SEQUENCE_DEEP, byte-exactly, and the store's feature bits carry bit
//!   14 when such descriptors are present;
//! - remount + fsck stay clean;
//! - GC retains the deep model/enc objects (reachability);
//! - the deep family is background-only: the foreground write path never
//!   emits SEQUENCE_DEEP descriptors.

#![forbid(unsafe_code)]

use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 4 * 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0xde; 16]).unwrap()
}

/// The extent family at `offset` of `ino`.
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
    let d = crate::format::descriptor::decode(&bytes, &limits).unwrap();
    d.family().to_string()
}

/// A 64 KiB chunk dominated by long exact repeats (4 KiB pattern repeated
/// 16×): the fast matcher must emit 131-byte continuations; the deep
/// matcher covers each repeat with one XCOPY.
fn long_repeat_chunk() -> Vec<u8> {
    let pattern: Vec<u8> = (0..4096u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 8) as u8)
        .collect();
    let mut out = Vec::with_capacity(65536);
    while out.len() < 65536 {
        out.extend_from_slice(&pattern);
    }
    out.truncate(65536);
    out
}

/// RLE chunk: one literal + one XCOPY under the deep language.
fn rle_chunk() -> Vec<u8> {
    vec![b'q'; 65536]
}

#[test]
fn background_pass_rewrites_to_deep_and_roundtrips() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let ino = store
        .create_entry(
            1,
            b"f",
            NewEntry::file(0o644, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap();
    let mut data = Vec::new();
    data.extend_from_slice(&long_repeat_chunk());
    data.extend_from_slice(&rle_chunk());
    data.extend_from_slice(&long_repeat_chunk());
    store.write_region(ino, 0, &data).unwrap();
    // The foreground write path must NOT have emitted SEQUENCE_DEEP (it is
    // background-only).
    assert_ne!(extent_family(&store, ino, 0), "SEQUENCE_DEEP");
    let stats = crate::optimizer::background::optimize_pass(
        &store,
        crate::optimizer::policy::OptimizeOptions::default(),
        None,
        None,
    )
    .unwrap();
    assert!(
        stats.rewritten >= 1,
        "background pass must rewrite the long-match corpus (got {stats:?})"
    );
    let mut deep_count = 0usize;
    for off in [0u64, 65536, 131072] {
        if extent_family(&store, ino, off) == "SEQUENCE_DEEP" {
            deep_count += 1;
        }
    }
    assert!(
        deep_count >= 1,
        "expected at least one SEQUENCE_DEEP extent (got {deep_count})"
    );
    // Byte-exact read-back.
    assert_eq!(store.read_file(ino, 0, data.len() as u64).unwrap(), data);
    // GC + fsck stay clean.
    crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
    assert_eq!(store.read_file(ino, 0, data.len() as u64).unwrap(), data);
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn deep_survives_remount_with_feature_bit() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let ino = store
        .create_entry(
            1,
            b"f",
            NewEntry::file(0o644, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap();
    let data = long_repeat_chunk();
    store.write_region(ino, 0, &data).unwrap();
    crate::optimizer::background::optimize_pass(
        &store,
        crate::optimizer::policy::OptimizeOptions::default(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(extent_family(&store, ino, 0), "SEQUENCE_DEEP");
    // The superblock feature bits must carry the SEQUENCE_DEEP incompat
    // bit (bit 14 ⇒ mask 1 << 13).
    let feats = store.features_in_use();
    assert!(
        feats & crate::format::features::Feature::SequenceDeep.mask() != 0,
        "feature bit 14 must be set"
    );
    drop(store);
    let store2 = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    assert_eq!(store2.read_file(ino, 0, 65536).unwrap(), data);
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn deep_gate_disables_background_rewrite() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let ino = store
        .create_entry(
            1,
            b"f",
            NewEntry::file(0o644, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap();
    let data = long_repeat_chunk();
    store.write_region(ino, 0, &data).unwrap();
    let opts = crate::optimizer::policy::OptimizeOptions {
        allow_sequence_rans_deep: false,
        ..Default::default()
    };
    let stats = crate::optimizer::background::optimize_pass(&store, opts, None, None).unwrap();
    // With deep off, the long-repeat corpus is still compressed by the
    // fast family (SequenceRans continuations), but never SEQUENCE_DEEP.
    assert_ne!(extent_family(&store, ino, 0), "SEQUENCE_DEEP");
    assert_eq!(store.read_file(ino, 0, 65536).unwrap(), data);
    let _ = stats;
}
