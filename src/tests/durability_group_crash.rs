//! Phase-12B crash court: the GROUP durability barrier under a simulated
//! crash at every barrier stage.
//!
//! The 12B brief's crash oracle:
//!
//! ```text
//! if fsync returned:        its required sequence is recoverable after
//!                           simulated power loss
//! if fsync had not returned: either state is admissible depending on the
//!                           barrier cut
//! ```
//!
//! This court drives CONCURRENT writers + fsyncs through the 12B group
//! gate while one thread's barrier is armed to crash at each of the five
//! physical-barrier stages, then reopens the store (recovery) and
//! verifies that every fsync whose barrier COMPLETED reads back
//! byte-exactly. The crash barrier's own writes and every un-returned
//! fsync are admissible either way — never asserted.
//!
//! # Why a plain reopen (no truncation) is the right oracle here
//!
//! The pre-12B courts truncate the segment to the last barrier size to
//! simulate page-cache loss. With the group, concurrent generations make
//! "the last barrier size" racy, and the property under test is
//! different: the group must never return Ok from an fsync whose physical
//! barrier did not complete. The reopen-with-recovery state is a
//! SUPERSET of every completed barrier's on-disk effect, so asserting the
//! returned set against it is sound, and the crash points' partial
//! effects (e.g. superblock written but not fsynced) are exactly the
//! states the recovery fallback must handle. The page-cache-loss
//! truncation semantics remain pinned by the existing `durability` /
//! `crash_recovery` courts, which run unmodified against the group gate.
//!
//! # The crash arming
//!
//! One dedicated thread writes unique content then calls
//! `durability_barrier` with a single armed crash point, looping until
//! the crash fires (`Err(CrashSimulated)`); every OTHER thread writes and
//! fsyncs unarmed, stopping on the first error. All threads stop once
//! `crashed` is set. Every Ok-returned fsync's (file, bytes) are
//! recorded; after the crash every store `Arc` is dropped (the flock!)
//! and the store is reopened with recovery; each recorded write must read
//! back exactly, and fsck must be clean.

#![forbid(unsafe_code)]

/// (ino, offset, bytes) of every fsync that returned Ok — the brief's
/// "returned -> recoverable" oracle ledger, shared across the writer
/// threads. Aliased for the clippy type-complexity gate.
type DurableLedger = std::sync::Arc<std::sync::Mutex<Vec<(u64, u64, Vec<u8>)>>>;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::{CrashHooks, CrashPoint};
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

fn create_store(dir: &TempDir) -> Arc<Store> {
    let cfg = StoreConfig {
        segment_size: 128 * 1024 * 1024,
        ..Default::default()
    };
    Arc::new(Store::create(dir.path(), &cfg, [0x55; 16]).unwrap())
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

fn stream_for(writer: u64, cycle: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(65536);
    let mut state = 0x12b_c0deu64;
    state ^= writer.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    state ^= cycle.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    for _ in 0..65536 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push((state >> 33) as u8);
    }
    out
}

