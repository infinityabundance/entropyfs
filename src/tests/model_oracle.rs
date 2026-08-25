//! Phase-9G model-sharing ORACLE (diagnostic, not sealed): measure, on the
//! real source tree, how many bytes the sequence families' persisted
//! entropy models would save under model-sharing strategies, BEFORE any
//! format change. The user's rule: only implement the ModelBundle format
//! if the oracle says the bytes are really there.
//!
//! Strategies compared (whole-store sequence-extent totals):
//!
//! - S0 baseline: the actual store after the pool + deep pass + GC (the
//!   Phase-9G0 model-cost-aware models).
//! - S1 intra-extent exhaustive partition: every set partition of an
//!   extent's streams shares one model per block (5 partitions for 3
//!   streams, 15 for 4); each stream may still fall back to RAW. Purely
//!   intra-extent, but still needs the ModelBundle stream→model mapping.
//! - S2 directory bundle: one model per stream TYPE trained on the
//!   directory's aggregate histograms; every member encodes against it;
//!   each directory's models are persisted once (content-addressed).
//! - S3/S4 directory bundle pools (2 / 4): greedy marginal selection over
//!   candidate bundles (member bundles + the aggregate), each extent picks
//!   its best bundle — the same shape as the shared-dictionary anchor pool.
//!
//! Everything is exact: real rANS encodes with the actual histogram
//! normalizer, real model encodings, and content-addressed accounting
//! (each unique model object counted once).

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::core::extent::ChunkId;
use crate::core::representation::{RansCodec, Representation};
use crate::evidence::corpus::source_tree_files;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use crate::tests::srctree_diag::numbers;

const SCALE: u8 = 14;
const CODEC: RansCodec = RansCodec::Interleaved2;

/// One decoded sequence-family extent.
struct ExtentStreams {
    /// Raw streams in family order (commands, literals, offsets,
    /// [sources|lengths]).
    streams: Vec<Vec<u8>>,
    /// Stream-type tags for cross-family aggregation: 0 commands, 1
    /// literals, 2 offsets, 3 dict-sources, 4 deep-lengths.
    types: Vec<u8>,
    /// Directory inode.
    dir: u64,
    /// Persisted model object id (S0 accounting; unique ids are counted
    /// once because the store content-addresses model objects).
    model_id: Option<ChunkId>,
    /// Persisted enc object id (same content-addressed accounting).
    enc_id: Option<ChunkId>,
}

fn model_of(hist: &[u32; 256]) -> Option<Vec<u8>> {
    let m = crate::rans::model::normalize_histogram(hist, SCALE, CODEC)?;
    Some(crate::rans::metadata::encode_model(&m))
}

/// Encode one stream against an optional model: rANS when the encoded +
/// model-share beats RAW (share = the model's bytes when this extent owns
/// the model, 0 when the model is already paid by the cohort), else RAW.
/// Returns (encoded_bytes, is_raw).
fn best_stream_cost(stream: &[u8], model: Option<&[u8]>, model_share: u64) -> (u64, bool) {
    let raw = stream.len() as u64;
    match model {
        Some(m) => {
            let parsed = match crate::rans::metadata::decode_model(m, 4096) {
                Ok(p) => p,
                Err(_) => return (raw, true),
            };
            match crate::rans::residual::encode_stream(stream, &parsed) {
                Ok(enc) if (enc.len() as u64) + model_share < raw => (enc.len() as u64, false),
                _ => (raw, true),
            }
        }
        None => (raw, true),
    }
}

/// All set partitions of `0..n` (Bell numbers: 5 for n=3, 15 for n=4).
fn set_partitions(n: usize) -> Vec<Vec<Vec<usize>>> {
    fn gen_partitions(
        n: usize,
        idx: usize,
        blocks: &mut Vec<Vec<usize>>,
        out: &mut Vec<Vec<Vec<usize>>>,
    ) {
        if idx == n {
            out.push(blocks.clone());
            return;
        }
        for b in 0..blocks.len() {
            blocks[b].push(idx);
            gen_partitions(n, idx + 1, blocks, out);
            blocks[b].pop();
        }
        blocks.push(vec![idx]);
        gen_partitions(n, idx + 1, blocks, out);
        blocks.pop();
    }
    let mut out = Vec::new();
    let mut blocks: Vec<Vec<usize>> = Vec::new();
    if n > 0 {
        blocks.push(vec![0]);
        gen_partitions(n, 1, &mut blocks, &mut out);
    }
    out
}

