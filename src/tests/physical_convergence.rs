//! Phase-9H: physical convergence — the reconciliation of what the
//! derived object index accounts for versus what is actually on disk,
//! and the full-compaction path that converges them.
//!
//! Regression: write the real source tree, run the shared-dictionary and
//! model-bundle passes, GC, then FULL compaction; assert the backing
//! converges toward reachable bytes + bounded overhead; a second
//! compaction must be essentially idempotent; remount + fsck + byte
//! hashes must stay exact.

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
    Store::create(dir.path(), &cfg, [0x7b; 16]).unwrap()
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

/// The physical-reconciliation diagnostic (Phase-9H): exact breakdown of
/// every physical byte per category, per segment, independent of the
/// object index.
#[test]
fn print_physical_reconciliation_real_tree() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = source_tree_files(root).unwrap();
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let tree = write_tree(&store, &files);
    crate::optimizer::background::shared_dict_pass(&store, OptimizeOptions::default(), None)
        .unwrap();
    crate::optimizer::background::model_bundle_pass(&store, OptimizeOptions::default(), None)
        .unwrap();
    println!(
        "segments before GC: {:?}",
        crate::store::segment::list_segments(store.dir()).unwrap()
    );
    let live0 = crate::store::gc::mark_live(&store).unwrap();
    let ratios = crate::store::gc::physical_ratios(&store, &live0).unwrap();
    for (seq, (l, t)) in &ratios {
        println!(
            "  pre-GC seg {seq}: live {l} / {t} = {:.3}",
            *l as f64 / *t as f64
        );
    }
    let reclaimed = crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
    println!(
        "collect reclaimed {reclaimed}; segments now: {:?}",
        crate::store::segment::list_segments(store.dir()).unwrap()
    );
    let by_tag = crate::store::gc::unreachable_bytes_by_record_tag(&store).unwrap();
    println!("unreachable by tag post-GC: {by_tag:?}");
    verify_bytes(&store, &tree);

    let report = crate::store::physical::physical_report(&store).unwrap();
    println!("\n==== Phase-9H physical reconciliation (real tree) ====");
    println!(
        "logical {} B   reachable {} B",
        store.logical_bytes().unwrap(),
        reachable(&store)
    );
    println!("{}", crate::store::physical::render(&report));
    println!(
        "segments: {} ({} with index-hidden bytes, {} with dead indexed bytes)",
        report.segments.len(),
        report
            .segments
            .iter()
            .filter(|s| s.index_hidden_bytes > 0)
            .count(),
        report
            .segments
            .iter()
            .filter(|s| s.dead_indexed_bytes > 0)
            .count(),
    );
    for s in &report.segments {
        if s.index_hidden_bytes > 0 || s.dead_indexed_bytes > 0 {
            println!(
                "  seg {:>3}: file {:>10} live {:>10} dead_idx {:>10} hidden {:>10} unindexed {:>10} phys_ratio {:.2} idx_ratio {:.2}",
                s.seq,
                s.file_bytes,
                s.live_bytes,
                s.dead_indexed_bytes,
                s.index_hidden_bytes,
                s.unindexed_bytes,
                s.physical_live_ratio(),
                s.index_live_ratio(),
            );
        }
    }
}

