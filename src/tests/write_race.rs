//! Write-path race regression court: concurrent small-file epoch writes
//! with interleaved checkpointing (the Phase-11E mount-court corruption).
//!
//! # The bug this pins
//!
//! The mounted-FUSE 11E court (`tools/court-worker-pool-mount.sh`) found a
//! real corruption: with `--threads >= 2`, parallel `tar -xf` extractions of
//! ~600-byte files lost ~10-45% of files' EXTENTS — the inode size survived
//! but the committed extent tree was empty (silent zero reads). Root cause,
//! in two parts:
//!
//! 1. **Stale-root commit (the data loss).** The epoch never rebuilds
//!    extent/directory trees — the CHECKPOINT does, and only for the
//!    files/dirs whose extents/entries are in ITS frozen snapshot. A
//!    pending inode re-staged by a concurrent op (a write's block-B
//!    re-read, a setattr) carries a stale (usually ZERO) root and can
//!    survive the checkpoint's compare-and-remove; the NEXT checkpoint
//!    committed it, orphaning the committed tree. Fixed in
//!    `Store::epoch_checkpoint` step 3.5: never commit a data root this
//!    checkpoint did not rebuild — resolve it from the committed inode.
//! 2. **The getxattr checkpoint storm (the amplifier).** `get_xattr` /
//!    `list_xattr` flushed the epoch on every call; the kernel probes
//!    security.capability / ACL xattrs on every file creation, so a
//!    parallel untar fired hundreds of full checkpoints, each widening the
//!    stale-root race window. Fixed: xattr reads are committed-side reads
//!    (xattrs are committed immediately) with an overlay existence check
//!    only.
//!
//! This court reproduces the race at the STORE level (no FUSE): writers +
//! a setattr-er (which re-stages pending inodes with stale roots) + an
//! explicit checkpointer, all concurrent, then a reopen+replay read-back —
//! the definitive check, because a checkpoint that consumes an envelope's
//! sequence without merging its extent is SILENT (replay cannot restore
//! it).
//!
//! The court deliberately runs WITHOUT FUSE. It passes only when the
//! checkpoint never commits a stale root.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::optimizer::foreground::ForegroundPolicy;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{AttrUpdate, NewEntry, Store, StoreConfig};

fn create_store(dir: &tempfile::TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x44; 16]).unwrap()
}

/// Deterministic per-file bytes (distinct content per file — the probe's
/// per-write-distinct discipline, so nothing dedups).
fn file_bytes(i: u64) -> Vec<u8> {
    let body = blake3::hash(&i.to_le_bytes());
    let mut out = Vec::with_capacity(599);
    out.extend_from_slice(b"# generated config\nhost = node-");
    out.extend_from_slice(i.to_string().as_bytes());
    out.extend_from_slice(b"\nport = 8000\nuser = svc\npayload = ");
    out.extend_from_slice(&body.as_bytes()[..32]);
    out.resize(599, 0x5A);
    out
}

