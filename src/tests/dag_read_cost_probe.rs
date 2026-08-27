//! Phase-12A oracle: does reference-DAG depth predict read latency?
//!
//! The 12A question (the brief): **for a reference DAG that is legal and
//! compact, when does its repeated read/materialization cost exceed the
//! storage savings it provides?** EntropyFS already bounds depth
//! (`max_reference_depth` = 4) and rebases over-depth chains on write
//! (`REBASE_DEPTH_THRESHOLD` = 2); 12A must NOT duplicate that machinery.
//! It must measure whether depth, fanout, and cache state actually drive
//! read latency — the "depth != latency" distinction: a depth-4 chain
//! whose dependencies are hot in memory may be cheaper than a depth-1
//! representation requiring a large cold fetch.
//!
//! # Construction (controlled DAG families)
//!
//! Each family group owns its OWN store (isolated model cache, page-cache
//! footprint, and sample ring), and the probe VERIFIES the constructed
//! families from the committed extent descriptors before measuring — the
//! construction fails loudly if the write/commit path produced something
//! else:
//!
//! ```text
//! raw           depth 0: 8 incompressible 64 KiB chunks (RAW objects)
//! exactref      depth 1: 8 chunks aliasing ONE shared RAW object via
//!                        EXACT_REF (fanout 8 — the "one hot base, many
//!                        consumers" caching shape) + 1 lone RAW source
//! base-inline   depth 1..4: BaseResidual chains with the SEARCH-NATURAL
//!               8 files per depth   residuals (tiny-edit XorSparse,
//!                        inline — no per-level objects)
//! base-object   depth 1..4: BaseResidual chains with FORCED rANS-coded
//!               8 files per depth   residuals (each level = enc + model
//!                        objects — the deep AND object-wide DAG)
//! diamond       depth 1..2: one RAW base, 3 residual consumers (fanout
//!                        3), one consumer re-based on a sibling (a
//!                        depth-2 chain) — the mixed shape
//! seqdict       depth 1: a structured 8-file tree run through the real
//!               background optimizer (optimize_pass) — the
//!                        SEQUENCE_DICT / SEQUENCE_SHARED_DICT extents it
//!                        actually creates (the dictionary reference is
//!                        the 9B/9C/9D family's depth-1 DAG)
//! ```
//!
//! The depth-1..4 BaseResidual chains are committed DIRECTLY (encode via
//! `encode_guided` with a crafted `prev_version`, or `RansResidualEncoder`
//! for the forced object-backed variant, then `commit_file_extents`),
//! because the foreground write path REBASES chains at depth ≥ 2
//! (`REBASE_DEPTH_THRESHOLD`): the natural path cannot produce depth-3/4
//! chains to measure. Every update is byte-validated (§32) before commit
//! (the search validates internally; the forced rANS updates are
//! materialized through a `PrefetchContext` over their own objects and
//! compared against the target bytes), and every committed file is read
//! back byte-exactly before it enters the measurement.
//!
//! # Measurement
//!
//! For each family group at each cache state — **cold** (the first read
//! pass: model cache empty, reader-side page cache unwarmed), **warm**
//! (1 prior pass), **hot** (8 prior passes) — every file is read in a
//! seeded random chunk order (64 KiB `read_file` calls), the per-read
//! wall latency feeds p50/p95/p99/mean per depth class, and the
//! per-materialization [`ReadCostSample`]s (the Phase-12A instrumentation:
//! family, depths, DAG nodes, fanout, referenced objects, bytes fetched,
//! I/O wait, decode CPU, total latency, logical bytes) are averaged per
//! depth class. The byte-exactness of every read is asserted (correctness
//! invariant, all builds).
//!
//! # The gate (decided by the evidence tooling, not asserted here)
//!
//! ```text
//! depth predicts p99 (controlling family + cache state)?
//!     yes, meaningfully  -> terminalization is justified (12A-1)
//!     no                 -> record and REJECT the daemon (the brief's
//!                           explicit outcome: depth itself is not the
//!                           cost; cache state and object-fetch width are)
//! ```
//!
//! `depth > N => RAW` is never a candidate — the brief's explicit
//! rejection: it would destroy density without measuring anything.
//!
//! The probe writes its TSV to `$DAG_READ_COST_OUT` when set (the
//! evidence tool's capture path); `$DAG_READ_COST_MODE` stamps the row
//! header. Debug builds run a reduced smoke sweep (correctness asserts
//! hold; the perf rows are diagnostics).

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;

use crate::core::candidate::{BaseChunk, Candidate, CandidateContext, Encoder};
use crate::core::extent::ChunkId;
use crate::core::representation::Representation;
use crate::optimizer::policy::OptimizeOptions;
use crate::optimizer::search::{GuidedContext, SearchMode, encode_guided};
use crate::store::epoch::PrefetchContext;
use crate::store::transaction::CrashHooks;
use crate::store::{ExtentUpdate, NewEntry, Store, StoreConfig};
use tempfile::TempDir;

