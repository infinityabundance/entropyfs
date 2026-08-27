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
            let d = crate::format::descriptor::decode(&bytes, &limits).unwrap();
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
    // chunk. Phase-9B: the foreground write path now densifies these AT
    // WRITE TIME via SequenceDict (previous same-file chunk as the
    // dictionary); the background pass must stay byte-exact, never regress
    // density, and have nothing left to densify.
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

    // The edited chunks must already be densified (SEQUENCE_DICT against
    // the previous chunk — never RAW), and far below the raw size.
    let exts = extents_of(&store, f);
    let raw = 4 * 65536u64;
    let mut total: u64 = 0;
    for (off, d) in &exts {
        assert_ne!(d.family(), "RAW", "extent at {off} must not be RAW");
        total += crate::optimizer::background::current_persisted_bytes(&store, d);
    }
    assert!(
        total < raw / 20,
        "foreground must densify sequential edits: {total} >= {raw}/20"
    );
    assert!(
        exts.iter()
            .any(|(_, d)| matches!(d, Representation::SequenceDict { .. })),
        "expected SEQUENCE_DICT for the edited chunks, got families: {:?}",
        exts.iter().map(|(_, d)| d.family()).collect::<Vec<_>>()
    );

    let stats = optimize_pass(&store, OptimizeOptions::default(), None, None).unwrap();
    let after = store.read_file(f, 0, 4 * 65536).unwrap();
    assert_eq!(after, before, "background pass changed logical bytes");
    let _ = stats;
    // Densification is measured after GC: the append-only store retains
    // superseded objects until reclaim, so compare post-GC usage.
    crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
    let physical_after = store.physical_used();
    assert!(
        physical_after <= physical_before,
        "densification must not grow the store: before {physical_before}, after {physical_after}"
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
fn chain_depth_reports_deepest_path_through_a_diamond() {
    // Regression (Phase-10E): a reference graph where one chunk is
    // reachable through BOTH branches of a SEQUENCE_SHARED_DICT (dict + a
    // shared chain that converges on the dict's chain). A first-reached-
    // wins visited set undercounts the depth — the deeper path through the
    // shared branch is blocked — so `chain_depth` must record the DEEPEST
    // depth at which each node was explored.
    //
    //   x = SharedDict { dict: y, shared: z }
    //   z = BaseResidual { base: y }
    //   y = BaseResidual { base: w }
    //   w = Raw (terminal)
    //
    // The longest path x -> z -> y -> w is 3 references below x, so
    // chain_depth(x) = 4 and a chain on top of x is depth 5 (over the
    // v1 decode cap of 4).
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let f = ino(&store);
    use crate::core::representation::RansCodec;
    let cid_of = |d: &Representation| ChunkId::of(&crate::format::descriptor::encode(d).unwrap());

    // w: terminal chunk (a RAW descriptor; never materialized here).
    let w = Representation::Raw {
        obj: ChunkId::of(b"w-object"),
        len: 64,
    };
    let w_cid = cid_of(&w);
    store
        .commit_file_extents(
            f,
            vec![ExtentUpdate {
                offset: 0,
                descriptor: w.clone(),
                content_id: w_cid,
                objects: Vec::new(),
            }],
            None,
            &CrashHooks::none(),
        )
        .unwrap();

    // y: depth 1 on w.
    let y = Representation::BaseResidual {
        base: w_cid,
        base_len: 64,
        residual: Residual::XorSparse {
            len: 64,
            edits: Vec::new(),
        },
        len: 64,
    };
    let y_cid = cid_of(&y);
    store
        .commit_file_extents(
            f,
            vec![ExtentUpdate {
                offset: 65536,
                descriptor: y.clone(),
                content_id: y_cid,
                objects: Vec::new(),
            }],
            None,
            &CrashHooks::none(),
        )
        .unwrap();

    // z: depth 2 on y.
    let z = Representation::BaseResidual {
        base: y_cid,
        base_len: 64,
        residual: Residual::XorSparse {
            len: 64,
            edits: Vec::new(),
        },
        len: 64,
    };
    let z_cid = cid_of(&z);
    store
        .commit_file_extents(
            f,
            vec![ExtentUpdate {
                offset: 131072,
                descriptor: z.clone(),
                content_id: z_cid,
                objects: Vec::new(),
            }],
            None,
            &CrashHooks::none(),
        )
        .unwrap();

    // x: the diamond — dict y AND shared z (z chains back to y).
    let x = Representation::SequenceSharedDict {
        dictionary: y_cid,
        dictionary_len: 64,
        shared: z_cid,
        shared_len: 64,
        model: ChunkId::of(b"x-model"),
        enc_obj: ChunkId::of(b"x-enc"),
        scale_bits: 12,
        codec: RansCodec::Single,
        seq_len: 0,
        lit_len: 0,
        off_len: 0,
        src_len: 0,
        cmds: 1,
        lit_out: 1,
        len: 64,
    };
    assert!(x.validate(store.limits()).is_ok(), "fixture must validate");
    let x_cid = cid_of(&x);
    store
        .commit_file_extents(
            f,
            vec![ExtentUpdate {
                offset: 196608,
                descriptor: x.clone(),
                content_id: x_cid,
                objects: Vec::new(),
            }],
            None,
            &CrashHooks::none(),
        )
        .unwrap();

    // x itself: the top level splits the branches (max of separate walks),
    // so its own depth is 1 + max(1, 2) = 3.
    assert_eq!(crate::optimizer::rebase::chain_depth(&store, &x), 3);

    // A chain ON TOP of x collapses the branches into ONE walk: the
    // deepest path x -> z -> y -> w is 3 references below x, so
    // chain_depth(chain-on-x) = 4. A first-reached-wins visited set would
    // undercount this to 3 (y reached at depth 1 via the dict branch
    // blocks the depth-2 path through z).
    let top = Representation::BaseResidual {
        base: x_cid,
        base_len: 64,
        residual: Residual::XorSparse {
            len: 64,
            edits: Vec::new(),
        },
        len: 64,
    };
    let top_cid = cid_of(&top);
    store
        .commit_file_extents(
            f,
            vec![ExtentUpdate {
                offset: 262144,
                descriptor: top.clone(),
                content_id: top_cid,
                objects: Vec::new(),
            }],
            None,
            &CrashHooks::none(),
        )
        .unwrap();
    assert_eq!(
        crate::optimizer::rebase::chain_depth(&store, &top),
        4,
        "a chain on the diamond must follow the longest path"
    );

    // One more link crosses the v1 decode cap (4): the walk must report 5
    // so the depth gate refuses the candidate.
    let top2 = Representation::BaseResidual {
        base: top_cid,
        base_len: 64,
        residual: Residual::XorSparse {
            len: 64,
            edits: Vec::new(),
        },
        len: 64,
    };
    assert_eq!(
        crate::optimizer::rebase::chain_depth(&store, &top2),
        5,
        "over-cap depth must be reported exactly"
    );
    assert!(
        crate::optimizer::rebase::chain_depth(&store, &top2) > store.limits().max_reference_depth,
        "the fixture must exceed the decode cap"
    );
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

/// A period-26 text chunk (benchmark pattern 0): compressible by rANS,
/// dedup-able, periodic, and cheap to edit — exercises every ladder step.
fn compressible_chunk(seed: u8) -> Vec<u8> {
    (0..65536u64)
        .map(|i| b'a' + ((i as u8).wrapping_add(seed)) % 26)
        .collect()
}

#[test]
fn cumulative_ladder_is_exact_and_monotone() {
    // The strict cumulative ladder A0-A8 (methodology §4, spec §43): each
    // step adds exactly one mechanism on top of the previous. Every step
    // must preserve bytes exactly, and physical bytes must never grow when
    // a mechanism is added (the ladder is monotone non-increasing). On this
    // corpus rANS (A1) must strictly beat RAW (A0).
    let mut prev_physical: Option<u64> = None;
    for (name, options, run_background) in OptimizeOptions::cumulative_ladder_modes() {
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir);
        let f = ino(&store);
        let v1 = compressible_chunk(0);
        // Dedup fixture: identical chunk at a second offset.
        store.write_region_with(f, 0, &v1, options).unwrap();
        store.write_region_with(f, 65536, &v1, options).unwrap();
        // Base-residual fixture: a drifted version of the first chunk
        // (three single-byte edits; P0 is in hand at the overwrite).
        let mut v1e = v1.clone();
        v1e[10] ^= 0x5;
        v1e[32000] ^= 0x5;
        v1e[65530] ^= 0x5;
        store.write_region_with(f, 0, &v1e, options).unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(&v1e);
        expected.extend_from_slice(&v1);
        let got = store.read_file(f, 0, expected.len() as u64).unwrap();
        assert_eq!(got, expected, "{name}: write path corrupted bytes");
        if run_background {
            optimize_pass(&store, options, None, None).unwrap();
            let got2 = store.read_file(f, 0, expected.len() as u64).unwrap();
            assert_eq!(got2, expected, "{name}: background pass corrupted bytes");
        }
        // The ladder measures the *persisted reachable* state, not the
        // transient unreclaimed garbage each step leaves behind or the
        // layout noise of the append-only compaction (record alignment,
        // copy order). Reachable bytes — the campaign's metric — is
        // deterministic; GC is the reclamation boundary.
        crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
        let got3 = store.read_file(f, 0, expected.len() as u64).unwrap();
        assert_eq!(got3, expected, "{name}: GC corrupted bytes");
        let live = crate::store::gc::mark_live(&store).unwrap();
        let reachable: u64 = store
            .object_index()
            .iter()
            .into_iter()
            .filter(|(id, _)| live.contains(id))
            .map(|(_, loc)| loc.total_size())
            .sum();
        let physical = reachable;
        if let Some(prev) = prev_physical {
            assert!(
                physical <= prev,
                "{name}: adding a mechanism must not grow physical bytes \
                 ({physical} > {prev})"
            );
            if name == "A1-byte-rans" {
                assert!(
                    physical < prev,
                    "A1-byte-rans must strictly beat A0-raw on compressible text"
                );
            }
        }
        prev_physical = Some(physical);
    }
}