/// Run the court: `files` files written by `writers` threads concurrently
/// with a setattr-er (re-staging pending inodes) and a checkpointer.
/// Returns the number of files whose POST-REOPEN read-back differs from
/// what was written (0 = clean). The reopen replays the mutation log, so
/// any extent the checkpoints consumed without merging shows up here as a
/// zero-filled read.
fn run_court(
    dir: &tempfile::TempDir,
    files: u64,
    writers: usize,
    setattr_iters: usize,
    checkpoint_iters: usize,
) -> usize {
    let store = Arc::new(create_store(dir));
    let root = store
        .create_entry(
            1,
            b"root",
            NewEntry::dir(0o755, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap();
    let inos: Vec<u64> = (0..files)
        .map(|i| {
            store
                .create_entry(
                    root,
                    format!("f{i:04}.cfg").as_bytes(),
                    NewEntry::file(0o644, 1000, 1000),
                    &CrashHooks::none(),
                )
                .unwrap()
        })
        .collect();
    let refs: Vec<Vec<u8>> = (0..files).map(file_bytes).collect();

    let stop = Arc::new(AtomicBool::new(false));
    let counter = AtomicUsize::new(0);
    let store_w = Arc::clone(&store);
    let inos_a = Arc::new(inos);
    let refs_a = Arc::new(refs);
    std::thread::scope(|s| {
        // Writers: each file gets one small write (the tar pattern).
        for _ in 0..writers {
            let store = Arc::clone(&store_w);
            let inos = Arc::clone(&inos_a);
            let refs = Arc::clone(&refs_a);
            let counter = &counter;
            s.spawn(move || {
                let opts = OptimizeOptions::default();
                let fg = ForegroundPolicy::full();
                loop {
                    let i = counter.fetch_add(1, Ordering::Relaxed);
                    if i >= files as usize {
                        break;
                    }
                    store
                        .epoch_write(inos[i], 0, &refs[i], opts, fg, &CrashHooks::none())
                        .unwrap_or_else(|e| panic!("file {i}: epoch_write failed: {e}"));
                }
            });
        }
        // Setattr-er: re-stages pending inodes (the stale-root vector —
        // the re-read copies the pending inode, which never carries a
        // rebuilt tree).
        let store = Arc::clone(&store_w);
        let inos = Arc::clone(&inos_a);
        let stop_s = Arc::clone(&stop);
        s.spawn(move || {
            let mut i = 0usize;
            while !stop_s.load(Ordering::Relaxed) && i < setattr_iters {
                let ino = inos[i % inos.len()];
                let mode = Some(0o644 + (i % 2) as u32);
                store
                    .epoch_setattr(
                        ino,
                        &AttrUpdate {
                            mode,
                            ..Default::default()
                        },
                        &CrashHooks::none(),
                    )
                    .unwrap_or_else(|e| panic!("setattr: {e}"));
                i += 1;
            }
        });
        // Checkpointer: the stale-root commit vector (the getxattr-storm
        // stand-in).
        let store = Arc::clone(&store_w);
        let stop_c = Arc::clone(&stop);
        s.spawn(move || {
            let mut i = 0usize;
            while !stop_c.load(Ordering::Relaxed) && i < checkpoint_iters {
                store
                    .epoch_checkpoint(&CrashHooks::none())
                    .unwrap_or_else(|e| panic!("checkpoint: {e}"));
                i += 1;
            }
        });
        // Let the workers finish, then stop the auxiliary threads.
        while counter.load(Ordering::Relaxed) < files as usize {
            std::thread::yield_now();
        }
        stop.store(true, Ordering::Relaxed);
    });

    // Reopen + replay: the committed tree must hold every extent (a
    // checkpoint that consumed an envelope without merging its extent is
    // unrecoverable here — that is exactly the corruption this pins).
    // The flock guard: EVERY Arc<Store> clone must be dropped first — the
    // lock file is held for the Store's lifetime, and `Store::open` waits
    // for it.
    drop(store);
    drop(store_w);
    let reopened = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    let mut bad = 0;
    for i in 0..files as usize {
        let got = reopened.read_file(inos_a[i], 0, 599).unwrap();
        if got != refs_a[i] {
            bad += 1;
        }
    }
    bad
}

#[test]
fn concurrent_writes_with_checkpointing_stay_byte_exact() {
    let dir = tempfile::TempDir::new().unwrap();
    let bad = run_court(&dir, 64, 4, 256, 64);
    assert_eq!(
        bad, 0,
        "concurrent writes + setattr + checkpoint lost extents (read-back mismatches: {bad})"
    );
}

#[test]
fn concurrent_writes_without_setattr_stay_byte_exact() {
    let dir = tempfile::TempDir::new().unwrap();
    let bad = run_court(&dir, 64, 4, 0, 64);
    assert_eq!(
        bad, 0,
        "concurrent writes + checkpoint lost extents (read-back mismatches: {bad})"
    );
}

/// Phase 12E.3/12E.1 regression: the rename-replay stale-root vector.
///
/// # The bug this pins
///
/// The Phase-11E stale-root rule was applied to the Setattr and Unlink
/// replay arms but NOT the Rename arm: `replay_op`'s Rename arm put the
/// log-staged moved inode wholesale, overwriting the extent_root that an
/// EARLIER Write replay in the same transaction had just built with
/// `put_extent_in_tx` — orphaning every extent (silent zero reads after
/// reopen). The Phase-12E engine's write-then-rename blob protocol
/// (create + write + rename in one epoch, close without a barrier,
/// reopen) exposed it deterministically — the same shape as tar's
/// extract-to-temp-then-rename, which is why a mounted 11E court could
/// have seen it too.
///
/// The replay arm now applies the staged metadata while PRESERVING the
/// transaction's current data root (the same rule the Setattr/Unlink
/// arms enforce).
#[test]
fn write_then_rename_in_one_epoch_survives_reopen() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = StoreConfig {
        segment_size: 1024 * 1024,
        ..Default::default()
    };
    let store = Store::create(dir.path(), &cfg, [0x55; 16]).unwrap();
    let hooks = CrashHooks::none();
    let ino = store
        .epoch_create(1, b"tmp", NewEntry::file(0o600, 1000, 1000), &hooks)
        .unwrap();
    store
        .epoch_write(
            ino,
            0,
            b"log-only blob",
            OptimizeOptions::default(),
            ForegroundPolicy::full(),
            &hooks,
        )
        .unwrap();
    store.epoch_rename(1, b"tmp", 1, b"final", &hooks).unwrap();
    // Close WITHOUT a barrier: the ops stay in the mutation log (the
    // durability barrier is what checkpoints). Reopen must replay them.
    drop(store);

    let reopened = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    let out = reopened.read_file(ino, 0, 64).unwrap();
    assert_eq!(
        out, b"log-only blob",
        "rename replay clobbered the moved file's extent root"
    );
    // The final name resolves to the same inode.
    let entry = reopened
        .dir_lookup(1, b"final")
        .unwrap()
        .expect("final entry exists");
    assert_eq!(entry.ino, ino);
}