const CHUNK: u64 = 65536;

fn create_store(dir: &TempDir) -> Arc<Store> {
    let cfg = StoreConfig {
        segment_size: 128 * 1024 * 1024,
        ..Default::default()
    };
    Arc::new(Store::create(dir.path(), &cfg, [0x33; 16]).unwrap())
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

/// Deterministic incompressible 64 KiB chunk (the raw / depth-0 family).
fn noise(seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(CHUNK as usize);
    let mut state = 0x12a_0001u64 ^ seed.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    for _ in 0..CHUNK {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push((state >> 33) as u8);
    }
    out
}

/// Deterministic structured 64 KiB chunk (repeated header + seeded body):
/// the residual families need compressible structure to win over RAW.
fn structured(seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(CHUNK as usize);
    let hdr = format!("ENTROPYFS-12A-STRUCTURED-{seed:016x}\n").into_bytes();
    while out.len() < CHUNK as usize {
        out.extend_from_slice(&hdr);
        let mut state = seed.wrapping_add(out.len() as u64);
        for _ in 0..32 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push(((state >> 33) & 0xff) as u8);
        }
        out.push(b'\n');
    }
    out.truncate(CHUNK as usize);
    out
}

/// A deterministic tiny edit of `base` (scattered single-byte changes at
/// level-dependent positions): the residual is small, so BASE_RESIDUAL
/// beats RAW on cost and the chain is legal at every depth.
fn variant(base: &[u8], level: u64) -> Vec<u8> {
    let mut out = base.to_vec();
    for k in 0..8u64 {
        let pos = ((level * 7919 + k * 104729) % (out.len() as u64 - 4)) as usize;
        out[pos] = out[pos].wrapping_add((k as u8).wrapping_mul(3).wrapping_add(1));
        out[pos + 1] ^= 0x5a;
    }
    out
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * q) as usize]
}

fn family_histogram(store: &Store, inos: &[u64]) -> BTreeMap<&'static str, u64> {
    let limits = *store.limits();
    let mut counts = BTreeMap::new();
    for &ino in inos {
        let Ok(Some(inode)) = store.get_inode(ino) else {
            continue;
        };
        let root = match inode.data {
            crate::store::inode::InodeData::File { extent_root } => extent_root,
            _ => continue,
        };
        if root.is_zero() {
            continue;
        }
        let Ok(entries) = crate::store::extent_tree::scan_all(
            root,
            crate::store::BTREE_ORDER,
            limits.max_fanout,
            store,
        ) else {
            continue;
        };
        for (_, bytes) in entries {
            if let Ok(d) = crate::format::descriptor::decode(&bytes, &limits) {
                *counts.entry(d.family()).or_insert(0) += 1;
            }
        }
    }
    counts
}

/// Commit one byte-validated extent update (the chain-construction path).
///
/// `candidate`: for search-produced updates the search already validated
/// (§32) and this is `None`; for the forced rANS updates the probe
/// materializes through a `PrefetchContext` over the candidate's own
/// objects and asserts the bytes. The store's own commit path does NOT
/// re-validate (it is the optimizer's contract that validation happened
/// in the search), so the probe closes that gap itself before any
/// measurement can run.
fn commit_chain_level(
    store: &Store,
    ino: u64,
    offset: u64,
    update: ExtentUpdate,
    target: &[u8],
    candidate: Option<&Candidate>,
) {
    if let Some(cand) = candidate {
        let limits = *store.limits();
        let mut objects = HashMap::new();
        for o in &cand.objects {
            objects.insert(ChunkId::of(&o.payload), o.payload.clone());
        }
        let empty = HashMap::new();
        let ctx = PrefetchContext::new(store, &objects, Some(&empty), None);
        let got = crate::core::materialize::materialize_to_vec(&cand.representation, &ctx, &limits)
            .unwrap_or_else(|e| panic!("forced candidate must materialize: {e}"));
        assert_eq!(got, target, "forced candidate must be byte-exact (§32)");
    }
    store
        .commit_file_extents(
            ino,
            vec![update],
            Some(target.len() as u64 + offset),
            &CrashHooks::none(),
        )
        .unwrap();
    // The committed file must read back exactly at the update's offset
    // (the chain's materialized truth — the final gate before this file
    // enters the measurement).
    let got = store.read_file(ino, offset, target.len() as u64).unwrap();
    assert_eq!(got, target, "committed chain level must read back exactly");
}