/// Extract the raw streams + accounting for one sequence-family extent.
fn extract_extent(store: &Store, ino: u64, offset: u64, dir: u64) -> Option<ExtentStreams> {
    let limits = *store.limits();
    let inode = store.get_inode(ino).ok()??;
    let root = match inode.data {
        crate::store::inode::InodeData::File { extent_root } => extent_root,
        _ => return None,
    };
    let (_, bytes) = crate::store::extent_tree::covering(
        root,
        offset,
        crate::store::BTREE_ORDER,
        limits.max_fanout,
        store,
    )
    .ok()??;
    let desc = crate::format::descriptor::decode(
        &bytes,
        limits.max_descriptor_bytes,
        limits.max_inline_bytes,
        limits.max_palette,
        limits.max_period,
        limits.max_chunk_size,
    )
    .ok()?;
    let refs = |model: ChunkId, enc_obj: ChunkId| crate::rans::sequence::StreamRefs {
        model,
        enc_obj,
        scale_bits: SCALE,
        codec: CODEC,
    };
    let (streams, types): (Vec<Vec<u8>>, Vec<u8>) = match &desc {
        Representation::Rans {
            model,
            enc_obj,
            scale_bits: _,
            codec: _,
            len,
            ..
        } => {
            let m = store.fetch_object(model).ok().flatten()?;
            let parsed = crate::rans::metadata::decode_model(&m, limits.max_model_bytes).ok()?;
            let e = store.fetch_object(enc_obj).ok().flatten()?;
            let out = crate::rans::residual::decode_stream(&parsed, &e, *len).ok()?;
            (vec![out], vec![1])
        }
        Representation::SequenceRans {
            model,
            enc_obj,
            scale_bits: _,
            codec: _,
            seq_len,
            lit_len,
            off_len,
            cmds,
            lit_out,
            ..
        } => {
            let v = crate::rans::sequence::decode_streams_n(
                store,
                &limits,
                refs(*model, *enc_obj),
                &[*seq_len, *lit_len, *off_len],
                *cmds as u64,
                *lit_out as u64,
                None,
                1,
            )
            .ok()?;
            (v, vec![0, 1, 2])
        }
        Representation::SequenceDeep {
            model,
            enc_obj,
            scale_bits: _,
            codec: _,
            seq_len,
            lit_len,
            off_len,
            len_len,
            cmds,
            lit_out,
            ..
        } => {
            let d = crate::rans::sequence::decode_deep_streams(
                store,
                &limits,
                refs(*model, *enc_obj),
                crate::rans::sequence::DeepLens {
                    seq_len: *seq_len,
                    lit_len: *lit_len,
                    off_len: *off_len,
                    len_len: *len_len,
                    cmds: *cmds,
                    lit_out: *lit_out,
                },
            )
            .ok()?;
            (
                vec![d.commands, d.literals, d.offsets, d.lengths],
                vec![0, 1, 2, 4],
            )
        }
        Representation::SequenceDict {
            dictionary: _,
            dictionary_len: _,
            model,
            enc_obj,
            scale_bits: _,
            codec: _,
            seq_len,
            lit_len,
            off_len,
            src_len,
            cmds,
            lit_out,
            ..
        }
        | Representation::SequenceSharedDict {
            dictionary: _,
            dictionary_len: _,
            shared: _,
            shared_len: _,
            model,
            enc_obj,
            scale_bits: _,
            codec: _,
            seq_len,
            lit_len,
            off_len,
            src_len,
            cmds,
            lit_out,
            ..
        } => {
            let d = crate::rans::sequence::decode_four_streams(
                store,
                &limits,
                refs(*model, *enc_obj),
                crate::rans::sequence::FourStreams {
                    seq_len: *seq_len,
                    lit_len: *lit_len,
                    off_len: *off_len,
                    src_len: *src_len,
                    cmds: *cmds,
                    lit_out: *lit_out,
                },
            )
            .ok()?;
            (
                vec![d.commands, d.literals, d.offsets, d.sources],
                vec![0, 1, 2, 3],
            )
        }
        _ => return None,
    };
    Some(ExtentStreams {
        streams,
        types,
        dir,
        model_id: desc_model_id(&desc),
        enc_id: desc_enc_id(&desc),
    })
}

fn desc_model_id(d: &Representation) -> Option<ChunkId> {
    match d {
        Representation::Rans { model, .. }
        | Representation::SequenceRans { model, .. }
        | Representation::SequenceDeep { model, .. }
        | Representation::SequenceDict { model, .. }
        | Representation::SequenceSharedDict { model, .. } => Some(*model),
        _ => None,
    }
}

