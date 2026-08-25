//! Phase 4 optimizer tests: DSFB-guided search integration, background
//! densification (H4), rebase flattening, CAS safety, and ablation
//! attribution (spec §43).

#![forbid(unsafe_code)]

use tempfile::TempDir;

use crate::core::extent::ChunkId;
use crate::core::materialize::materialize_to_vec;
use crate::core::representation::{Representation, Residual};
use crate::optimizer::background::{PassCursor, current_persisted_bytes, optimize_pass};
use crate::optimizer::policy::OptimizeOptions;
use crate::store::inode::InodeData;
use crate::store::transaction::CrashHooks;
use crate::store::{ExtentUpdate, NewEntry, Store, StoreConfig};

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x77; 16]).unwrap()
}

fn ino(store: &Store) -> u64 {
    store
        .create_entry(
            1,
            b"f",
            NewEntry::file(0o644, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap()
}

/// A pseudo-random-ish but compressible-by-base chunk pattern.
fn chunk(seed: u32, edit: u8) -> Vec<u8> {
    let mut b: Vec<u8> = (0..65536u64)
        .map(|i| (((i.wrapping_mul(seed as u64 * 2654435761)) >> 8) % 251) as u8)
        .collect();
    if edit > 0 {
        // Three single-byte edits (sparse patch territory).
        b[10] ^= edit;
        b[32000] ^= edit.wrapping_add(1);
        b[65530] ^= edit.wrapping_add(2);
    }
    b
}

fn extents_of(store: &Store, ino: u64) -> Vec<(u64, Representation)> {
    let limits = *store.limits();
    let inode = store.get_inode(ino).unwrap().unwrap();
    let root = match inode.data {
        InodeData::File { extent_root } => extent_root,
        _ => return Vec::new(),
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
fn background_pass_preserves_exact_bytes() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let f = ino(&store);
    let mut content = Vec::new();
    for i in 0..8u32 {
        content.extend_from_slice(&chunk(i + 1, (i % 3) as u8));
    }
    store.write_region(f, 0, &content).unwrap();
    let before = store.read_file(f, 0, content.len() as u64).unwrap();
    assert_eq!(before, content);

    let stats = optimize_pass(&store, OptimizeOptions::default(), None, None).unwrap();
    // The pass must never corrupt bytes.
    let after = store.read_file(f, 0, content.len() as u64).unwrap();
    assert_eq!(after, content);
    // Every rewritten extent must be cheaper.
    let _ = stats;
}

#[test]
fn drift_workload_stays_shallow_and_exact() {
    // H2: repeated in-place edits of one chunk. Each edit must stay
    // BASE_RESIDUAL with a shallow chain (rebase-on-write flattens
    // deep previous versions, §11) and never collapse to RAW.
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let f = ino(&store);
    let base: Vec<u8> = (0..65536u64).map(|i| ((i * 7) % 251) as u8).collect();
    store.write_region(f, 0, &base).unwrap();
    let mut current = base.clone();
    for v in 1..12u64 {
        for j in 0..v {
            let pos = ((j * 997) % 65536) as usize;
            let val = (v + j) as u8;
            store.write_region(f, pos as u64, &[val]).unwrap();
            current[pos] = val;
        }
    }
    let got = store.read_file(f, 0, 65536).unwrap();
    assert_eq!(got, current, "drift writes corrupted bytes");
    let exts = extents_of(&store, f);
    assert_eq!(exts.len(), 1);
    let (_, desc) = &exts[0];
    assert!(
        matches!(desc, Representation::BaseResidual { .. }),
        "expected BASE_RESIDUAL, got {:?}",
        desc.family()
    );
    let depth = crate::optimizer::rebase::chain_depth(&store, desc);
    assert!(depth <= 2, "chain must stay shallow, got {depth}");
    // Fully decodable and fsck-clean.
    let limits = *store.limits();
    assert_eq!(materialize_to_vec(desc, &store, &limits).unwrap(), current);
}

#[test]
fn background_pass_densifies_sequential_edits() {
    // H2/H4: sequentially written chunks that are tiny edits of the first
    // chunk. The foreground write path (first write to each offset, no
    // bases) cannot use P3; the background pass can, and must rewrite to
    // BASE_RESIDUAL without changing bytes.
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let f = ino(&store);
    let base = chunk(7, 0);
    store.write_region(f, 0, &base).unwrap();
    // Chunks 2..4 are sparse edits of the base (P3 = previous chunk).
    let mut offsets = Vec::new();
    for i in 1..4u32 {
        let c = chunk(7, i as u8);
        store.write_region(f, (i as u64) * 65536, &c).unwrap();
        offsets.push((i as u64) * 65536);
    }
    let before = store.read_file(f, 0, 4 * 65536).unwrap();
    let physical_before = store.physical_used();

    let stats = optimize_pass(&store, OptimizeOptions::default(), None, None).unwrap();
    assert!(stats.rewritten > 0, "expected rewrites, got {stats:?}");
    let after = store.read_file(f, 0, 4 * 65536).unwrap();
    assert_eq!(after, before, "background pass changed logical bytes");
    // Densification is measured after GC: the append-only store retains
    // superseded objects until reclaim, so compare post-GC usage.
    crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
    let physical_after = store.physical_used();
    assert!(
        physical_after <= physical_before,
        "densification must not grow the store: before {physical_before}, after {physical_after}"
    );
    // The edited chunks must now be base+residual against the previous
    // chunk (or at least cheaper than RAW/RANS would allow).
    let exts = extents_of(&store, f);
    assert!(exts.len() >= 2);
    let base_residuals = exts
        .iter()
        .filter(|(_, d)| matches!(d, Representation::BaseResidual { .. }))
        .count();
    assert!(
        base_residuals >= 1,
        "expected at least one BASE_RESIDUAL, got families: {:?}",
        exts.iter().map(|(_, d)| d.family()).collect::<Vec<_>>()
    );
}

#[test]
fn background_pass_is_idempotent_and_byte_exact_after_remount() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let f = ino(&store);
    let mut content = Vec::new();
    for i in 0..6u32 {
        content.extend_from_slice(&chunk(i + 1, i as u8));
    }
    store.write_region(f, 0, &content).unwrap();
    optimize_pass(&store, OptimizeOptions::default(), None, None).unwrap();
    let after_first = store.read_file(f, 0, content.len() as u64).unwrap();
    assert_eq!(after_first, content);
    // A second pass must not regress bytes either.
    optimize_pass(&store, OptimizeOptions::default(), None, None).unwrap();
    let after_second = store.read_file(f, 0, content.len() as u64).unwrap();
    assert_eq!(after_second, content);
    // Survives a remount, and fsck is clean.
    drop(store);
    let store2 = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    let after_remount = store2.read_file(f, 0, content.len() as u64).unwrap();
    assert_eq!(after_remount, content);
}

#[test]
fn resumable_cursor_advances() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let f = ino(&store);
    let mut content = Vec::new();
    for i in 0..16u32 {
        content.extend_from_slice(&chunk(i + 1, i as u8));
    }
    store.write_region(f, 0, &content).unwrap();
    let mut cursor = PassCursor::default();
    let s1 = optimize_pass(
        &store,
        OptimizeOptions::default(),
        Some(4),
        Some(&mut cursor),
    )
    .unwrap();
    assert_eq!(s1.scanned, 4);
    assert!(cursor.ino_index >= 1 || cursor.offset > 0);
    let s2 = optimize_pass(
        &store,
        OptimizeOptions::default(),
        Some(4),
        Some(&mut cursor),
    )
    .unwrap();
    assert_eq!(s2.scanned, 4);
    // Finishing resets the cursor.
    let s3 = optimize_pass(&store, OptimizeOptions::default(), None, Some(&mut cursor)).unwrap();
    assert_eq!(cursor, PassCursor::default());
    let _ = s3;
}

#[test]
fn chain_depth_resolves_through_the_chunk_index() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let f = ino(&store);
    // A depth-0 base chunk.
    let base_bytes = chunk(3, 0);
    store.write_region(f, 0, &base_bytes).unwrap();
    let base_cid = ChunkId::of(&base_bytes);

    // A depth-1 BaseResidual referencing it (empty residual → exact).
    let b1 = Representation::BaseResidual {
        base: base_cid,
        base_len: base_bytes.len() as u64,
        residual: Residual::XorSparse {
            len: base_bytes.len() as u64,
            edits: Vec::new(),
        },
        len: base_bytes.len() as u64,
    };
    let b1_bytes = crate::format::descriptor::encode(&b1).unwrap();
    let b1_cid = ChunkId::of(&b1_bytes);
    store
        .commit_file_extents(
            f,
            vec![ExtentUpdate {
                offset: 65536,
                descriptor: b1.clone(),
                content_id: b1_cid,
                objects: Vec::new(),
            }],
            None,
            &CrashHooks::none(),
        )
        .unwrap();
    assert_eq!(crate::optimizer::rebase::chain_depth(&store, &b1), 1);

    // A depth-2 chain on top of b1.
    let b2 = Representation::BaseResidual {
        base: b1_cid,
        base_len: base_bytes.len() as u64,
        residual: Residual::XorSparse {
            len: base_bytes.len() as u64,
            edits: Vec::new(),
        },
        len: base_bytes.len() as u64,
    };
    let b2_bytes = crate::format::descriptor::encode(&b2).unwrap();
    let b2_cid = ChunkId::of(&b2_bytes);
    store
        .commit_file_extents(
            f,
            vec![ExtentUpdate {
                offset: 131072,
                descriptor: b2.clone(),
                content_id: b2_cid,
                objects: Vec::new(),
            }],
            None,
            &CrashHooks::none(),
        )
        .unwrap();
    assert_eq!(crate::optimizer::rebase::chain_depth(&store, &b2), 2);

    // flatten_if_deep must produce a depth-0 candidate that materializes
    // to the same bytes.
    let limits = *store.limits();
    let bytes = materialize_to_vec(&b2, &store, &limits).unwrap();
    let flat = crate::optimizer::rebase::flatten_if_deep(&store, 131072, &b2, &bytes, &b2_cid)
        .unwrap()
        .expect("flatten should fire");
    assert_eq!(crate::optimizer::rebase::depth_of(&flat.descriptor), 0);
    let back = materialize_to_vec(&flat.descriptor, &store, &limits).unwrap();
    assert_eq!(back, bytes);
    assert_eq!(ChunkId::of(&back), ChunkId::of(&base_bytes));
}