/// Build a BASE_RESIDUAL chain of `depth` levels on top of a RAW base
/// chunk, returning `(file, expected_bytes, depth)` for the base file and
/// each level file.
///
/// `object_backed`: force each level's residual to be rANS-coded (enc +
/// model objects per level — the deep + object-wide DAG) instead of the
/// search-natural inline XorSparse residual.
fn build_base_chain(
    store: &Store,
    depth: u64,
    object_backed: bool,
    tag: u64,
) -> Vec<(u64, Vec<u8>, u8)> {
    let mut files: Vec<(u64, Vec<u8>, u8)> = Vec::new();
    // Level 0: the RAW base, committed through the ordinary write path.
    let b0 = structured(0x12a_0000 + depth * 17 + tag * 131);
    let f0 = create_file(store, &format!("chain-d{depth}-t{tag}-b0"));
    store
        .epoch_write(
            f0,
            0,
            &b0,
            OptimizeOptions::default(),
            store.foreground_policy(),
            &CrashHooks::none(),
        )
        .unwrap();
    store.epoch_checkpoint(&CrashHooks::none()).unwrap();
    files.push((f0, b0.clone(), 0));
    let mut base = BaseChunk {
        id: ChunkId::of(&b0),
        bytes: b0.clone(),
        depth: 0,
    };
    let opts = OptimizeOptions::default();
    let fg = store.foreground_policy();
    for level in 1..=depth {
        let target = variant(files.last().unwrap().1.as_slice(), level);
        let cid = ChunkId::of(&target);
        let f = create_file(store, &format!("chain-d{depth}-t{tag}-l{level}"));
        if object_backed {
            let limits = *store.limits();
            let policy = *store.policy();
            let base_ctx = CandidateContext {
                limits: &limits,
                policy: &policy,
                content_id: cid,
                bases: std::slice::from_ref(&base),
                dedup: None,
            };
            let cands = crate::rans::residual::RansResidualEncoder.encode(&target, &base_ctx);
            let cand = cands
                .into_iter()
                .find(|c| matches!(c.representation, Representation::BaseResidual { .. }))
                .unwrap_or_else(|| panic!("rans residual encoder must produce a BaseResidual"));
            let update = ExtentUpdate {
                offset: 0,
                descriptor: cand.representation.clone(),
                content_id: cid,
                objects: cand.objects.clone(),
            };
            commit_chain_level(store, f, 0, update, &target, Some(&cand));
        } else {
            let ctx = GuidedContext {
                ino: f,
                offset: 0,
                target: &target,
                prev_version: Some(base.clone()),
                dictionary: None,
                shared: None,
                pending: None,
                mode: SearchMode::Foreground,
            };
            let outcome = encode_guided(store, &ctx, opts, fg).unwrap();
            assert!(
                matches!(
                    outcome.update.descriptor,
                    Representation::BaseResidual { .. }
                ),
                "level {level} must be BASE_RESIDUAL, got {:?}",
                outcome.update.descriptor.family()
            );
            assert_eq!(
                outcome.depth, level as u8,
                "level {level} must carry depth {level} (got {})",
                outcome.depth
            );
            let update = outcome.update;
            commit_chain_level(store, f, 0, update, &target, None);
        }
        base = BaseChunk {
            id: cid,
            bytes: target.clone(),
            depth: level as u8,
        };
        files.push((f, target, level as u8));
    }
    files
}

/// Build the mixed diamond: one RAW base, 3 residual consumers (fanout
/// 3), one consumer re-based on a sibling (a depth-2 chain). Returns
/// `(file, expected_bytes, depth)` for the base, the consumers, and the
/// depth-2 file.
fn build_diamond(store: &Store, tag: u64) -> Vec<(u64, Vec<u8>, u8)> {
    let mut files: Vec<(u64, Vec<u8>, u8)> = Vec::new();
    let b0 = structured(0x12a_5000 + tag * 131);
    let f0 = create_file(store, &format!("diamond-t{tag}-b0"));
    store
        .epoch_write(
            f0,
            0,
            &b0,
            OptimizeOptions::default(),
            store.foreground_policy(),
            &CrashHooks::none(),
        )
        .unwrap();
    store.epoch_checkpoint(&CrashHooks::none()).unwrap();
    files.push((f0, b0.clone(), 0));
    let base = BaseChunk {
        id: ChunkId::of(&b0),
        bytes: b0.clone(),
        depth: 0,
    };
    let opts = OptimizeOptions::default();
    let fg = store.foreground_policy();
    let mut consumers: Vec<(u64, Vec<u8>, ChunkId)> = Vec::new();
    for i in 0..3u64 {
        let target = variant(&b0, 100 + i);
        let cid = ChunkId::of(&target);
        let f = create_file(store, &format!("diamond-t{tag}-c{i}"));
        let ctx = GuidedContext {
            ino: f,
            offset: 0,
            target: &target,
            prev_version: Some(base.clone()),
            dictionary: None,
            shared: None,
            pending: None,
            mode: SearchMode::Foreground,
        };
        let outcome = encode_guided(store, &ctx, opts, fg).unwrap();
        assert!(matches!(
            outcome.update.descriptor,
            Representation::BaseResidual { .. }
        ));
        let update = outcome.update;
        commit_chain_level(store, f, 0, update, &target, None);
        consumers.push((f, target.clone(), cid));
        files.push((f, target, 1));
    }
    // The depth-2 chain: consumer 2 re-based on consumer 1.
    let (_, c1_bytes, c1_cid) = &consumers[1];
    let target = variant(c1_bytes, 200);
    let _cid = ChunkId::of(&target);
    let f = create_file(store, &format!("diamond-t{tag}-d2"));
    let base2 = BaseChunk {
        id: *c1_cid,
        bytes: c1_bytes.clone(),
        depth: 1,
    };
    let ctx = GuidedContext {
        ino: f,
        offset: 0,
        target: &target,
        prev_version: Some(base2),
        dictionary: None,
        shared: None,
        pending: None,
        mode: SearchMode::Foreground,
    };
    let outcome = encode_guided(store, &ctx, opts, fg).unwrap();
    assert!(matches!(
        outcome.update.descriptor,
        Representation::BaseResidual { .. }
    ));
    assert_eq!(outcome.depth, 2);
    let update = outcome.update;
    commit_chain_level(store, f, 0, update, &target, None);
    files.push((f, target, 2));
    files
}