fn desc_enc_id(d: &Representation) -> Option<ChunkId> {
    match d {
        Representation::Rans { enc_obj, .. }
        | Representation::SequenceRans { enc_obj, .. }
        | Representation::SequenceDeep { enc_obj, .. }
        | Representation::SequenceDict { enc_obj, .. }
        | Representation::SequenceSharedDict { enc_obj, .. } => Some(*enc_obj),
        _ => None,
    }
}

fn hist_of(stream: &[u8]) -> [u32; 256] {
    let mut h = [0u32; 256];
    for &b in stream {
        h[b as usize] += 1;
    }
    h
}

#[test]
fn print_model_sharing_oracle() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = source_tree_files(root).unwrap();

    // Build the real tree with directories, run the pool + deep pass, GC.
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = StoreConfig {
        segment_size: 4 * 1024 * 1024,
        ..Default::default()
    };
    let store = Store::create(dir.path(), &cfg, [0x6f; 16]).unwrap();
    let mut dir_cache: HashMap<String, u64> = HashMap::new();
    dir_cache.insert(String::new(), store.current_root().root_dir_ino);
    let mut ino_dir: Vec<(u64, u64)> = Vec::new();
    for (rel, bytes) in &files {
        let (dir_part, name) = match rel.rsplit_once('/') {
            Some((d, n)) => (d.to_string(), n.to_string()),
            None => (String::new(), rel.clone()),
        };
        if !dir_cache.contains_key(&dir_part) {
            let mut cur = String::new();
            let mut cur_ino = store.current_root().root_dir_ino;
            for comp in dir_part.split('/') {
                if comp.is_empty() {
                    continue;
                }
                let next_path = if cur.is_empty() {
                    comp.to_string()
                } else {
                    format!("{cur}/{comp}")
                };
                let ino = match dir_cache.get(&next_path) {
                    Some(&c) => c,
                    None => {
                        let existing = store.dir_lookup(cur_ino, comp.as_bytes()).unwrap();
                        let ino = match existing {
                            Some(e) => e.ino,
                            None => store
                                .create_entry(
                                    cur_ino,
                                    comp.as_bytes(),
                                    NewEntry::dir(0o755, 1000, 1000),
                                    &CrashHooks::none(),
                                )
                                .unwrap(),
                        };
                        dir_cache.insert(next_path.clone(), ino);
                        ino
                    }
                };
                cur = next_path;
                cur_ino = ino;
            }
            dir_cache.insert(dir_part.clone(), cur_ino);
        }
        let ino = store
            .create_entry(
                dir_cache[&dir_part],
                name.as_bytes(),
                NewEntry::file(0o644, 1000, 1000),
                &CrashHooks::none(),
            )
            .unwrap();
        let mut writes: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut off = 0u64;
        while off < bytes.len() as u64 {
            let len = 65536u64.min(bytes.len() as u64 - off);
            writes.push((off, bytes[off as usize..(off + len) as usize].to_vec()));
            off += len;
        }
        store
            .write_region_batch(ino, &writes, OptimizeOptions::default())
            .unwrap();
        ino_dir.push((ino, dir_cache[&dir_part]));
    }
    crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
    crate::optimizer::background::shared_dict_pass(&store, OptimizeOptions::default(), None)
        .unwrap();
    crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
    let (_, reachable, _b, _f) = numbers(&store);

    // Extract all sequence-family extents.
    let mut extents: Vec<ExtentStreams> = Vec::new();
    for (ino, dir) in &ino_dir {
        let limits = *store.limits();
        let inode = store.get_inode(*ino).unwrap().unwrap();
        let root = match inode.data {
            crate::store::inode::InodeData::File { extent_root } => extent_root,
            _ => continue,
        };
        if root.is_zero() {
            continue;
        }
        for (off, _) in crate::store::extent_tree::scan_all(
            root,
            crate::store::BTREE_ORDER,
            limits.max_fanout,
            &store,
        )
        .unwrap()
        {
            if let Some(e) = extract_extent(&store, *ino, off, *dir) {
                extents.push(e);
            }
        }
    }
    // S0 baseline: unique persisted object bytes. The store is
    // content-addressed, so a model/enc object shared by many extents is
    // persisted once; per-extent summation would double-count it.
    let mut s0_model_ids: HashSet<ChunkId> = HashSet::new();
    let mut s0_enc_ids: HashSet<ChunkId> = HashSet::new();
    for e in &extents {
        if let Some(id) = e.model_id {
            s0_model_ids.insert(id);
        }
        if let Some(id) = e.enc_id {
            s0_enc_ids.insert(id);
        }
    }
    let obj_len = |id: &ChunkId| {
        store
            .object_index()
            .get(id)
            .map(|l| l.stored_len)
            .unwrap_or(0)
    };
    let s0_models: u64 = s0_model_ids.iter().map(obj_len).sum();
    let s0_enc: u64 = s0_enc_ids.iter().map(obj_len).sum();

    // S1: intra-extent exhaustive partition. Every set partition of an
    // extent's streams shares one model per block (5 for 3 streams, 15 for
    // 4); identical block models within an extent count once.
    let mut s1_models = 0u64;
    let mut s1_enc = 0u64;
    for e in &extents {
        let n = e.streams.len();
        let hists: Vec<[u32; 256]> = e.streams.iter().map(|s| hist_of(s)).collect();
        let mut best = u64::MAX;
        let mut best_models = 0u64;
        let mut best_enc = 0u64;
        for part in set_partitions(n) {
            // One model per block, trained on the block's summed histogram.
            let mut models: Vec<Option<Vec<u8>>> = vec![None; n];
            let mut model_bytes = 0u64;
            let mut seen = HashSet::new();
            for block in &part {
                let mut sum = [0u32; 256];
                for &si in block {
                    for b in 0..256 {
                        sum[b] = sum[b].saturating_add(hists[si][b]);
                    }
                }
                let m = model_of(&sum);
                if let Some(mb) = &m {
                    if seen.insert(mb.clone()) {
                        model_bytes = model_bytes.saturating_add(mb.len() as u64);
                    }
                }
                for &si in block {
                    models[si] = m.clone();
                }
            }
            let mut enc = 0u64;
            for (si, stream) in e.streams.iter().enumerate() {
                let (c, _) = best_stream_cost(stream, models[si].as_deref(), 0);
                enc = enc.saturating_add(c);
            }
            let cost = model_bytes.saturating_add(enc);
            if cost < best {
                best = cost;
                best_models = model_bytes;
                best_enc = enc;
            }
        }
        s1_models = s1_models.saturating_add(best_models);
        s1_enc = s1_enc.saturating_add(best_enc);
    }

    // S2: one model per stream TYPE per directory, aggregate histograms.
    // S3/S4: greedy bundle pools (candidates = member bundles + aggregate).
    let mut s2_models = 0u64;
    let mut s2_enc = 0u64;
    let mut pool_models: BTreeMap<u64, Vec<u64>> = BTreeMap::new(); // dir -> pool size -> model bytes
    let mut pool_enc: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for (dir, members) in group_by_dir(&extents) {
        // Aggregate histograms per stream type.
        let mut agg: BTreeMap<u8, [u32; 256]> = BTreeMap::new();
        for e in &members {
            for (si, &t) in e.types.iter().enumerate() {
                let h = hist_of(&e.streams[si]);
                let a = agg.entry(t).or_insert([0u32; 256]);
                for b in 0..256 {
                    a[b] = a[b].saturating_add(h[b]);
                }
            }
        }
        let mut agg_models: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
        for (t, h) in &agg {
            if let Some(m) = model_of(h) {
                agg_models.insert(*t, m);
            }
        }
        let agg_cost: u64 = agg_models.values().map(|m| m.len() as u64).sum();
        let mut dir_enc = 0u64;
        for e in &members {
            for (si, &t) in e.types.iter().enumerate() {
                let (c, _) =
                    best_stream_cost(&e.streams[si], agg_models.get(&t).map(|v| v.as_slice()), 0);
                dir_enc = dir_enc.saturating_add(c);
            }
        }
        s2_models = s2_models.saturating_add(agg_cost);
        s2_enc = s2_enc.saturating_add(dir_enc);

        // Pool candidates: each member's bundle (its own stream histograms)
        // plus the aggregate. Greedy marginal selection for K in {2, 4}.
        let mut cands: Vec<BTreeMap<u8, Vec<u8>>> = Vec::new();
        let mut seen_cand: HashSet<Vec<u8>> = HashSet::new();
        let mut member_costs: Vec<u64> = Vec::new();
        for e in &members {
            let mut b: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
            for (si, &t) in e.types.iter().enumerate() {
                b.entry(t)
                    .or_insert_with(|| model_of(&hist_of(&e.streams[si])).unwrap_or_default());
            }
            let key: Vec<u8> = b.values().flatten().cloned().collect();
            if seen_cand.insert(key) {
                let cost: u64 = b.values().map(|m| m.len() as u64).sum();
                member_costs.push(cost);
                cands.push(b);
            }
        }
        // The aggregate is also a candidate.
        let agg_key: Vec<u8> = agg_models.values().flatten().cloned().collect();
        if seen_cand.insert(agg_key) {
            cands.push(agg_models.clone());
            member_costs.push(agg_cost);
        }
        // Greedy marginal selection over the RAW baseline (a member with no
        // selected bundle stores every stream raw). `covered` tracks each
        // member's best enc cost among the selected bundles; a candidate's
        // gain is how much it improves that. Stop when the best remaining
        // gain is zero: an extra bundle that improves no member is never
        // worth its model bytes.
        let raw_costs: Vec<u64> = members
            .iter()
            .map(|e| e.streams.iter().map(|s| s.len() as u64).sum())
            .collect();
        for &k in &[2usize, 4] {
            let mut selected: Vec<usize> = Vec::new();
            let mut covered: Vec<u64> = raw_costs.clone();
            for _ in 0..k {
                let mut best_i: Option<usize> = None;
                let mut best_gain = 0u64;
                for (i, cand) in cands.iter().enumerate() {
                    if selected.contains(&i) {
                        continue;
                    }
                    // Marginal savings: for each member, how much the
                    // candidate improves over the best already covered.
                    let mut total = 0u64;
                    for (m, e) in members.iter().enumerate() {
                        let mut cost = 0u64;
                        for (si, &t) in e.types.iter().enumerate() {
                            let (c, _) = best_stream_cost(
                                &e.streams[si],
                                cand.get(&t).map(|v| v.as_slice()),
                                0,
                            );
                            cost = cost.saturating_add(c);
                        }
                        total = total.saturating_add(covered[m].saturating_sub(cost));
                    }
                    if best_i.is_none() || total > best_gain {
                        best_i = Some(i);
                        best_gain = total;
                    }
                }
                let Some(i) = best_i else { break };
                if best_gain == 0 {
                    break;
                }
                selected.push(i);
                for (m, e) in members.iter().enumerate() {
                    let mut cost = 0u64;
                    for (si, &t) in e.types.iter().enumerate() {
                        let (c, _) = best_stream_cost(
                            &e.streams[si],
                            cands[i].get(&t).map(|v| v.as_slice()),
                            0,
                        );
                        cost = cost.saturating_add(c);
                    }
                    covered[m] = covered[m].min(cost);
                }
            }
            let model_cost: u64 = selected.iter().map(|&i| member_costs[i]).sum();
            let enc_cost: u64 = covered.iter().sum();
            pool_models.entry(dir).or_default().push(model_cost);
            pool_enc.entry(dir).or_default().push(enc_cost);
        }
    }
    let s3_models: u64 = pool_models.values().map(|v| v[0]).sum();
    let s3_enc: u64 = pool_enc.values().map(|v| v[0]).sum();
    let s4_models: u64 = pool_models.values().map(|v| v[1]).sum();
    let s4_enc: u64 = pool_enc.values().map(|v| v[1]).sum();

    println!("\n==== Phase-9G model-sharing oracle (real tree) ====");
    println!(
        "sequence extents {}   store reachable {} B\n",
        extents.len(),
        reachable
    );
    let s0_total = s0_models + s0_enc;
    // Signed savings (negative = the strategy loses bytes) so a falsified
    // strategy is visible as a negative number, not a silent zero.
    let save = |a: u64, b: u64| a as i128 - b as i128;
    println!(
        "S0 baseline (actual, 9G0 models):   models {s0_models:>8} + enc {s0_enc:>8} = {s0_total:>9} B"
    );
    println!(
        "S1 intra-extent best partition:     models {s1_models:>8} + enc {s1_enc:>8} = {:>9} B  (saves {})",
        s1_models + s1_enc,
        save(s0_total, s1_models + s1_enc)
    );
    println!(
        "S2 directory bundle (1):            models {s2_models:>8} + enc {s2_enc:>8} = {:>9} B  (saves {})",
        s2_models + s2_enc,
        save(s0_total, s2_models + s2_enc)
    );
    println!(
        "S3 directory bundle pool (2):       models {s3_models:>8} + enc {s3_enc:>8} = {:>9} B  (saves {})",
        s3_models + s3_enc,
        save(s0_total, s3_models + s3_enc)
    );
    println!(
        "S4 directory bundle pool (4):       models {s4_models:>8} + enc {s4_enc:>8} = {:>9} B  (saves {})",
        s4_models + s4_enc,
        save(s0_total, s4_models + s4_enc)
    );
}

fn group_by_dir(extents: &[ExtentStreams]) -> BTreeMap<u64, Vec<&ExtentStreams>> {
    let mut m: BTreeMap<u64, Vec<&ExtentStreams>> = BTreeMap::new();
    for e in extents {
        m.entry(e.dir).or_default().push(e);
    }
    m
}
