//! Phase-9C evidence gate (temporary diagnostic — superseded by the
//! sealed campaign's tree corpus): does the previous-chunk SequenceDict
//! dictionary capture cross-chunk context on a *real* source tree, or is
//! the src-pack gain dominated by cross-FILE structure that per-file
//! dictionaries cannot see?
//!
//! Measurements printed with `--nocapture`:
//! - zstd -1/-19 whole-pack, per-file, per-64KiB;
//! - EntropyFS full on the pack (one inode) vs the tree (one inode per
//!   file), post-GC reachable bytes and family distributions.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::core::candidate::{CandidateContext, Encoder};
use crate::core::cost::Policy;
use crate::core::limits::Limits;
use crate::evidence::corpus::{source_tree_files, source_tree_pack};
use crate::optimizer::policy::OptimizeOptions;
use crate::rans::sequence::SequenceDictEncoder;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 4 * 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x91; 16]).unwrap()
}

fn new_file(store: &Store) -> u64 {
    store
        .create_entry(
            1,
            b"f",
            NewEntry::file(0o644, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap()
}

fn write_chunks(store: &Store, ino: u64, bytes: &[u8]) {
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
}

fn numbers(store: &Store) -> (u64, u64, u64, BTreeMap<String, u64>) {
    // (logical, reachable, total_backing, families)
    let total_backing = dir_bytes(store.dir());
    let unreachable = crate::store::gc::unreachable_bytes(store).unwrap();
    let records_total: u64 = store
        .object_index()
        .iter()
        .into_iter()
        .map(|(_, loc)| loc.total_size())
        .sum();
    let reachable = records_total.saturating_sub(unreachable);
    let logical = store.logical_bytes().unwrap();
    let mut families: BTreeMap<String, u64> = BTreeMap::new();
    for ino in store.all_inodes().unwrap() {
        let Some(inode) = store.get_inode(ino).unwrap() else {
            continue;
        };
        let root = match inode.data {
            crate::store::inode::InodeData::File { extent_root } => extent_root,
            _ => continue,
        };
        for (_, bytes) in
            crate::store::extent_tree::scan_all(root, crate::store::BTREE_ORDER, 256, store)
                .unwrap()
        {
            let d = crate::format::descriptor::decode(&bytes, 1 << 20, 4096, 256, 1 << 16, 1 << 16)
                .unwrap();
            *families.entry(d.family().to_string()).or_insert(0) += 1;
            let _ = d;
        }
    }
    (logical, reachable, total_backing, families)
}

fn dir_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(md) = e.metadata() {
                    total += md.len();
                }
            }
        }
    }
    total
}

/// zstd -level of `data` as a single stream (bytes), via the binary.
fn zstd_bytes(data: &[u8], level: i32) -> Option<usize> {
    let tmp_in = tempfile::NamedTempFile::new().ok()?;
    std::fs::write(tmp_in.path(), data).ok()?;
    let out = Command::new("zstd")
        .args(["-q", &format!("-{level}"), "-c"])
        .arg(tmp_in.path())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(out.stdout.len())
}

