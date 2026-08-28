//! Phase 12C-1-3: the over-depth chain repair + write-path base-channel
//! regression pins (mounted-court-found).
//!
//! # THE BUGS THIS PINS
//!
//! The 12C-1-3 mounted engagement probe (16 writers x 1 MiB structured
//! writes, pressure policy, pool-16) produced stores whose background
//! settle CRASHED with `DepthExceeded { depth: 5, max: 4 }`, and a
//! post-mortem scan found LIVE file extents whose reference chains
//! exceeded the decode cap — user-visible unreadable regions. Three
//! defects combined:
//!
//! 1. **The crash-before-sweep ordering**: `optimize_pass` ran its
//!    over-depth repair sweep (Stage 5) only AFTER the per-extent search,
//!    but the search's base channels call `base_chunk_at` -> `read_file`,
//!    and an over-depth base's read failed with `DepthExceeded`, aborting
//!    the WHOLE pass before the sweep could run. An unreadable base must
//!    mean "no base", never a crash.
//! 2. **No pre-pass recovery**: a store already carrying over-depth
//!    chains (from the writes' own chain-deepening or a prior crashed
//!    pass) was never repaired before the search read it — the sweep
//!    moved to the FRONT of the pass (the pre-pass rebase) so repair
//!    happens before anything reads the chains.
//! 3. **Overlay-only inodes**: the same `base_chunk_at` crashed with
//!    `Invariant("inode N missing")` when the write path searched a
//!    freshly-created inode that exists only in the epoch overlay (the
//!    committed-state lookup finds nothing). No committed data = no base.
//!
//! # THE FIXES
//!
//! - `base_chunk_at` treats a read failure (over-depth chain, torn
//!   record) and a committed-inode miss as "no base" (`Ok(None)`).
//! - `optimize_pass` runs the repair sweep BEFORE the per-extent search
//!   (pre-pass rebase) and keeps the end-of-pass sweep for the
//!   deepenings the pass itself introduces.
//! - `rebase_overdepth_extents` detects with `chain_depth_uncapped`
//!   (hardening: the capped walk's push-before-prune happens to report
//!   the over-depth value today, but the uncapped walk makes the
//!   detection explicit rather than incidental).

#![forbid(unsafe_code)]

use std::sync::Arc;

use tempfile::TempDir;

use crate::optimizer::foreground::ForegroundPolicy;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};

fn create_store(dir: &TempDir) -> Arc<Store> {
    Arc::new(Store::create(dir.path(), &StoreConfig::default(), [0x81; 16]).unwrap())
}

