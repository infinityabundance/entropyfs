//! Phase 12C-1-3: the pool write-path deadlock regression pin.
//!
//! # THE BUG THIS PINS (mounted-court-found)
//!
//! The 12C-1-3 mounted engagement probe (16 FUSE writers x 1 MiB
//! structured writes against pool-16, capacity 128) deadlocked the
//! daemon: every session thread blocked, the pool ring empty, all worker
//! threads parked. Two distinct pool defects combined:
//!
//! 1. **Runtime-mutex-across-wait** (the primary): `SearchPool::submit`
//!    held the pool's `runtime` mutex across the whole backpressure wait,
//!    while the worker tasks' own per-chunk `store.pressure_engaged()`
//!    calls (Focused mode) go through `POOL.pressure()` — which takes
//!    the SAME `runtime` lock. Once more submitters blocked in the wait
//!    than the capacity admits, every worker's pressure call blocked on
//!    the held guard: tasks never completed, `in_flight` never dropped,
//!    and the waiters never woke. The 12C-1-2 mounted court never saw it
//!    because its 8 writers x 16 chunks sat exactly at capacity (no
//!    waiters).
//! 2. **Notify-at-full-drain only** (the secondary): the backpressure
//!    condvar was notified only when `in_flight` hit zero, and without
//!    holding the wait lock — so under saturation fast re-submitters
//!    could re-take the freed capacity before the pool reached zero
//!    (waiters starved), and a notify landing between a submitter's
//!    check and its sleep was lost.
//!
//! # WHAT THIS TEST DOES
//!
//! Replays the engagement shape at the store level (no FUSE): 16 writer
//! threads x 512 KiB distinct structured writes, the Focused/pressure
//! policy (so the pressure gate fires inside the pool tasks), pool-8
//! (capacity 64 < 16 x 8 chunks = 128 wanted — waiters MUST form). The
//! pre-fix code deadlocks here (a 60 s deadline fails loudly instead of
//! hanging the suite); the fixed code completes in well under a second.
//!
//! # BOUNDARY
//!
//! KNOWS the store write path + the pool + the pressure policy — the
//! exact surface the 12C-1-3 mounted court exercises. Byte correctness
//! is the write path's own contract (asserted by the write path's §32
//! validation); this pin is about LIVENESS under saturation.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Instant;

use tempfile::TempDir;

use crate::optimizer::foreground::ForegroundPolicy;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};

/// The liveness deadline: the fixed path completes in < 1 s; the
/// pre-fix deadlock must fail loudly, not hang the suite forever.
const LIVENESS_DEADLINE_SECS: u64 = 60;

#[test]
fn pool_write_path_saturation_stays_live() {
    let dir = TempDir::new().unwrap();
    let cfg = StoreConfig {
        segment_size: 128 * 1024 * 1024,
        ..Default::default()
    };
    // The pool is process-global and the pool tests configure it; this
    // test must serialize with them (see `workers::tests::POOL_LOCK`).
    let _pool_guard = crate::store::workers::tests::POOL_LOCK
        .lock()
        .expect("pool test lock poisoned");
    let store = Arc::new(Store::create(dir.path(), &cfg, [0x44; 16]).unwrap());
    crate::store::workers::POOL.enable(8, 8); // capacity 64
    crate::store::workers::POOL.bind(&store);
    store.enable_worker_pool();
    let hooks = &CrashHooks::none();
    store.set_semantic_mode(crate::dsfb::semantics::SemanticMode::Combined);
    // The sealed 12C-1-2 `pressure` shape: the pressure gate engages
    // inside the pool tasks (the bug's trigger), with a generous debt
    // cap so no starvation-cap refusal distorts the liveness check.
    let fg = ForegroundPolicy {
        pressure_enter: 0.80,
        pressure_leave: 0.60,
        pressure_defer_configurational: true,
        pressure_max_deferred_bytes: 1024 * 1024 * 1024,
        ..ForegroundPolicy::focused()
    };
    let opts = OptimizeOptions::default();
    let root = store.current_root().root_dir_ino;
    let dir_ino = store
        .epoch_create(root, b"sat", NewEntry::dir(0o755, 1000, 1000), hooks)
        .unwrap();

    let t = 16usize;
    let rounds = 3usize;
    let mut handles = Vec::new();
    let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for w in 0..t {
        let store = Arc::clone(&store);
        let done = Arc::clone(&done);
        handles.push(std::thread::spawn(move || {
            let hooks = &CrashHooks::none();
            let mut state: u64 = 1000 + w as u64;
            for r in 0..rounds {
                // 512 KiB of DISTINCT structured text per (writer, round):
                // rANS-valuable (the gate defers it under pressure), never
                // a dedup hit, 8 chunks per write (16 writers x 8 = 128
                // wanted vs capacity 64 — waiters MUST form).
                let mut b = Vec::with_capacity(512 * 1024);
                while b.len() < 512 * 1024 {
                    state = state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    b.push(
                        b"abcdefghijklmnopqrstuvwxyz0123456789{}();,= \n"
                            [((state >> 33) as usize) % 45],
                    );
                }
                let name = format!("w{w}-{r}");
                let ino = store
                    .epoch_create(
                        dir_ino,
                        name.as_bytes(),
                        NewEntry::file(0o644, 1000, 1000),
                        hooks,
                    )
                    .unwrap();
                store
                    .epoch_write_semantic(ino, 0, &b, opts, fg, None, hooks)
                    .unwrap();
            }
            done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }));
    }
    let deadline = Instant::now() + std::time::Duration::from_secs(LIVENESS_DEADLINE_SECS);
    while done.load(std::sync::atomic::Ordering::Relaxed) < t {
        assert!(
            Instant::now() < deadline,
            "pool write-path deadlock: {}/{} writers completed within the liveness deadline",
            done.load(std::sync::atomic::Ordering::Relaxed),
            t
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    for h in handles {
        h.join().unwrap();
    }
    crate::store::workers::POOL.disable();
    let (de, db, _) = store.deferred_debt();
    assert!(
        de > 0 && db > 0,
        "the pressure gate must have deferred under saturation (de={de}, db={db})"
    );
}