#[test]
fn print_srctree_gate_evidence() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let pack = source_tree_pack(root).unwrap();
    let files = source_tree_files(root).unwrap();
    let logical: u64 = files.iter().map(|(_, b)| b.len() as u64).sum();

    // --- zstd baselines ---
    let z_whole_1 = zstd_bytes(&pack, 1);
    let z_whole_19 = zstd_bytes(&pack, 19);
    let mut z_per_file_1 = 0usize;
    let mut z_per_file_19 = 0usize;
    for (_, b) in &files {
        z_per_file_1 += zstd_bytes(b, 1).unwrap_or(b.len());
        z_per_file_19 += zstd_bytes(b, 19).unwrap_or(b.len());
    }
    let mut z_chunk_1 = 0usize;
    let mut z_chunk_19 = 0usize;
    for c in pack.chunks(65536) {
        z_chunk_1 += zstd_bytes(c, 1).unwrap_or(c.len());
        z_chunk_19 += zstd_bytes(c, 19).unwrap_or(c.len());
    }

    // --- EntropyFS on the pack (one inode) ---
    let d1 = TempDir::new().unwrap();
    let s1 = create_store(&d1);
    let ino = new_file(&s1);
    write_chunks(&s1, ino, &pack);
    crate::store::gc::collect(&s1, &CrashHooks::none()).unwrap();
    let (l1, r1, b1, f1) = numbers(&s1);

    // --- EntropyFS on the tree (one inode per file) ---
    let d2 = TempDir::new().unwrap();
    let s2 = create_store(&d2);
    for (i, (name, bytes)) in files.iter().enumerate() {
        let ino = s2
            .create_entry(
                1,
                format!("f{i:04}").as_bytes(),
                NewEntry::file(0o644, 1000, 1000),
                &CrashHooks::none(),
            )
            .unwrap();
        write_chunks(&s2, ino, bytes);
        let _ = name;
    }
    crate::store::gc::collect(&s2, &CrashHooks::none()).unwrap();
    let (l2, r2, b2, f2) = numbers(&s2);

    println!("\n==== Phase-9C evidence gate: src pack vs real tree ====");
    println!("files: {}   logical: {logical} B", files.len());
    println!(
        "single-chunk files: {}",
        files.iter().filter(|(_, b)| b.len() <= 65536).count()
    );
    println!("\n-- zstd baselines (pack = {logical} B) --");
    if let Some(n) = z_whole_1 {
        println!(
            "zstd -1 whole-pack: {n:>10} B  ({:.3}x)",
            logical as f64 / n as f64
        );
    }
    if let Some(n) = z_whole_19 {
        println!(
            "zstd -19 whole-pack: {n:>10} B  ({:.3}x)",
            logical as f64 / n as f64
        );
    }
    println!(
        "zstd -1 per-file:  {z_per_file_1:>10} B  ({:.3}x)",
        logical as f64 / z_per_file_1.max(1) as f64
    );
    println!(
        "zstd -19 per-file: {z_per_file_19:>10} B  ({:.3}x)",
        logical as f64 / z_per_file_19.max(1) as f64
    );
    println!(
        "zstd -1 per-64KiB: {z_chunk_1:>10} B  ({:.3}x)",
        logical as f64 / z_chunk_1.max(1) as f64
    );
    println!(
        "zstd -19 per-64KiB: {z_chunk_19:>10} B ({:.3}x)",
        logical as f64 / z_chunk_19.max(1) as f64
    );
    println!("\n-- EntropyFS (post-GC) --");
    println!(
        "pack (1 inode): logical {l1}  reachable {r1}  ({:.3}x)  backing {b1}",
        l1 as f64 / r1.max(1) as f64
    );
    println!(
        "tree (per-file): logical {l2}  reachable {r2}  ({:.3}x)  backing {b2}",
        l2 as f64 / r2.max(1) as f64
    );
    println!("\npack families: {f1:?}");
    println!("tree families: {f2:?}");

    // Gate assertions (recorded, not enforced as pass/fail here): the
    // per-file EntropyFS result must be far below the pack result for 9C
    // to be warranted; assert the structural precondition instead.
    let single_chunk = files.iter().filter(|(_, b)| b.len() <= 65536).count();
    assert!(single_chunk as f64 / files.len() as f64 > 0.9);
    assert_eq!(l1, pack.len() as u64);
    assert_eq!(l2, logical);
}