/// Build the seqdict family: 12 files whose chunk 1 is a REAL
/// dictionary reference (SEQUENCE_DICT / SEQUENCE_SHARED_DICT) built
/// with the actual 9B/9C encoders, committed through the normal commit
/// path. The corpus is the encoders' ideal shape: an irregular shared
/// core (64 KiB LCG noise — incompressible, so RAW holds chunk 0 and
/// neither RANS nor PERIODIC can compete with a dictionary match) plus a
/// per-file 8-byte id. Chunk 1 shares all but a few bytes with chunk 0,
/// so the dictionary reference + tiny residual strictly beats RAW — the
/// same reason the 9C/9D background pass rewrites real shared-structure
/// files.
fn build_seqdict(store: &Store) -> Vec<(u64, Vec<u8>)> {
    let core_full = noise(0x9c_12a0);
    let core = &core_full[..65528];
    let opts = OptimizeOptions::default();
    let fg = store.foreground_policy();
    let hooks = &CrashHooks::none();
    let mut files = Vec::new();
    // Chunk 0 of every file: core + per-file id (RAW; written through the
    // ordinary write path, then one checkpoint so the committed chunk
    // index resolves the dictionaries).
    let mut c0s: Vec<(u64, Vec<u8>, ChunkId)> = Vec::new();
    for i in 0..12u64 {
        let f = create_file(store, &format!("s{i:04}.cfg"));
        let mut c0 = core.to_vec();
        // Exactly 8 id bytes: core(65528) + 8 = a full 64 KiB chunk, so
        // the write path stores chunk 0 as exactly 65536 bytes (no
        // mid-chunk clip) and the expected bytes are unambiguous.
        c0.extend_from_slice(format!("f{i:04}xc0").as_bytes());
        store.epoch_write(f, 0, &c0, opts, fg, hooks).unwrap();
        c0s.push((f, c0.clone(), ChunkId::of(&c0)));
        files.push((f, c0));
    }
    store.epoch_checkpoint(hooks).unwrap();
    // The shared pool anchor: file 0's chunk 0 (the 9D shape — one
    // committed terminal chunk amortized across the cohort).
    let anchor = BaseChunk {
        id: c0s[0].2,
        bytes: c0s[0].1.clone(),
        depth: 0,
    };
    for (i, (f, _, c0_cid)) in c0s.iter().enumerate() {
        let target = core.to_vec();
        let mut t = target;
        t.extend_from_slice(format!("f{i:04}xc1").as_bytes());
        let cid = ChunkId::of(&t);
        let limits = *store.limits();
        let policy = *store.policy();
        let base_ctx = CandidateContext {
            limits: &limits,
            policy: &policy,
            content_id: cid,
            bases: &[],
            dedup: None,
        };
        let own_dict = BaseChunk {
            id: *c0_cid,
            bytes: c0s[i].1.clone(),
            depth: 0,
        };
        let cands = if i % 2 == 0 {
            // SEQUENCE_DICT: the previous same-file chunk as dictionary.
            let enc = crate::rans::sequence::SequenceDictEncoder {
                dictionary: own_dict.id,
                dict_bytes: own_dict.bytes.clone(),
                dict_depth: own_dict.depth,
            };
            enc.encode(&t, &base_ctx)
        } else {
            // SEQUENCE_SHARED_DICT: the pool anchor ALONE (no in-file
            // dictionary). The 9C shape is "shared anchors serve member
            // first-chunks": with the in-file dict present it always
            // matches as well or better than the anchor, so SRC_SHARED
            // would never appear and the family yields no candidate.
            let enc = crate::rans::sequence::SequenceSharedDictEncoder {
                dictionary: crate::core::extent::ChunkId::ZERO,
                dict_bytes: Vec::new(),
                dict_depth: 0,
                shared: anchor.id,
                shared_bytes: anchor.bytes.clone(),
                shared_depth: anchor.depth,
            };
            enc.encode(&t, &base_ctx)
        };
        let want = if i % 2 == 0 {
            "SEQUENCE_DICT"
        } else {
            "SEQUENCE_SHARED_DICT"
        };
        let cand = cands
            .into_iter()
            .find(|c| c.representation.family() == want)
            .unwrap_or_else(|| panic!("dict encoder must produce a {want}"));
        let update = ExtentUpdate {
            offset: CHUNK,
            descriptor: cand.representation.clone(),
            content_id: cid,
            objects: cand.objects.clone(),
        };
        commit_chain_level(store, *f, CHUNK, update, &t, Some(&cand));
        // The file now has two chunks (0 + the dict-referencing 1);
        // refresh the expected bytes.
        if let Some((_, b)) = files.iter_mut().find(|(ff, _)| ff == f) {
            b.extend_from_slice(&t);
        }
    }
    files
}

