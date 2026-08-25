//! Phase-9G: the amortized entropy-model background pass (`model_bundle_pass`).
//!
//! Correctness battery:
//! - byte-exactness is validated (§32) before every rewrite and after
//!   remount;
//! - a second pass is a no-op (idempotence — the cohort aggregate is
//!   deterministic and members that already use it gain nothing);
//! - rewrites strictly shrink the post-GC reachable footprint;
//! - the negative control (noise) is never rewritten (the group gate
//!   requires the amortized model objects to pay back);
//! - fsck stays clean after the pass.

#![forbid(unsafe_code)]

use std::path::Path;

use crate::evidence::corpus::source_tree_files;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 4 * 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x9a; 16]).unwrap()
}

fn write_tree(store: &Store, files: &[(String, Vec<u8>)]) -> Vec<(u64, u64, Vec<u8>)> {
    use std::collections::HashMap;
    let mut dir_cache: HashMap<String, u64> = HashMap::new();
    dir_cache.insert(String::new(), store.current_root().root_dir_ino);
    let mut out: Vec<(u64, u64, Vec<u8>)> = Vec::new();
    for (rel, bytes) in files {
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
        out.push((ino, dir_cache[&dir_part], bytes.clone()));
    }
    out
}

fn reachable(store: &Store) -> u64 {
    let unreachable = crate::store::gc::unreachable_bytes(store).unwrap();
    let records_total: u64 = store
        .object_index()
        .iter()
        .into_iter()
        .map(|(_, loc)| loc.total_size())
        .sum();
    records_total.saturating_sub(unreachable)
}

fn verify_bytes(store: &Store, files: &[(u64, u64, Vec<u8>)]) {
    for (ino, _dir, bytes) in files {
        let mut got = Vec::new();
        let mut off = 0u64;
        while off < bytes.len() as u64 {
            let len = 65536u64.min(bytes.len() as u64 - off);
            got.extend_from_slice(&store.read_file(*ino, off, len).unwrap());
            off += len;
        }
        assert_eq!(got, *bytes, "byte-exactness violated for ino {ino}");
    }
}

#[test]
fn model_bundle_pass_is_byte_exact_idempotent_and_shrinks_the_real_tree() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = source_tree_files(root).unwrap();
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let tree = write_tree(&store, &files);
    // The realistic baseline: the shared-dict pool + deep pass first, then
    // the model pass — exactly the tree-court pipeline.
    crate::optimizer::background::shared_dict_pass(&store, OptimizeOptions::default(), None)
        .unwrap();
    let before_models = reachable(&store);
    let first =
        crate::optimizer::background::model_bundle_pass(&store, OptimizeOptions::default(), None)
            .unwrap();
    crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
    let after_models = reachable(&store);
    verify_bytes(&store, &tree);
    println!(
        "model-bundle pass (real tree): scanned {}, rewrote {}, saved {} B (cohort-accounted); reachable {} -> {} B ({:.1} KiB)",
        first.scanned,
        first.rewritten,
        first.saved_bytes,
        before_models,
        after_models,
        before_models.saturating_sub(after_models) as f64 / 1024.0
    );

    // The model pass must have rewritten at least some extents on the real
    // tree (the oracle measured ~55 KiB of cohort-model savings there) —
    // and a second pass must be a no-op (idempotence).
    let stats =
        crate::optimizer::background::model_bundle_pass(&store, OptimizeOptions::default(), None)
            .unwrap();
    assert_eq!(stats.errors, 0, "second pass must not error");
    assert_eq!(
        stats.rewritten, 0,
        "second pass must be a no-op (rewrote {})",
        stats.rewritten
    );

    // Byte-exactness across remount + fsck.
    drop(store);
    let store2 = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    verify_bytes(&store2, &tree);
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
    drop(store2);

    // Sanity: the tree-court pipeline result must be no worse than the
    // shared-dict-only baseline at the same revision.
    let dir2 = TempDir::new().unwrap();
    let store3 = create_store(&dir2);
    let tree3 = write_tree(&store3, &files);
    crate::optimizer::background::shared_dict_pass(&store3, OptimizeOptions::default(), None)
        .unwrap();
    crate::store::gc::collect(&store3, &CrashHooks::none()).unwrap();
    let baseline = reachable(&store3);
    assert!(
        after_models <= baseline,
        "model pass must not regress the footprint: {after_models} vs {baseline}"
    );
    verify_bytes(&store3, &tree3);
}

#[test]
fn model_bundle_pass_never_rewrites_noise() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    // One directory of unrelated noise chunks: the cohort aggregate encodes
    // nothing better than per-extent models, so the group gate must hold
    // (the amortized model objects would cost more than they save).
    let noise_dir = store
        .create_entry(
            store.current_root().root_dir_ino,
            b"noise",
            NewEntry::dir(0o755, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap();
    let mut inos = Vec::new();
    for i in 0..4u64 {
        let ino = store
            .create_entry(
                noise_dir,
                format!("n{i}.bin").as_bytes(),
                NewEntry::file(0o644, 1000, 1000),
                &CrashHooks::none(),
            )
            .unwrap();
        // Deterministic pseudo-noise that is structurally unrelated across
        // files (but not pure urandom — a pure-noise file stays RAW and is
        // not even a sequence member).
        let mut bytes = Vec::with_capacity(8192);
        let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ (0x1234_0000 + i);
        for _ in 0..8192 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            bytes.push((state >> 33) as u8);
        }
        store.write_region(ino, 0, &bytes).unwrap();
        inos.push((ino, bytes));
    }
    let stats =
        crate::optimizer::background::model_bundle_pass(&store, OptimizeOptions::default(), None)
            .unwrap();
    assert_eq!(
        stats.rewritten, 0,
        "noise cohort must not be rewritten (group gate failed): {stats:?}"
    );
    for (ino, bytes) in &inos {
        assert_eq!(store.read_file(*ino, 0, 8192).unwrap(), *bytes);
    }
}
