//! BaseSequence integration: shifted writes through the real write path
//! must round-trip byte-exactly and land on the shift-aware copy/literal
//! delta residual when the base carries unique content (no local repeats
//! for the SequenceRans floor, no positional-XOR win for RansResidual).

#![forbid(unsafe_code)]

use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x61; 16]).unwrap()
}

/// Deterministic byte-uniform noise (SplitMix64): no local matches, so
/// SequenceRans cannot win on it and a positional XOR of shifted copies
/// is ~random too.
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

#[test]
fn inserted_region_roundtrips_and_uses_base_sequence() {
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
    // Version 0: 60 KiB of noise (RAW at rest).
    let v0 = noise(60 * 1024, 0x1111_2222_3333_4444);
    store.write_region(ino, 0, &v0).unwrap();
    // Version 1: 6 KiB of new noise inserted in the middle — everything
    // after shifts by 6 KiB.
    let insert = noise(6 * 1024, 0xaaaa_bbbb_cccc_dddd);
    let mut v1 = Vec::with_capacity(66 * 1024);
    v1.extend_from_slice(&v0[..30 * 1024]);
    v1.extend_from_slice(&insert);
    v1.extend_from_slice(&v0[30 * 1024..]);
    store.write_region(ino, 0, &v1).unwrap();
    // Byte-exact read-back.
    let back = store.read_file(ino, 0, v1.len() as u64).unwrap();
    assert_eq!(back, v1, "shifted write must round-trip");
    // The winning extent must be a BaseResidual whose residual is the
    // copy/literal delta.
    let limits = store.limits();
    let inode = store.get_inode(ino).unwrap().unwrap();
    let root = match inode.data {
        crate::store::inode::InodeData::File { extent_root } => extent_root,
        _ => panic!("not a file"),
    };
    let (_, bytes) = crate::store::extent_tree::covering(
        root,
        0,
        crate::store::BTREE_ORDER,
        limits.max_fanout,
        &store,
    )
    .unwrap()
    .unwrap();
    let desc = crate::format::descriptor::decode(&bytes, limits).unwrap();
    match &desc {
        crate::core::representation::Representation::BaseResidual { residual, .. } => {
            assert!(
                matches!(
                    residual,
                    crate::core::representation::Residual::BaseSequence { .. }
                ),
                "shifted write must choose BASE_SEQUENCE, got {:?}",
                residual
            );
        }
        other => panic!("expected BASE_RESIDUAL, got {:?}", other.family()),
    }
    // Materialized scrub of the extent matches the chunk's logical bytes.
    let bytes_m = crate::core::materialize::materialize_to_vec(&desc, &store, limits).unwrap();
    assert_eq!(bytes_m, &v1[..65536]);
    // Descriptor is small (the delta's payload lives in the enc object;
    // the inserted noise is the only literal data, everything else is a
    // base copy).
    assert!(
        desc.encoded_size() < 256,
        "descriptor {}",
        desc.encoded_size()
    );
}