/// Phase-9C ceiling prototype: encode every single-chunk file against
/// candidate shared dictionaries (directory siblings / global largest),
/// using the EXISTING SequenceDict encoder, and sum the cheapest valid
/// candidates. Tells us whether a shared-dictionary representation can
/// plausibly recover the cross-file structure the pack exploits, before
/// building any new representation.
#[test]
fn print_shared_dict_ceiling() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = source_tree_files(root).unwrap();
    let limits = Limits::default();
    let policy = Policy::default();

    // First chunk (≤ 64 KiB) of each file, grouped by directory.
    struct Dir {
        // (relative_path, first_chunk_bytes)
        members: Vec<(String, Vec<u8>)>,
    }
    let mut dirs: BTreeMap<String, Dir> = BTreeMap::new();
    for (name, bytes) in &files {
        let d = name.rsplit('/').nth(1).unwrap_or(".").to_string();
        let first = bytes[..bytes.len().min(65536)].to_vec();
        dirs.entry(d)
            .or_insert(Dir {
                members: Vec::new(),
            })
            .members
            .push((name.clone(), first));
    }

    // Candidate dicts: every distinct first-chunk in the directory, plus
    // the global largest first-chunk. Deduplicate by ChunkId.
    let mut global_cands: Vec<Vec<u8>> = Vec::new();
    for dir in dirs.values() {
        for (_, b) in &dir.members {
            global_cands.push(b.clone());
        }
    }
    global_cands.sort_by_key(|b| std::cmp::Reverse(b.len()));
    let mut global_unique: Vec<Vec<u8>> = Vec::new();
    for b in global_cands {
        let id = crate::core::extent::ChunkId::of(&b);
        if !global_unique
            .iter()
            .any(|u| crate::core::extent::ChunkId::of(u) == id)
        {
            global_unique.push(b);
        }
    }
    let global_dict = global_unique.first().cloned();

    let mut raw_total = 0u64;
    let mut dir_anchor_total = 0u64;
    let mut global_anchor_total = 0u64;
    let mut global_anchor_used = 0u64;
    let mut min_total = 0u64;
    let mut dict_hits = 0u64;

    for dir in dirs.values() {
        // Per-directory best anchor: argmin Σ encode cost over members.
        let mut best_dir = u64::MAX;
        for (_, cand) in &dir.members {
            if cand.len() < 256 {
                continue;
            }
            let mut total = 0u64;
            for (_, b) in &dir.members {
                if b.len() < 128 {
                    total += b.len() as u64;
                    continue;
                }
                total += encode_with_dict(b, cand, &limits, &policy).unwrap_or(b.len() as u64);
            }
            best_dir = best_dir.min(total);
        }
        let mut no_dict_total = 0u64;
        for (_, b) in &dir.members {
            raw_total += b.len() as u64;
            no_dict_total += b.len() as u64;
            // Best dict among the directory's own candidates.
            let mut best = b.len() as u64;
            for (_, cand) in &dir.members {
                if cand.len() < 256 {
                    continue;
                }
                if let Some(c) = encode_with_dict(b, cand, &limits, &policy) {
                    best = best.min(c);
                }
            }
            // Global anchor as an extra candidate.
            if let Some(g) = &global_dict {
                if let Some(c) = encode_with_dict(b, g, &limits, &policy) {
                    if c < best {
                        best = c;
                        global_anchor_used += 1;
                    }
                }
            }
            if best < b.len() as u64 {
                dict_hits += 1;
            }
            min_total += best;
        }
        if best_dir != u64::MAX {
            dir_anchor_total += best_dir;
        } else {
            dir_anchor_total += no_dict_total;
        }
        global_anchor_total += no_dict_total;
    }
    // Global anchor applied to every file: Σ encode(file, global_dict).
    if let Some(g) = &global_dict {
        let mut t = 0u64;
        for dir in dirs.values() {
            for (_, b) in &dir.members {
                t += encode_with_dict(b, g, &limits, &policy).unwrap_or(b.len() as u64);
            }
        }
        global_anchor_total = t;
    }

    println!("\n==== Phase-9C shared-dict ceiling (prototype, existing encoder) ====");
    println!("single-chunk logical bytes: {raw_total}");
    println!(
        "dir-anchor (best single dict per dir): {dir_anchor_total}  ({:.3}x)",
        raw_total as f64 / dir_anchor_total.max(1) as f64
    );
    println!(
        "global-anchor (one dict for all):      {global_anchor_total}  ({:.3}x)",
        raw_total as f64 / global_anchor_total.max(1) as f64
    );
    println!(
        "per-file best-of-dir+global:           {min_total}  ({:.3}x)  dict hits {dict_hits}/{raw_total}",
        raw_total as f64 / min_total.max(1) as f64
    );
    println!("global anchor used for {global_anchor_used} files",);
}

