//! Phase-10G regression: the epoch envelope sequence number must be
//! globally monotonic across checkpoints, and the checkpoint must never
//! create a visibility gap for concurrent epoch ops.
//!
//! Two distinct races were found by the parallel-workload sweep and are
//! pinned here together (they share the checkpoint interleaving):
//!
//! 1. **Sequence monotonicity.** `Epoch::envelope()` assigns each op's
//!    `MutationLog` sequence at STAGE time (under the epoch mutex), while
//!    the envelope is appended later under `commit_lock`. A concurrent
//!    `epoch_checkpoint` can interleave between the two. When the fresh
//!    epoch RESTARTED at 0, an op staged after a checkpoint received a
//!    small seq <= an earlier `log_seq` and was silently DROPPED at
//!    recovery even though its overlay was never checkpointed, and two
//!    epochs could emit envelopes sharing a sequence (the recovery
//!    "duplicate mutation log sequence" invariant). The fix: the sequence
//!    counter is globally monotonic (never reset), so envelope seqs are
//!    unique, ordered by append order, and recovery replays exactly the
//!    envelopes whose overlays are absent from the last checkpoint root.
//!
//! 2. **Checkpoint visibility gap.** The old checkpoint `mem::take`'d the
//!    epoch (emptying the overlay) and then merged for a while before
//!    publishing the root — a concurrent epoch op in that window saw an
//!    EMPTY overlay AND a STALE committed root and failed with a spurious
//!    `Invariant("inode N missing")` (EIO on the mounted filesystem). The
//!    fix: the checkpoint SNAPSHOTS the overlay (the live epoch keeps its
//!    state, so reads never see the gap) and compare-and-removes exactly
//!    the snapshot's entries only after a successful commit.
//!
//! This test hammers concurrent epoch ops (create + write + setattr(size)
//! checkpoint flush) against an interleaving checkpoint thread on BOTH io
//! backends, then drops the store WITHOUT a final checkpoint (a process
//! crash: the log tail + staged objects are page-cache durable) and
//! requires every acknowledged file to remount byte-exact.

#![forbid(unsafe_code)]

use crate::optimizer::foreground::ForegroundPolicy;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::io::IoBackendKind;
use crate::store::transaction::CrashHooks;
use crate::store::{AttrUpdate, NewEntry, Store, StoreConfig};
use tempfile::TempDir;

fn cfg(kind: IoBackendKind) -> StoreConfig {
    StoreConfig {
        io_backend: kind,
        ..Default::default()
    }
}

/// Deterministic byte-uniform noise (SplitMix64): every file's content is
/// unique, so a dropped envelope at recovery is a visible byte mismatch.
fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let b = z.to_le_bytes();
        let take = (n - out.len()).min(8);
        out.extend_from_slice(&b[..take]);
    }
    out
}