/// Build a real store containing a LIVE extent whose reference chain is
/// depth 5 (> max_reference_depth 4):
///
/// ```text
/// E  = BaseResidual{ base: C1, xor at 0 }  -> B0   (the file's extent)
/// C1 = BaseResidual{ base: C2, xor at 0 }  -> B1
/// C2 = BaseResidual{ base: C3, xor at 0 }  -> B2
/// C3 = BaseResidual{ base: C4, xor at 0 }  -> B3
/// C4 = BaseResidual{ base: C5, xor at 0 }  -> B4
/// C5 = Raw                                  -> B5   (terminal)
/// ```
///
/// Each level's bytes are the previous XOR a distinct byte, so every
/// content id is distinct (the reference chains resolve through the chunk
/// index at materialize time — exactly the shape the mounted court's
/// settle left behind).
fn stage_overdepth_chain(store: &Arc<Store>) -> u64 {
    let hooks = &CrashHooks::none();
    let root = store.current_root().root_dir_ino;
    let dir_ino = store
        .epoch_create(root, b"d", NewEntry::dir(0o755, 1000, 1000), hooks)
        .unwrap();
    let file_ino = store
        .epoch_create(dir_ino, b"f", NewEntry::file(0o644, 1000, 1000), hooks)
        .unwrap();
    // The direct `commit_file_extents` API reads the COMMITTED inode:
    // flush + checkpoint the create so the fixture's staging is visible.
    store.ensure_epoch_flushed(hooks).unwrap();
    store.epoch_checkpoint(hooks).unwrap();

    // Terminal: B5 as a RAW object.
    let b5: Vec<u8> = (0..64u8)
        .map(|i| b"abcdefghijklmnopqrstuvwxyz012345"[i as usize % 32])
        .collect();
    let c5 = crate::core::extent::ChunkId::of(&b5);
    let raw5 = crate::core::representation::Representation::Raw {
        obj: c5,
        len: b5.len() as u64,
    };
    store
        .commit_file_extents(
            file_ino,
            vec![crate::store::ExtentUpdate {
                offset: 0,
                descriptor: raw5,
                content_id: c5,
                objects: vec![crate::core::candidate::ObjectRecord::data(b5.clone())],
            }],
            None,
            hooks,
        )
        .unwrap();

    // Reference levels C4..C1, each a BaseResidual on the previous with a
    // distinct XOR edit (distinct bytes per level -> distinct content ids).
    let mut prev_id = c5;
    let mut prev_bytes = b5;
    let xors = [0xABu8, 0xCD, 0xEF, 0x12];
    let mut ids = Vec::new();
    for x in xors.iter() {
        let cur_bytes: Vec<u8> = prev_bytes
            .iter()
            .enumerate()
            .map(|(j, b)| if j == 0 { b ^ x } else { *b })
            .collect();
        let cur_id = crate::core::extent::ChunkId::of(&cur_bytes);
        let desc = crate::core::representation::Representation::BaseResidual {
            base: prev_id,
            base_len: prev_bytes.len() as u64,
            residual: crate::core::representation::Residual::XorSparse {
                len: prev_bytes.len() as u64,
                edits: vec![crate::core::representation::Edit { pos: 0, val: *x }],
            },
            len: cur_bytes.len() as u64,
        };
        store
            .commit_file_extents(
                file_ino,
                vec![crate::store::ExtentUpdate {
                    offset: 0,
                    descriptor: desc,
                    content_id: cur_id,
                    objects: Vec::new(),
                }],
                None,
                hooks,
            )
            .unwrap();
        ids.push(cur_id);
        prev_id = cur_id;
        prev_bytes = cur_bytes;
    }
    // The file extent E (depth 5: E -> C1 -> C2 -> C3 -> C4 -> C5).
    let e_bytes: Vec<u8> = prev_bytes
        .iter()
        .enumerate()
        .map(|(j, b)| if j == 0 { b ^ 0x56 } else { *b })
        .collect();
    let e_cid = crate::core::extent::ChunkId::of(&e_bytes);
    let e_desc = crate::core::representation::Representation::BaseResidual {
        base: prev_id,
        base_len: prev_bytes.len() as u64,
        residual: crate::core::representation::Residual::XorSparse {
            len: prev_bytes.len() as u64,
            edits: vec![crate::core::representation::Edit { pos: 0, val: 0x56 }],
        },
        len: e_bytes.len() as u64,
    };
    store
        .commit_file_extents(
            file_ino,
            vec![crate::store::ExtentUpdate {
                offset: 0,
                descriptor: e_desc,
                content_id: e_cid,
                objects: Vec::new(),
            }],
            None,
            hooks,
        )
        .unwrap();
    store.ensure_epoch_flushed(hooks).unwrap();
    store.epoch_checkpoint(hooks).unwrap();
    file_ino
}

#[test]
fn overdepth_chain_is_detected_and_repaired() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let file_ino = stage_overdepth_chain(&store);
    let limits = *store.limits();

    // The chain is genuinely over-depth: strict materialization fails,
    // the uncapped walk reports 5 (> max 4).
    let extent = store
        .extent_descriptor(file_ino, 0)
        .unwrap()
        .expect("extent exists");
    let desc = crate::format::descriptor::decode(&extent, &limits).unwrap();
    assert!(
        crate::core::materialize::materialize_to_vec(&desc, &*store, &limits).is_err(),
        "the staged chain must exceed the decode cap"
    );
    assert_eq!(
        crate::optimizer::rebase::chain_depth_uncapped(&store, &desc, &Default::default()),
        5,
        "the uncapped walk must see the over-depth chain"
    );
    // The repair sweep detects + flattens the extent (the live data
    // becomes readable again).
    let rebased = store.rebase_overdepth_extents(&CrashHooks::none()).unwrap();
    assert!(rebased >= 1, "the over-depth extent must be rebased");
    let after = store
        .extent_descriptor(file_ino, 0)
        .unwrap()
        .expect("extent still exists");
    let after_desc = crate::format::descriptor::decode(&after, &limits).unwrap();
    assert_eq!(
        crate::optimizer::rebase::depth_of(&after_desc),
        0,
        "the repaired extent must be terminal (depth 0)"
    );
    let bytes = crate::core::materialize::materialize_to_vec(&after_desc, &*store, &limits)
        .expect("repaired extent materializes");
    assert_eq!(bytes.len(), 64);

    // A second repair sweep is a no-op (idempotent).
    let again = store.rebase_overdepth_extents(&CrashHooks::none()).unwrap();
    assert_eq!(again, 0, "the repair must be idempotent");
}

