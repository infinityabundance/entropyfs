//! SparseBlock64 store-level integration: a 64 KiB chunk with `k` in the
//! plain-SPARSE u128-overflow range must be representable through the real
//! write path, round-trip byte-exactly, and beat RAW.

#![forbid(unsafe_code)]

use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x71; 16]).unwrap()
}

#[test]
fn overflow_range_sparse_roundtrips_via_write_path() {
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
    // 500 marked bytes in a 64 KiB chunk: C(65536, 500) overflows u128,
    // so plain SPARSE cannot represent it; SparseBlock64 must.
    let mut content = vec![0u8; 65536];
    let mut placed = 0usize;
    let mut x: u64 = 0x0ddc_0ffe_e15e_5eed;
    while placed < 500 {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let pos = ((x >> 32) as usize) % 65536;
        if content[pos] == 0 {
            content[pos] = (placed % 223) as u8 + 1;
            placed += 1;
        }
    }
    store.write_region(ino, 0, &content).unwrap();
    let back = store.read_file(ino, 0, 65536).unwrap();
    assert_eq!(back, content, "read-back must be byte-exact");
    // The winning family must be the blockwise sparse codec (not RAW).
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
    let desc = crate::format::descriptor::decode(
        &bytes,
        limits.max_descriptor_bytes,
        limits.max_inline_bytes,
        limits.max_palette,
        limits.max_period,
        limits.max_chunk_size,
    )
    .unwrap();
    assert!(
        matches!(
            desc,
            crate::core::representation::Representation::SparseBlock64 { .. }
        ),
        "expected SPARSE_BLOCK64, got {:?}",
        desc.family()
    );
    // GC must preserve the model/enc objects and reads must survive.
    crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
    let back = store.read_file(ino, 0, 65536).unwrap();
    assert_eq!(back, content, "read-back after GC must be byte-exact");
}