/// Run the REAL shared-dict pass on a tree written with its real directory
/// structure (mirrors the campaign tree court) and print what it achieves
/// against the actual incumbents.
#[test]
fn print_shared_dict_pass_on_real_tree() {
    use std::collections::HashMap;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = source_tree_files(root).unwrap();

    let dir = TempDir::new().unwrap();
    let cfg = crate::store::StoreConfig {
        segment_size: 4 * 1024 * 1024,
        ..Default::default()
    };
    let store = crate::store::Store::create(dir.path(), &cfg, [0x9c; 16]).unwrap();
    let mut dir_cache: HashMap<String, u64> = HashMap::new();
    dir_cache.insert(String::new(), store.current_root().root_dir_ino);
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
                                    crate::store::NewEntry::dir(0o755, 1000, 1000),
                                    &crate::store::transaction::CrashHooks::none(),
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
                crate::store::NewEntry::file(0o644, 1000, 1000),
                &crate::store::transaction::CrashHooks::none(),
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
            .write_region_batch(
                ino,
                &writes,
                crate::optimizer::policy::OptimizeOptions::default(),
            )
            .unwrap();
    }
    crate::store::gc::collect(&store, &crate::store::transaction::CrashHooks::none()).unwrap();
    let (l1, r1, _b1, f1) = numbers(&store);
    let stats = crate::optimizer::background::shared_dict_pass(
        &store,
        crate::optimizer::policy::OptimizeOptions::default(),
        None,
    )
    .unwrap();
    crate::store::gc::collect(&store, &crate::store::transaction::CrashHooks::none()).unwrap();
    let (l2, r2, _b2, f2) = numbers(&store);
    println!("\n==== shared-dict pass on the REAL tree (real dirs) ====");
    println!(
        "before: logical {l1} reachable {r1} ({:.3}x) fam {f1:?}",
        l1 as f64 / r1.max(1) as f64
    );
    println!("pass:   {stats:?}");
    println!(
        "after:  logical {l2} reachable {r2} ({:.3}x) fam {f2:?}",
        l2 as f64 / r2.max(1) as f64
    );
}

/// Phase-9E diagnostic: the deep matcher (SEQUENCE_DEEP) versus the fast
/// matcher (SEQUENCE_RANS) on the src pack chunks — the per-64K matcher
/// quality question the 9E review told us to measure before deepening.
#[test]
fn print_deep_vs_fast_on_pack() {
    use crate::core::candidate::{CandidateContext, Encoder};
    use crate::core::cost::Policy;
    use crate::core::limits::Limits;
    use crate::rans::sequence::{SequenceDeepEncoder, SequenceEncoder};
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let pack = source_tree_pack(root).unwrap();
    let limits = Limits::default();
    let policy = Policy::default();
    let mut fast_total = 0u64;
    let mut deep_total = 0u64;
    let mut deep_wins = 0usize;
    let mut chunks = 0usize;
    for c in pack.chunks(65536) {
        if c.len() < 128 {
            fast_total += c.len() as u64;
            deep_total += c.len() as u64;
            continue;
        }
        let ctx = CandidateContext {
            limits: &limits,
            policy: &policy,
            content_id: crate::core::extent::ChunkId::of(c),
            bases: &[],
            dedup: None,
        };
        let f = SequenceEncoder
            .encode(c, &ctx)
            .into_iter()
            .map(|cand| cand.cost.persisted_bytes())
            .min()
            .unwrap_or(c.len() as u64);
        let d = SequenceDeepEncoder
            .encode(c, &ctx)
            .into_iter()
            .map(|cand| cand.cost.persisted_bytes())
            .min()
            .unwrap_or(c.len() as u64);
        fast_total += f;
        deep_total += d;
        if d < f {
            deep_wins += 1;
        }
        chunks += 1;
    }
    println!("\n==== Phase-9E: deep vs fast matcher on the src pack ====");
    println!(
        "chunks {chunks}  fast total {fast_total} ({:.3}x)  deep total {deep_total} ({:.3}x)  deep wins {deep_wins}",
        pack.len() as f64 / fast_total.max(1) as f64,
        pack.len() as f64 / deep_total.max(1) as f64
    );
}

/// Encode `b` against dictionary `dict` with the existing SequenceDict
/// encoder; returns the candidate's total persisted bytes if it wins
/// (marginal cost), else None.
fn encode_with_dict(b: &[u8], dict: &[u8], limits: &Limits, policy: &Policy) -> Option<u64> {
    let cid = crate::core::extent::ChunkId::of(b);
    let ctx = CandidateContext {
        limits,
        policy,
        content_id: cid,
        bases: &[],
        dedup: None,
    };
    let enc = SequenceDictEncoder {
        dictionary: crate::core::extent::ChunkId::of(dict),
        dict_bytes: dict.to_vec(),
        dict_depth: 0,
    };
    let cands = enc.encode(b, &ctx);
    cands.into_iter().map(|c| c.cost.persisted_bytes()).min()
}