fn run_one(point: CrashPoint) {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let fg = store.foreground_policy();
    let opts = OptimizeOptions::default();
    let crashed = Arc::new(AtomicBool::new(false));
    // (ino, offset, bytes) of every fsync that RETURNED Ok — the brief's
    // "returned -> recoverable" oracle. Each write goes to a UNIQUE offset
    // (cycle * chunk), so recorded writes are never superseded by a later
    // write at the same position and the post-recovery read is exact.
    let durable: DurableLedger = Arc::new(Mutex::new(Vec::new()));
    let mut inos = Vec::new();
    for w in 0..8u64 {
        inos.push(create_file(&store, &format!("f{w}")));
    }
    let crash_ino = create_file(&store, "crash");

    std::thread::scope(|s| {
        for w in 0..8u64 {
            let store = Arc::clone(&store);
            let crashed = Arc::clone(&crashed);
            let durable = Arc::clone(&durable);
            let inos = &inos;
            s.spawn(move || {
                let mut cycle = 0u64;
                while !crashed.load(Ordering::Relaxed) && cycle < 12 {
                    let data = stream_for(w, cycle);
                    store
                        .epoch_write(
                            inos[w as usize],
                            cycle * 65536,
                            &data,
                            opts,
                            fg,
                            &CrashHooks::none(),
                        )
                        .unwrap();
                    match store.durability_barrier(&CrashHooks::none()) {
                        Ok(()) => {
                            durable
                                .lock()
                                .unwrap()
                                .push((inos[w as usize], cycle * 65536, data))
                        }
                        Err(_) => {
                            crashed.store(true, Ordering::Relaxed);
                            break;
                        }
                    }
                    cycle += 1;
                }
            });
        }
        // The crash thread: unique content + an armed barrier, looped
        // until the injection fires.
        let store = Arc::clone(&store);
        let crashed = Arc::clone(&crashed);
        let durable = Arc::clone(&durable);
        s.spawn(move || {
            let mut cycle = 0u64;
            let t0 = Instant::now();
            while !crashed.load(Ordering::Relaxed) {
                let data = stream_for(999, cycle);
                store
                    .epoch_write(
                        crash_ino,
                        cycle * 65536,
                        &data,
                        opts,
                        fg,
                        &CrashHooks::none(),
                    )
                    .unwrap();
                match store.durability_barrier(&CrashHooks::crash_at(point)) {
                    Ok(()) => {
                        durable
                            .lock()
                            .unwrap()
                            .push((crash_ino, cycle * 65536, data));
                    }
                    Err(_) => {
                        crashed.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                cycle += 1;
                assert!(
                    t0.elapsed().as_secs() < 30,
                    "crash point {point:?} never fired"
                );
            }
        });
    });
    assert!(
        crashed.load(Ordering::Relaxed),
        "crash point {point:?} must fire"
    );

    // Drop EVERY store Arc (the flock is held for the store's lifetime —
    // a reopen while any Arc lives blocks forever), then reopen with
    // recovery.
    drop(store);
    let cfg = StoreConfig {
        segment_size: 128 * 1024 * 1024,
        ..Default::default()
    };
    let store2 = Store::open(dir.path(), &cfg).unwrap();
    let durable = durable.lock().unwrap();
    assert!(
        !durable.is_empty(),
        "crash point {point:?}: at least one fsync must have returned Ok before the crash"
    );
    // Every Ok-returned write sits at its own offset (never superseded);
    // each must read back exactly after recovery. The un-returned writes
    // (the crashed cycle's, at their own offsets) are admissible either
    // way — never asserted.
    let mut missing: Vec<(u64, u64)> = Vec::new();
    for (ino, off, data) in durable.iter() {
        let got = store2.read_file(*ino, *off, data.len() as u64).unwrap();
        if &got != data {
            missing.push((*ino, *off));
        }
    }
    assert!(
        missing.is_empty(),
        "crash point {point:?}: {} RETURNED fsyncs missing after recovery: {missing:?} of {} recorded",
        missing.len(),
        durable.len()
    );
    drop(store2);
    // The reopened store must be fsck-clean (the group never publishes a
    // torn state).
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(
        report.is_clean(),
        "crash point {point:?}: fsck must be clean after recovery ({})",
        report.error_count()
    );
}

#[test]
fn group_barrier_crash_at_every_stage_keeps_returned_fsyncs() {
    // The five physical-barrier stages (AfterRootWrite belongs to the tx
    // commit path, not the barrier).
    for point in [
        CrashPoint::AfterRecordAppend,
        CrashPoint::AfterSegmentFdatasync,
        CrashPoint::AfterSegmentDirFsync,
        CrashPoint::AfterSuperblockWrite,
        CrashPoint::AfterSuperblockFsync,
    ] {
        run_one(point);
    }
}