/// One family group: its own store (isolated caches + sample ring), the
/// files to read with their expected bytes, the per-chunk depth class,
/// and the expected family histogram (the construction witness).
struct Group {
    _dir: TempDir,
    store: Arc<Store>,
    label: &'static str,
    files: Vec<(u64, Vec<u8>)>,
    /// (ino, chunk offset) -> reference depth class of that chunk.
    depth_of: HashMap<(u64, u64), u8>,
    expected_families: BTreeMap<&'static str, u64>,
}

impl Group {
    fn new(_label: &'static str) -> (TempDir, Arc<Store>) {
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir);
        (dir, store)
    }
}

/// A measured pass over one group: per-depth rows.
struct PassResult {
    cache: &'static str,
    rows: Vec<DepthRow>,
}

struct DepthRow {
    depth: u8,
    n: usize,
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
    mean_us: f64,
    dag_nodes: f64,
    referenced_objects: f64,
    bytes_fetched: f64,
    io_wait_us: f64,
    decode_us: f64,
    sample_latency_us: f64,
}

/// Run one read pass over a group: seeded random chunk order, per-read
/// wall latency, byte-exact verification, sample snapshot, per-depth rows.
fn read_pass(store: &Store, g: &Group, cache: &'static str) -> PassResult {
    store.clear_read_cost();
    let mut order: Vec<(u64, u64)> = Vec::new();
    for (ino, bytes) in &g.files {
        let n_chunks = (bytes.len() as u64 / CHUNK).max(1);
        for c in 0..n_chunks {
            order.push((*ino, c * CHUNK));
        }
    }
    // Deterministic shuffle (LCG permutation of the pair list).
    let mut state = 0x12a_0dd5u64;
    for i in (1..order.len()).rev() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = ((state >> 33) as usize) % (i + 1);
        order.swap(i, j);
    }
    let mut per_read: Vec<(u8, f64)> = Vec::with_capacity(order.len());
    for (ino, off) in &order {
        let t = Instant::now();
        let got = store.read_file(*ino, *off, CHUNK).unwrap();
        let us = t.elapsed().as_secs_f64() * 1e6;
        let expected: &Vec<u8> = &g
            .files
            .iter()
            .find(|(f, _)| f == ino)
            .expect("read target exists")
            .1;
        let slice = &expected
            [*off as usize..(*off as usize + CHUNK.min(expected.len() as u64 - *off) as usize)];
        assert_eq!(
            &got[..slice.len()],
            slice,
            "{}: byte-exact read-back failed at ino {ino} off {off}",
            g.label
        );
        let depth = g.depth_of.get(&(*ino, *off)).copied().unwrap_or(0);
        per_read.push((depth, us));
    }
    // Per-depth latency buckets + the sample aggregates (the ring holds
    // exactly this pass's reads; group-local stores isolate families).
    let samples = store.read_cost_samples();
    let mut rows = Vec::new();
    let mut by_depth: BTreeMap<u8, Vec<f64>> = BTreeMap::new();
    for (d, us) in &per_read {
        by_depth.entry(*d).or_default().push(*us);
    }
    for (depth, lats) in by_depth {
        let mut s = lats.clone();
        s.sort_unstable_by(|a, b| a.total_cmp(b));
        let n = s.len();
        let mut agg: HashMap<&'static str, f64> = HashMap::new();
        let mut cnt = 0usize;
        for smp in &samples {
            if smp.max_path_depth == depth {
                *agg.entry("dag_nodes").or_insert(0.0) += smp.dag_nodes as f64;
                *agg.entry("referenced_objects").or_insert(0.0) += smp.referenced_objects as f64;
                *agg.entry("bytes_fetched").or_insert(0.0) += smp.bytes_fetched as f64;
                *agg.entry("io_wait_ns").or_insert(0.0) += smp.io_wait_ns as f64;
                *agg.entry("decode_ns").or_insert(0.0) += smp.decode_cpu_ns as f64;
                *agg.entry("latency_ns").or_insert(0.0) += smp.read_latency_ns as f64;
                cnt += 1;
            }
        }
        let c = cnt.max(1) as f64;
        rows.push(DepthRow {
            depth,
            n,
            p50_us: percentile(&s, 0.50),
            p95_us: percentile(&s, 0.95),
            p99_us: percentile(&s, 0.99),
            mean_us: s.iter().sum::<f64>() / n.max(1) as f64,
            dag_nodes: agg.get("dag_nodes").copied().unwrap_or(0.0) / c,
            referenced_objects: agg.get("referenced_objects").copied().unwrap_or(0.0) / c,
            bytes_fetched: agg.get("bytes_fetched").copied().unwrap_or(0.0) / c,
            io_wait_us: agg.get("io_wait_ns").copied().unwrap_or(0.0) / 1e3 / c,
            decode_us: agg.get("decode_ns").copied().unwrap_or(0.0) / 1e3 / c,
            sample_latency_us: agg.get("latency_ns").copied().unwrap_or(0.0) / 1e3 / c,
        });
    }
    PassResult { cache, rows }
}

