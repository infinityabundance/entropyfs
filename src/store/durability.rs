//! Phase-12B durability generations: the group-commit coordinator.
//!
//! # Purpose
//!
//! Amortize concurrent `fsync` barriers without weakening the durability
//! contract. The 12B model (the brief):
//!
//! ```text
//! logical_seq   monotonically identifies acknowledged mutation state
//! durable_seq   highest logical sequence known to survive power loss
//!
//! fsync(required_seq = N) may return success iff durable_seq >= N
//! ```
//!
//! N concurrent fsyncs coalesce onto ONE physical barrier whose cut
//! covers every waiter registered before the physical work starts; each
//! waiter completes only after a cut that includes its writes (the
//! linearizability requirement the crash courts pin). A mutation
//! acknowledged AFTER the cut is chosen must NOT inherit that barrier.
//!
//! EntropyFS's acknowledged-mutation state has TWO monotonic coordinates:
//!
//! - `seq` — the epoch's mutation-log sequence (`Epoch::seq`; envelopes
//!   `> root.log_seq` are replayed at recovery). Covers staged epoch ops.
//! - `gen` — the published root's `generation` (bumped by EVERY commit:
//!   epoch checkpoints AND direct transaction commits). Covers direct
//!   (non-epoch) writes, which never touch `seq`.
//!
//! A barrier makes durable everything ≤ its cut `(seq, gen)` componentwise;
//! the group's durable state is the pair of store atomics advanced to the
//! last completed cut.
//!
//! # Model: the coordinator
//!
//! ```text
//! DurabilityGroup
//!   waiters: (required_seq, required_gen, joined_gen)
//!   owner_cut: Option<(seq, gen)>     the in-flight physical barrier
//!   owner_error: Option<(gen, String)>  a FAILED generation's error,
//!                                     tagged with its generation so only
//!                                     ITS covered waiters surface it
//!   next_gen: u64                     the generation counter
//! ```
//!
//! A caller registers with `joined_gen = next_gen` — the generation that
//! will cover it (the in-flight one if it registered before the takeover,
//! otherwise the next). The first waiter when idle becomes the OWNER:
//! it fixes the cut at the componentwise max of the CURRENT waiters'
//! requirements, increments `next_gen`, releases the group lock, and runs
//! the physical barrier (epoch checkpoint + commit-lock-held
//! fdatasync → dir sync → superblock write → superblock fsync). On
//! success the durable atomics advance to the CUT (conservative: never
//! beyond what the generation was required to cover, even if the
//! checkpoint flushed more); on failure the error is stored tagged with
//! the generation. Every waiter is woken and re-checks:
//!
//! ```text
//! durable covers required  -> Ok
//! my generation failed     -> Err(the generation's error)
//! otherwise                -> park again (or become the next owner)
//! ```
//!
//! # Correctness invariants
//!
//! - **Linearizability (write→fsync):** a waiter returns Ok only after a
//!   completed physical barrier whose cut covers its requirement — the
//!   checkpoint consumed every envelope ≤ its `seq` into the root (which
//!   recovery replays nothing beyond) and the superblock fsync'd a root
//!   at generation ≥ its `gen`.
//! - **No barrier inheritance across cuts:** a mutation acknowledged
//!   after the owner fixed the cut is not covered by `durable`'s advance
//!   to that cut (its `seq`/`gen` exceed it), so its fsync stays pending.
//! - **Failed generations are surfaced, never silently retried forever:**
//!   each waiter returns exactly one outcome; a failure is tagged with
//!   the generation so late arrivals (joined the NEXT generation) retry
//!   rather than inheriting the previous generation's error.
//! - **The physical barrier is unchanged:** same steps, same crash hooks,
//!   same commit-lock hold — the group only decides WHO runs it and WHO
//!   waits.
//!
//! # Concurrency
//!
//! One short mutex critical section at registration/takeover/wake; the
//! physical barrier runs outside the group lock (the owner holds the
//! COMMIT lock across the fsync window, exactly like the pre-12B
//! barrier). Waiters park on the condition variable; the durable atomics
//! are lock-free (Relaxed is sufficient — the store state they certify is
//! synchronized by the store's own locks, and the atomics are only
//! readiness markers).
//!
//! # Resource bounds
//!
//! One `(u64, u64, u64)` entry per concurrent fsync; the epoch bounds
//! outstanding requests. No allocations on the fast path.
//!
//! # Failure modes
//!
//! A poisoned group mutex panics (like every store mutex). A physical
//! barrier failure (I/O, ENOSPC) is stored and surfaced to the covered
//! waiters as `StoreError::Io` — durability is never silently claimed.
//!
//! # History / evidence
//!
//! Phase-11B/11C measured the fsync convoy (`commit_lock_wait` 34.7% of
//! 16-thread request time pre-11C; the barrier's own comment named the
//! group-durability future). The 12B oracle (`src/tests/fsync_group_probe.rs`,
//! sealed `evidence/performance/fsync-group-probe-*/`) sealed the
//! baseline: amplification 1.00 at every concurrency, fsync p99 45 µs →
//! 7.9 ms at 32 callers. This coordinator is the 12B-1 fix; the re-run
//! seals the amplification reduction (CHANGELOG v0.7.9).

#![forbid(unsafe_code)]

use std::collections::VecDeque;

/// One registered fsync waiter.
#[derive(Debug, Clone, Copy)]
pub struct DurabilityWaiter {
    /// Required logical sequence (epoch mutation-log sequence; envelopes
    /// `> root.log_seq` are replayed at recovery).
    pub required_seq: u64,
    /// Required root generation (covers direct non-epoch commits).
    pub required_gen: u64,
    /// The generation that will cover this waiter (`next_gen` at
    /// registration). Tagged errors are surfaced only to the waiters of
    /// the failed generation.
    pub joined_gen: u64,
}

/// The group-commit coordinator state (behind the store's
/// `durability_group` mutex; the condition variable lives beside it in
/// the store).
#[derive(Debug, Default)]
pub struct DurabilityGroup {
    /// Waiters not yet covered, in arrival order.
    pub waiters: VecDeque<DurabilityWaiter>,
    /// The in-flight physical barrier's cut (`(seq, gen)`), or `None`
    /// when idle. Fixed at owner takeover; late arrivals are NOT covered.
    pub owner_cut: Option<(u64, u64)>,
    /// A FAILED generation: `(generation, error)`. Cleared on the next
    /// successful barrier.
    pub owner_error: Option<(u64, String)>,
    /// Monotonic generation counter (bumped at each owner takeover).
    pub next_gen: u64,
}

impl DurabilityGroup {
    /// Register a waiter; returns the generation that will cover it.
    pub fn register(&mut self, required_seq: u64, required_gen: u64) -> DurabilityWaiter {
        let joined_gen = self.next_gen;
        let w = DurabilityWaiter {
            required_seq,
            required_gen,
            joined_gen,
        };
        self.waiters.push_back(w);
        w
    }

    /// Componentwise max over the current waiters — the cut that covers
    /// every waiter registered so far.
    pub fn max_required(&self) -> (u64, u64) {
        let mut s = 0u64;
        let mut g = 0u64;
        for w in &self.waiters {
            s = s.max(w.required_seq);
            g = g.max(w.required_gen);
        }
        (s, g)
    }

    /// Whether the durable state covers this requirement.
    pub fn covers(
        durable_seq: u64,
        durable_gen: u64,
        required_seq: u64,
        required_gen: u64,
    ) -> bool {
        durable_seq >= required_seq && durable_gen >= required_gen
    }
}