/// The Phase-9H regression: physical backing must converge to the
/// reachable persistent state after the full pipeline (write → shared
/// dict → model bundles → GC → full compaction), and full compaction
/// must be idempotent.
#[test]
fn full_compaction_converges_real_tree() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = source_tree_files(root).unwrap();
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let tree = write_tree(&store, &files);
    crate::optimizer::background::shared_dict_pass(&store, OptimizeOptions::default(), None)
        .unwrap();
    crate::optimizer::background::model_bundle_pass(&store, OptimizeOptions::default(), None)
        .unwrap();
    verify_bytes(&store, &tree);

    let logical = store.logical_bytes().unwrap();
    let report_pre = crate::store::physical::physical_report(&store).unwrap();
    let backing_pre = report_pre.file_bytes;

    // Stage 1: threshold GC (physical victim selection).
    let reclaimed_gc = crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
    let backing_after_gc = crate::store::physical::physical_report(&store)
        .unwrap()
        .file_bytes;
    verify_bytes(&store, &tree);
    println!(
        "gc: reclaimed {reclaimed_gc} B; backing {} -> {} B",
        backing_pre, backing_after_gc
    );
    assert!(
        backing_after_gc < backing_pre,
        "threshold GC must reclaim physical garbage: {backing_after_gc} >= {backing_pre}"
    );

    // Stage 2: full compaction converges backing to reachable state.
    let reachable_before = reachable(&store);
    let reclaimed = crate::store::gc::compact_full(&store, &CrashHooks::none()).unwrap();
    let report_after = crate::store::physical::physical_report(&store).unwrap();
    let backing_after = report_after.file_bytes;
    let reachable_after = reachable(&store);
    verify_bytes(&store, &tree);

    println!(
        "compact: reclaimed {reclaimed} B; backing {} -> {} B; reachable {} -> {} B",
        backing_after_gc, backing_after, reachable_before, reachable_after
    );
    assert_eq!(
        reachable_before, reachable_after,
        "compaction must not change the reachable set"
    );
    assert!(
        backing_after <= backing_after_gc,
        "full compaction must not grow the backing store: {backing_after} > {backing_after_gc}"
    );
    let overhead = backing_after.saturating_sub(reachable_after);
    let logical_fraction = overhead as f64 / logical.max(1) as f64;
    println!(
        "post-compact overhead: {overhead} B = {:.2}% of logical ({logical} B)",
        100.0 * logical_fraction
    );
    assert!(
        overhead < 192 * 1024,
        "post-compaction backing must converge to reachable + bounded overhead: {overhead} B"
    );

    // Idempotence: a second full compaction must reclaim ~nothing.
    let report_second = crate::store::physical::physical_report(&store).unwrap();
    let reclaimed2 = crate::store::gc::compact_full(&store, &CrashHooks::none()).unwrap();
    let report_second2 = crate::store::physical::physical_report(&store).unwrap();
    println!(
        "second compact reclaimed {reclaimed2} B; backing {} -> {} B",
        report_second.file_bytes, report_second2.file_bytes
    );
    assert!(
        reclaimed2 < 4096,
        "second compaction must be essentially idempotent: reclaimed {reclaimed2}"
    );
    assert_eq!(
        report_second2.file_bytes, report_second.file_bytes,
        "backing must be stable after the second compaction"
    );
    assert_eq!(report_second2.unexplained(), 0);

    // Remount + fsck + byte-exactness.
    drop(store);
    let store2 = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    verify_bytes(&store2, &tree);
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

/// Snapshot roots are Root-tagged records: full compaction must preserve
/// them (the current-root copy-skip must not remove snapshot roots).
#[test]
fn compact_preserves_snapshot_roots() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let d = store
        .create_entry(
            store.current_root().root_dir_ino,
            b"docs",
            NewEntry::dir(0o755, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap();
    let ino = store
        .create_entry(
            d,
            b"v1.txt",
            NewEntry::file(0o644, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap();
    // A few structured chunks so the store has real data.
    let mut body = Vec::new();
    for i in 0..3000u64 {
        body.extend_from_slice(format!("record {i}: value {i:x}\n").as_bytes());
    }
    store.write_region(ino, 0, &body).unwrap();
    // Snapshot v1 (before the second version).
    let _snap1 = store.create_snapshot(b"v1", &CrashHooks::none()).unwrap();
    // Second version (the first version's records become garbage).
    let mut body2 = Vec::new();
    for i in 0..3000u64 {
        body2.extend_from_slice(format!("entry {i}: value {:x}\n", i * 7).as_bytes());
    }
    store.write_region(ino, 0, &body2).unwrap();
    // Full compaction.
    crate::store::gc::compact_full(&store, &CrashHooks::none()).unwrap();
    // The snapshot must still restore byte-exactly.
    store
        .restore_snapshot(b"v1", &CrashHooks::none())
        .expect("snapshot must survive compaction");
    let back = store.read_file(ino, 0, body.len() as u64).unwrap();
    assert_eq!(back, body, "snapshot restore must be byte-exact");
    // Remount: snapshot list still present, fsck clean.
    drop(store);
    let store2 = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    assert!(
        store2
            .list_snapshots()
            .unwrap()
            .iter()
            .any(|(name, _)| name == b"v1")
    );
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}