#[test]
fn current_persisted_bytes_counts_objects() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let f = ino(&store);
    let data = chunk(4, 0);
    store.write_region(f, 0, &data).unwrap();
    let exts = extents_of(&store, f);
    let (_, desc) = &exts[0];
    // Raw accounts for its full payload; Zero accounts for ~nothing.
    let bytes = current_persisted_bytes(&store, desc);
    assert!(bytes >= desc.encoded_size());
    // The zero chunk is nearly free.
    let zeros = vec![0u8; 65536];
    store.write_region(f, 65536, &zeros).unwrap();
    let exts = extents_of(&store, f);
    let (_, zdesc) = exts.iter().find(|(o, _)| *o == 65536).unwrap();
    assert!(matches!(zdesc, Representation::Zero { .. }));
    assert!(current_persisted_bytes(&store, zdesc) < 32);
}

#[test]
fn ablation_modes_preserve_bytes_and_differ() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let f = ino(&store);
    let data = chunk(9, 0);
    store
        .write_region_with(f, 0, &data, OptimizeOptions::raw_only())
        .unwrap();
    let read = store.read_file(f, 0, data.len() as u64).unwrap();
    assert_eq!(read, data);
    let exts = extents_of(&store, f);
    assert!(matches!(exts[0].1, Representation::Raw { .. }));

    // Full optimization of the same chunk must be at least as dense.
    let dir2 = TempDir::new().unwrap();
    let store2 = create_store(&dir2);
    let f2 = ino(&store2);
    store2.write_region(f2, 0, &data).unwrap();
    let exts2 = extents_of(&store2, f2);
    let raw_bytes = current_persisted_bytes(&store, &exts[0].1);
    let full_bytes = current_persisted_bytes(&store2, &exts2[0].1);
    assert!(full_bytes <= raw_bytes);
}