#[test]
fn optimize_pass_survives_and_repairs_overdepth_store() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    stage_overdepth_chain(&store);

    // The full optimize pass must (a) repair the over-depth chain via the
    // pre-pass rebase and (b) complete without crashing (the pre-fix code
    // aborted on the base-channel read of the over-depth chain).
    let stats =
        crate::optimizer::background::optimize_pass(&store, OptimizeOptions::default(), None, None)
            .expect("optimize_pass must not crash on an over-depth store");
    assert!(stats.errors == 0, "no extent may error: {stats:?}");

    // After the pass, every live extent materializes under the strict cap.
    let limits = *store.limits();
    let inos = store.all_inodes().unwrap();
    let mut live_checked = 0u64;
    for ino in inos {
        let Some(inode) = store.get_inode(ino).unwrap() else {
            continue;
        };
        if !inode.is_file() {
            continue;
        }
        let extent_root = match &inode.data {
            crate::store::inode::InodeData::File { extent_root } => *extent_root,
            _ => continue,
        };
        if extent_root.is_zero() {
            continue;
        }
        let entries = crate::store::extent_tree::scan_all(
            extent_root,
            crate::store::BTREE_ORDER,
            limits.max_fanout,
            &*store,
        )
        .unwrap();
        for (_start, desc_bytes) in entries {
            live_checked += 1;
            let desc = crate::format::descriptor::decode(&desc_bytes, &limits).unwrap();
            assert!(
                crate::core::materialize::materialize_to_vec(&desc, &*store, &limits).is_ok(),
                "every live extent must be readable after the repair"
            );
        }
    }
    assert!(live_checked >= 1);
}

#[test]
fn write_path_base_channels_tolerate_overlay_only_inodes() {
    // The mounted-court-found write-path crash: with a policy/content mix
    // whose DSFB trust admits the base channels, a write to a freshly-
    // created (overlay-only, uncommitted) inode called `base_chunk_at` ->
    // the committed-only inode lookup -> `Invariant("inode N missing")` ->
    // the whole write failed. Both the pool and the semaphore paths must
    // tolerate it (no committed data = no base). The 1 MiB sequential
    // appends also exercise the in-batch dictionary + store-dictionary
    // paths against the overlay.
    for pool in [false, true] {
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir);
        // The pool is process-global and the pool tests reconfigure it:
        // hold POOL_LOCK for the WHOLE pool-path run (enable -> writes ->
        // disable), not just the enable block — a concurrent test's
        // disable/rebind would otherwise unbind the pool mid-run and the
        // writes' `store_arc()` would panic ("11E pool bound to a store").
        let _pool_guard = if pool {
            Some(
                crate::store::workers::tests::POOL_LOCK
                    .lock()
                    .expect("poisoned"),
            )
        } else {
            None
        };
        if pool {
            crate::store::workers::POOL.enable(16, 8);
            crate::store::workers::POOL.bind(&store);
            store.enable_worker_pool();
        }
        let hooks = &CrashHooks::none();
        let fg = ForegroundPolicy::full();
        let opts = OptimizeOptions::default();
        let root = store.current_root().root_dir_ino;
        let dir_ino = store
            .epoch_create(root, b"d", NewEntry::dir(0o755, 1000, 1000), hooks)
            .unwrap();
        let ino = store
            .epoch_create(dir_ino, b"f", NewEntry::file(0o644, 1000, 1000), hooks)
            .unwrap();
        for r in 0..6u64 {
            let mut b = Vec::with_capacity(1024 * 1024);
            let mut x: u64 = r * 0x9E3779B97F4A7C15 + 1;
            while b.len() < 1024 * 1024 {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                b.extend_from_slice(
                    format!(
                        "f-{r} line={} key={:04x} val={:06x} name=obj-{}\n",
                        b.len(),
                        x & 0xffff,
                        (x >> 16) & 0xffffff,
                        (x >> 20) % 10000,
                    )
                    .as_bytes(),
                );
            }
            store
                .epoch_write_semantic(ino, r * 1024 * 1024, &b, opts, fg, None, hooks)
                .expect("the write must tolerate the overlay-only inode");
        }
        store.ensure_epoch_flushed(hooks).unwrap();
        store.epoch_checkpoint(hooks).unwrap();
        // Every live extent is readable (the depth scan).
        let limits = *store.limits();
        let inode = store.get_inode(ino).unwrap().unwrap();
        let extent_root = match &inode.data {
            crate::store::inode::InodeData::File { extent_root } => *extent_root,
            _ => unreachable!(),
        };
        let entries = crate::store::extent_tree::scan_all(
            extent_root,
            crate::store::BTREE_ORDER,
            limits.max_fanout,
            &*store,
        )
        .unwrap();
        assert!(!entries.is_empty());
        for (_start, desc_bytes) in entries {
            let desc = crate::format::descriptor::decode(&desc_bytes, &limits).unwrap();
            assert!(
                crate::core::materialize::materialize_to_vec(&desc, &*store, &limits).is_ok(),
                "no over-depth chain from the sequential-append pattern"
            );
        }
        if pool {
            crate::store::workers::POOL.disable();
        }
    }
}