/// RAII: on drop (normal return OR unwind) the checkpoint thread's stop
/// flag is set, so a worker panic can never leave the scope waiting on a
/// checkpoint thread that loops forever.
struct StopCheckpoints(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for StopCheckpoints {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

#[test]
fn epoch_sequence_is_globally_monotonic_across_checkpoints() {
    for kind in IoBackendKind::ALL {
        let dir = TempDir::new().unwrap();
        let store = Store::create(dir.path(), &cfg(kind), [0x51; 16]).unwrap();
        let root = store.current_root().root_dir_ino;

        const WORKERS: usize = 8;
        const FILES_PER_WORKER: usize = 6;
        const CHUNKS: usize = 2;
        let text_len = CHUNKS * 65536;

        if std::env::var("EFS_TRACE").is_ok() {
            eprintln!("[trace] {kind:?}: starting (workers {WORKERS}, files {FILES_PER_WORKER})");
        }

        // (worker, file) -> (ino, content): verified after the crash.
        let mut expected: Vec<(u64, Vec<u8>)> = Vec::new();

        // A dedicated checkpoint thread maximizes freeze-vs-append
        // interleavings: a checkpoint can consume the staged seqs into
        // log_seq and empty the overlay while another op's envelope is
        // mid-flight between stage-time seq assignment and its commit-lock
        // append. It exits when the workers are done (the scope joins it).
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cp_store = &store;
        let _no_cp_thread = std::env::var("EFS_NO_CP_THREAD").is_ok();
        std::thread::scope(|s| {
            // Sets the stop flag when this closure ends — on success OR on
            // unwind — so the checkpoint thread always exits before the
            // scope joins it (a worker panic must not hang the scope).
            let _stop_guard = StopCheckpoints(std::sync::Arc::clone(&stop));
            let stop_cp = std::sync::Arc::clone(&stop);
            if !_no_cp_thread {
                s.spawn(move || {
                    while !stop_cp.load(std::sync::atomic::Ordering::Relaxed) {
                        cp_store
                            .epoch_checkpoint(&CrashHooks::none())
                            .expect("checkpoint");
                        std::thread::yield_now();
                    }
                });
            }
            let handles: Vec<_> = (0..WORKERS)
                .map(|w| {
                    let store = &store;
                    s.spawn(move || {
                        let mut results = Vec::new();
                        for f in 0..FILES_PER_WORKER {
                            let name = format!("w{w}f{f}");
                            if std::env::var("EFS_TRACE").is_ok() {
                                eprintln!("[trace] w{w}: create {name}");
                            }
                            let ino = store
                                .epoch_create(
                                    root,
                                    name.as_bytes(),
                                    NewEntry::file(0o644, 1000, 1000),
                                    &CrashHooks::none(),
                                )
                                .expect("epoch_create");
                            let text = noise(text_len, 0xC0FFEE + (w as u64) * 256 + f as u64);
                            if std::env::var("EFS_TRACE").is_ok() {
                                eprintln!("[trace] w{w}: write {ino}");
                            }
                            store
                                .epoch_write(
                                    ino,
                                    0,
                                    &text,
                                    OptimizeOptions::default(),
                                    ForegroundPolicy::full(),
                                    &CrashHooks::none(),
                                )
                                .expect("epoch_write");
                            // The kernel's post-write setattr carries the
                            // SIZE, which flushes the epoch (a checkpoint)
                            // and runs the transactional truncate path.
                            if std::env::var("EFS_TRACE").is_ok() {
                                eprintln!("[trace] w{w}: setattr {ino}");
                            }
                            store
                                .epoch_setattr(
                                    ino,
                                    &AttrUpdate {
                                        size: Some(text.len() as u64),
                                        ..Default::default()
                                    },
                                    &CrashHooks::none(),
                                )
                                .expect("epoch_setattr");
                            // Overlay read-back must already be exact.
                            if std::env::var("EFS_TRACE").is_ok() {
                                eprintln!("[trace] w{w}: readback {ino}");
                            }
                            let ep = store.epoch();
                            let back = store
                                .read_file_epoch(&ep, ino, 0, text.len() as u64)
                                .expect("overlay read");
                            drop(ep);
                            assert_eq!(back, text, "{kind:?} overlay read mismatch");
                            if std::env::var("EFS_TRACE").is_ok() {
                                eprintln!("[trace] w{w}: done {ino}");
                            }
                            results.push((ino, text));
                        }
                        if std::env::var("EFS_TRACE").is_ok() {
                            eprintln!("[trace] w{w}: ALL DONE");
                        }
                        results
                    })
                })
                .collect();
            for h in handles {
                expected.extend(h.join().unwrap());
            }
        });

        // Crash WITHOUT a final checkpoint: the log tail replays at open.
        // Every acknowledged op must survive — the envelope and its
        // objects were flushed to the segment page cache before the ack.
        drop(store);
        let store = Store::open(dir.path(), &cfg(kind)).unwrap();
        for (ino, text) in &expected {
            let back = store
                .read_file(*ino, 0, text.len() as u64)
                .unwrap_or_else(|e| panic!("{kind:?}: remount read of ino {ino} failed: {e:?}"));
            assert_eq!(
                &back, text,
                "{kind:?}: acknowledged write lost across the crash (ino {ino})"
            );
        }
        let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
        assert!(
            report.is_clean(),
            "{kind:?} fsck after the crash:\n{}",
            report.render()
        );
    }
}