#[test]
fn dag_read_cost_probe() {
    // Debug: a reduced smoke sweep (correctness asserts hold; the perf
    // rows are diagnostics). Release: the sealed full oracle.
    let per_depth_files = if cfg!(debug_assertions) { 2 } else { 8 };
    let passes: &[(&str, u32)] = if cfg!(debug_assertions) {
        &[("cold", 0), ("hot", 2)]
    } else {
        &[("cold", 0), ("warm", 1), ("hot", 8)]
    };

    let mut groups: Vec<Group> = Vec::new();

    // ------------------------------------------------------------------
    // raw (depth 0).
    // ------------------------------------------------------------------
    {
        let (dir, store) = Group::new("raw");
        let mut files = Vec::new();
        let mut depth_of = HashMap::new();
        for i in 0..per_depth_files as u64 {
            let f = create_file(&store, &format!("raw-{i}"));
            let b = noise(0x100 + i);
            store
                .epoch_write(
                    f,
                    0,
                    &b,
                    OptimizeOptions::default(),
                    store.foreground_policy(),
                    &CrashHooks::none(),
                )
                .unwrap();
            store.epoch_checkpoint(&CrashHooks::none()).unwrap();
            files.push((f, b));
            depth_of.insert((f, 0), 0u8);
        }
        let mut expected_families = BTreeMap::new();
        expected_families.insert("RAW", per_depth_files as u64);
        groups.push(Group {
            _dir: dir,
            store,
            label: "raw",
            files,
            depth_of,
            expected_families,
        });
    }

    // ------------------------------------------------------------------
    // exactref (depth 1, shared target — fanout).
    // ------------------------------------------------------------------
    {
        let (dir, store) = Group::new("exactref");
        let shared = structured(0x12a_2000);
        let f0 = create_file(&store, "exactref-src");
        store
            .epoch_write(
                f0,
                0,
                &shared,
                OptimizeOptions::default(),
                store.foreground_policy(),
                &CrashHooks::none(),
            )
            .unwrap();
        store.epoch_checkpoint(&CrashHooks::none()).unwrap();
        let mut files = vec![(f0, shared.clone())];
        let mut depth_of = HashMap::new();
        depth_of.insert((f0, 0), 0u8);
        for i in 0..per_depth_files as u64 {
            let f = create_file(&store, &format!("exactref-{i}"));
            store
                .epoch_write(
                    f,
                    0,
                    &shared,
                    OptimizeOptions::default(),
                    store.foreground_policy(),
                    &CrashHooks::none(),
                )
                .unwrap();
            store.epoch_checkpoint(&CrashHooks::none()).unwrap();
            files.push((f, shared.clone()));
            depth_of.insert((f, 0), 1u8);
        }
        let mut expected_families = BTreeMap::new();
        expected_families.insert("RAW", 1);
        expected_families.insert("EXACT_REF", per_depth_files as u64);
        groups.push(Group {
            _dir: dir,
            store,
            label: "exactref",
            files,
            depth_of,
            expected_families,
        });
    }

    // ------------------------------------------------------------------
    // base-inline chains (depths 1..4, search-natural residuals).
    // ------------------------------------------------------------------
    {
        let (dir, store) = Group::new("base-inline");
        let mut files = Vec::new();
        let mut depth_of = HashMap::new();
        for d in 1..=4u64 {
            for t in 0..per_depth_files as u64 {
                let chain = build_base_chain(&store, d, false, t);
                for (f, b, depth) in chain.iter().skip(1) {
                    files.push((*f, b.clone()));
                    depth_of.insert((*f, 0), *depth);
                }
            }
        }
        let mut expected_families = BTreeMap::new();
        // The group's files are the LEVEL files only (the RAW bases are
        // skipped); depths 1..=4 contribute 1+2+3+4 = 10 level files per
        // chain, all BASE_RESIDUAL.
        expected_families.insert("BASE_RESIDUAL", (10 * per_depth_files) as u64);
        groups.push(Group {
            _dir: dir,
            store,
            label: "base-inline",
            files,
            depth_of,
            expected_families,
        });
    }

    // ------------------------------------------------------------------
    // base-object chains (depths 1..4, forced rANS residuals).
    // ------------------------------------------------------------------
    {
        let (dir, store) = Group::new("base-object");
        let mut files = Vec::new();
        let mut depth_of = HashMap::new();
        for d in 1..=4u64 {
            for t in 0..per_depth_files as u64 {
                let chain = build_base_chain(&store, d, true, t);
                for (f, b, depth) in chain.iter().skip(1) {
                    files.push((*f, b.clone()));
                    depth_of.insert((*f, 0), *depth);
                }
            }
        }
        let mut expected_families = BTreeMap::new();
        // Same shape as base-inline: 10 level files per chain.
        expected_families.insert("BASE_RESIDUAL", (10 * per_depth_files) as u64);
        groups.push(Group {
            _dir: dir,
            store,
            label: "base-object",
            files,
            depth_of,
            expected_families,
        });
    }

    // ------------------------------------------------------------------
    // diamond (mixed: one base, fanout-3 consumers, depth-2 chain).
    // ------------------------------------------------------------------
    {
        let (dir, store) = Group::new("diamond");
        let mut files = Vec::new();
        let mut depth_of = HashMap::new();
        for t in 0..per_depth_files as u64 {
            let dia = build_diamond(&store, t);
            for (f, b, depth) in dia {
                files.push((f, b));
                depth_of.insert((f, 0), depth);
            }
        }
        let mut expected_families = BTreeMap::new();
        expected_families.insert("RAW", per_depth_files as u64);
        expected_families.insert("BASE_RESIDUAL", (4 * per_depth_files) as u64);
        groups.push(Group {
            _dir: dir,
            store,
            label: "diamond",
            files,
            depth_of,
            expected_families,
        });
    }

    // ------------------------------------------------------------------
    // seqdict (the background optimizer's real shared-dict extents).
    // ------------------------------------------------------------------
    {
        let (dir, store) = Group::new("seqdict");
        let files = build_seqdict(&store);
        let mut depth_of = HashMap::new();
        for (f, bytes) in &files {
            // Chunk 0 is the RAW dictionary candidate (terminal); chunk 1
            // references it (depth 1).
            depth_of.insert((*f, 0), 0u8);
            if bytes.len() as u64 > CHUNK {
                depth_of.insert((*f, CHUNK), 1u8);
            }
        }
        groups.push(Group {
            _dir: dir,
            store,
            label: "seqdict",
            files,
            depth_of,
            expected_families: BTreeMap::new(), // filled by the witness below
        });
    }

    // ------------------------------------------------------------------
    // Construction witnesses: every family must be what the oracle claims
    // (the probe fails loudly on a construction drift).
    // ------------------------------------------------------------------
    for g in groups.iter() {
        if g.label == "seqdict" {
            let inos: Vec<u64> = g.files.iter().map(|(f, _)| *f).collect();
            let hist = family_histogram(&g.store, &inos);
            // 12 RAW chunk-0s + 6 SEQUENCE_DICT + 6 SEQUENCE_SHARED_DICT
            // chunk-1s (the encoder-driven construction).
            assert_eq!(
                hist.get("RAW").copied().unwrap_or(0),
                12,
                "seqdict RAW chunk-0s"
            );
            assert_eq!(
                hist.get("SEQUENCE_DICT").copied().unwrap_or(0),
                6,
                "seqdict SEQUENCE_DICT chunk-1s"
            );
            assert_eq!(
                hist.get("SEQUENCE_SHARED_DICT").copied().unwrap_or(0),
                6,
                "seqdict SEQUENCE_SHARED_DICT chunk-1s"
            );
            continue;
        }
        let inos: Vec<u64> = g.files.iter().map(|(f, _)| *f).collect();
        let hist = family_histogram(&g.store, &inos);
        if g.label == "exactref" {
            // The source chunk's terminal family is whatever the write
            // path produced (RAW for incompressible, SEQUENCE_RANS for
            // the structured source here) — the construction witness is
            // the ALIAS count + the total extent count, not the source's
            // exact family.
            let aliases = hist.get("EXACT_REF").copied().unwrap_or(0);
            let total: u64 = hist.values().sum();
            assert_eq!(aliases, per_depth_files as u64, "exactref alias count");
            assert_eq!(total, per_depth_files as u64 + 1, "exactref extent count");
            continue;
        }
        if g.label == "diamond" {
            // The bases' terminal family is SEQUENCE_RANS for this
            // structured corpus (not RAW) — the witness is the residual
            // consumer count + the total extent count.
            let residuals = hist.get("BASE_RESIDUAL").copied().unwrap_or(0);
            let total: u64 = hist.values().sum();
            assert_eq!(
                residuals,
                (4 * per_depth_files) as u64,
                "diamond residual count"
            );
            assert_eq!(total, (5 * per_depth_files) as u64, "diamond extent count");
            continue;
        }
        assert_eq!(
            hist, g.expected_families,
            "{}: family construction drifted (got {hist:?}, want {:?})",
            g.label, g.expected_families
        );
    }

    // ------------------------------------------------------------------
    // Measure: passes x groups, per-depth rows.
    // ------------------------------------------------------------------
    println!("\n==== Phase-12A read-cost oracle ====");
    println!(
        "{:<12} {:>6} {:>6} {:>8} {:>8} {:>8} {:>8} {:>5} {:>5} {:>9} {:>8} {:>7} {:>7} {:>7}",
        "family",
        "cache",
        "depth",
        "p50_us",
        "p95_us",
        "p99_us",
        "mean_us",
        "nodes",
        "objs",
        "bytes_fet",
        "io_us",
        "dec_us",
        "samp_us",
        "n"
    );
    let mut tsv_rows: Vec<(String, PassResult)> = Vec::new();
    for g in groups.iter() {
        for (cache, prior_passes) in passes {
            for _ in 0..*prior_passes {
                read_pass(&g.store, g, "warmup");
            }
            let pr = read_pass(&g.store, g, cache);
            for r in &pr.rows {
                println!(
                    "{:<12} {:>6} {:>6} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>5.0} {:>5.0} {:>9.0} {:>8.1} {:>7.1} {:>7.1} {:>7}",
                    g.label,
                    pr.cache,
                    r.depth,
                    r.p50_us,
                    r.p95_us,
                    r.p99_us,
                    r.mean_us,
                    r.dag_nodes,
                    r.referenced_objects,
                    r.bytes_fetched,
                    r.io_wait_us,
                    r.decode_us,
                    r.sample_latency_us,
                    r.n
                );
            }
            tsv_rows.push((g.label.to_string(), pr));
        }
    }

    // The gate rows: within-family depth-4 vs depth-1 p99 (cold and hot)
    // — the evidence tooling's decision input.
    println!("\n-- gate rows (p99 depth-4 / depth-1 within family) --");
    for g in groups
        .iter()
        .filter(|g| g.label == "base-inline" || g.label == "base-object")
    {
        for (cache, prior_passes) in passes {
            for _ in 0..*prior_passes {
                read_pass(&g.store, g, "warmup");
            }
            let pr = read_pass(&g.store, g, cache);
            let d1 = pr.rows.iter().find(|r| r.depth == 1);
            let d4 = pr.rows.iter().find(|r| r.depth == 4);
            if let (Some(a), Some(b)) = (d1, d4) {
                println!(
                    "{:<12} {:<6} p99 ratio d4/d1 = {:.2} ({:.1} us / {:.1} us)",
                    g.label,
                    cache,
                    b.p99_us / a.p99_us.max(1.0),
                    b.p99_us,
                    a.p99_us
                );
            }
        }
    }

    // ------------------------------------------------------------------
    // TSV for the evidence tool.
    // ------------------------------------------------------------------
    let mut tsv = String::new();
    tsv.push_str(
        "mode\tfamily\tcache\tdepth\tn\tp50_us\tp95_us\tp99_us\tmean_us\tdag_nodes\treferenced_objects\tbytes_fetched\tio_wait_us\tdecode_us\tsample_latency_us\n",
    );
    let mode = std::env::var("DAG_READ_COST_MODE").unwrap_or_else(|_| "unknown".into());
    for (fam_label, pr) in &tsv_rows {
        for r in &pr.rows {
            tsv.push_str(&format!(
                "{mode}\t{fam_label}\t{}\t{}\t{}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{:.0}\t{:.0}\t{:.0}\t{:.1}\t{:.1}\t{:.1}\n",
                pr.cache,
                r.depth,
                r.n,
                r.p50_us,
                r.p95_us,
                r.p99_us,
                r.mean_us,
                r.dag_nodes,
                r.referenced_objects,
                r.bytes_fetched,
                r.io_wait_us,
                r.decode_us,
                r.sample_latency_us
            ));
        }
    }
    if let Ok(path) = std::env::var("DAG_READ_COST_OUT") {
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, &tsv).expect("write probe summary");
        println!("probe summary written to {path}");
    }
}
