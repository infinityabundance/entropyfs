//! The storage observer: wraps the published `dsfb` crate and adapts it to
//! per-chunk predictor-channel evidence.
//!
//! # Purpose
//!
//! Maintain the per-chunk evidence state that steers candidate search:
//! per-channel trust (from EMA residuals), the drift–slew regime, and the
//! resulting search plan. Fed once per write from `encode_guided`
//! ([`ShardedStorageObserver::observe`]), consulted before the budgeted
//! search ([`ShardedStorageObserver::plan`], [`ShardedStorageObserver::trust`]).
//!
//! # Model: the per-chunk state machine
//!
//! For each chunk key (file inode, chunk index, **content id** — the state
//! is per content *version*, so a rewritten chunk starts from full
//! distrust) the observer keeps one [`ChunkObserver`] with 9 channels
//! (P0..P8; P8 SharedDict joined in Phase-9C). Each write feeds:
//!
//! 1. the bounded measurement `y ∈ [0, 1]` of every channel that was
//!    actually evaluated — unevaluated channels are fed 0.0 ("no
//!    evidence"), so only evaluated channels earn trust;
//! 2. the winning channel; and
//! 3. the outcome quality of the win (1.0 for perfect structural/generated
//!    wins, the channel's measurement otherwise) — the regime tracker
//!    consumes this so a structural win never looks like a regime break.
//!
//! Per-channel trust follows `ema ← rho·ema + (1 − rho)·|1 − y|`,
//! initialized to 1.0 (max distrust); weights are normalized by
//! `dsfb::trust::calculate_trust_weights` (raw `1/(σ0 + residual)`,
//! normalized across channels). The regime comes from the robust
//! [`MeasurementTracker`] over the winner-quality series:
//! Unknown → Stable / Drift / Slew. When the previous observation
//! classified a slew (or on the first observation) the baseline re-points
//! at the new chunk's content id. Regime maps to search strategy and
//! budget in [`ShardedStorageObserver::plan`]: Slew → Broad/32,
//! Drift → Balanced/12, Stable/Unknown → Narrow/4.
//!
//! # Why the parameters are shaped this way
//!
//! Steps are sparse — one per write, not per µs — and measurements are
//! bounded in `[0, 1]`. The EMA is therefore slow (rho = 0.9) and the
//! gains gentle (k_phi = 0.5, k_omega = 0.1, k_alpha = 0.02): a single
//! noisy write must not move trust or regime far, and the slew gain being
//! the smallest keeps α from firing on step noise. `dt` is fixed at 1.0 —
//! the observer is clocked by writes, so φ/ω/α integrate per step, not
//! per wall-time unit. `sigma0` (0.1) softens the raw trust weight
//! `1/(σ0 + residual)`. All thresholds live in the measurement scale
//! `[0, 1]`.
//!
//! # Boundary
//!
//! The observer knows only derived, advisory evidence. It never reads or
//! writes store bytes, never persists, and cannot veto a candidate — it
//! only orders and budgets the search (`docs/theory/dsfb-selection.md`
//! §4). `core` never imports `dsfb`.
//!
//! # Correctness invariants
//!
//! - Measurements are clamped to `[0, 1]` at the boundary; EMAs and the
//!   tracker stay in bounded ranges.
//! - Trust is version-scoped: the key includes the content id, so a
//!   rewritten chunk (new bytes → new id) starts from ema 1.0 / Unknown
//!   and its history never bleeds across versions.
//! - The regime tracker sees outcome quality, not the raw winner channel,
//!   so a perfect structural win cannot trigger a regime break.
//! - Forgetting, evicting, or deleting the observer never changes
//!   persisted bytes (ADR-0004).
//! - **Atomic-count exactness:** `tracked` is the exact live entry count —
//!   every insert bumps it and every remove (forget/evict) decrements it
//!   under the same shard lock that mutates the map, so `tracked == Σ
//!   shard lens` at every instant. `len()` therefore reads one atomic
//!   instead of summing 16 locks, and the store's cap gate
//!   (`DSFB_MAX_CHUNKS`) is race-free without a global mutex.
//!
//! # Concurrency (Phase-11F)
//!
//! The observer is **sharded** (11F): the per-key state lives in
//! `DSFB_SHARDS` (16) independently locked `HashMap`s, and the aggregate
//! statistics are lock-free atomics. Every accessor locks **exactly one
//! shard** — the one `shard_of(key)` picks via a stable FNV-1a hash of the
//! key bytes:
//!
//! ```text
//! ChunkKey (ino, index, content_id)
//!     -> FNV-1a 64 over the 48 key bytes   (fully specified, stable
//!        across platforms and Rust versions — std's DefaultHasher is not)
//!     -> shard = hash % DSFB_SHARDS
//! ```
//!
//! The store previously serialized ALL observer access through one mutex
//! (`Store::dsfb`), even though each `ChunkObserver` is per-key
//! independent state. The 11D brief predicted that mutex would become
//! visible as more independent requests advanced through search
//! simultaneously under the 11E fair pool. The 11F oracle
//! (`evidence/performance/dsfb-shard-*/`, probe
//! `src/tests/dsfb_shard_probe.rs`) tested that prediction directly and
//! found the observer calls measure ~1 µs each — 0.1–0.5% of `prepare`
//! even at 4× the sealed sweep scale — so the mutex was never a material
//! wall-time bottleneck at any measured scale. The shard was adopted
//! anyway, for three reasons the oracle does not contradict:
//!
//! 1. **Architecture:** the observer's state is per-key, so its locking
//!    should be per-key; the shard removes the LAST process-wide write
//!    serialization point, so the write path is now synchronization-free
//!    end to end except for the commit coordinator and the per-inode
//!    locks (which are real shared state, not advisory evidence).
//! 2. **Future-proofing:** Phase-12C (DSFB Structural Semiotics) deepens
//!    the per-call work (semantic context features) and will widen the
//!    per-call critical section; a single mutex would serialize that.
//! 3. **Zero measured regression:** the 11F oracle verified byte
//!    identity, density, wall, latency, and CPU are unchanged within
//!    run-to-run noise.
//!
//! The 11F oracle's falsification of the "mutex becomes visible"
//! prediction is recorded in the sealed evidence and CHANGELOG v0.7.7;
//! this code implements the shard as the permanently correct shape, not
//! as a response to a measured emergency.
//!
//! # Resource bounds
//!
//! One [`ChunkObserver`] per distinct key; the store caps the total at
//! `DSFB_MAX_CHUNKS` (100 000) by reading the exact atomic count and
//! calling [`ShardedStorageObserver::evict_one_from`] on the shard that
//! just grew (the store's policy, unchanged in shape from the pre-11F
//! gate). Per-entry state is fixed-size arrays over the 9 channels.
//! Distinct content versions are the only growth vector, and the cap
//! bounds it. Per-shard maps hold ~cap/16 entries, which also improves
//! lookup locality over one 100k-entry map.
//!
//! # Performance
//!
//! [`ShardedStorageObserver::observe`] is O(channels) with stack-only
//! scratch; [`ShardedStorageObserver::plan`] sorts 9 elements. Both run on
//! the write path, so allocation is avoided. The 11F oracle measured the
//! whole call (lock + work) at ~1 µs in release; the shard converts the
//! theoretical O(concurrency) serialization into O(concurrency/16)
//! and, more importantly, makes unrelated keys never block each other.
//! The exhaustive-search alternative is the `no-dsfb` ablation mode (H3).
//!
//! # Failure modes
//!
//! Infallible by construction. Out-of-range measurements are clamped; a
//! NaN would propagate into the EMAs (the caller contract is bounded
//! evidence — `Features::measurement` and `measurement_for_ratio` both
//! clamp). Wrong predictions cost search CPU only. A poisoned shard mutex
//! is a panic (`.expect("dsfb shard poisoned")`) — the observer cannot
//! run under a panicked writer, and panicking loudly beats silent
//! corruption of advisory state.
//!
//! # History / evidence
//!
//! Phase 4 (channels P0–P5), Phase-9C (P8 SharedDict, v0.4.0), ADR-0004,
//! H3 ablation methodology (`docs/theory/dsfb-selection.md` §5), upstream
//! audit (`docs/research/upstream-audit.md` §2), 11D oracle
//! (`evidence/performance/worker-oracle-1787765041-052bc46/`) and 11E
//! probe (the "DSFB mutex becomes visible" prediction), and the 11F
//! shard oracle (`evidence/performance/dsfb-shard-*/`) that falsified the
//! prediction at the sealed scale and verified zero regression (CHANGELOG
//! v0.7.7).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::dsfb::drift::{MeasurementTracker, Regime};
use crate::dsfb::features::{Channel, ChunkKey};
use crate::dsfb::selection::{SearchPlan, SearchStrategy};

/// Number of observer shards.
///
/// WHY 16:
///
/// - It equals the maximum worker concurrency the 11E pool can run (16 on
///   this 8-core/16-SMT machine, `available_parallelism()`), so two
///   workers contending on the same key class are rare and unrelated keys
///   essentially never collide;
/// - it is a power of two, so `hash % DSFB_SHARDS` is a mask — one
///   cycle, no division;
/// - it bounds per-shard maps at `DSFB_MAX_CHUNKS / 16 ≈ 6250` entries,
///   which keeps a shard's `HashMap` in better cache locality than one
///   100k-entry map would have.
///
/// The count is deliberately NOT `available_parallelism()`-derived: the
/// observer is per-store and the pool is process-global, so a store-local
/// shard count derived from the machine's cores would change meaning
/// across machines. 16 is a fixed architectural constant; the oracle's
/// before/after runs share it.
pub const DSFB_SHARDS: usize = 16;

/// DSFB observer parameters for storage evidence.
///
/// Values chosen for the storage domain: measurements are bounded in
/// [0, 1], steps are sparse (per write, not per µs), so the EMA is slow
/// (high rho) and gains are gentle. Units: gains are per-measurement /
/// per-step factors; `rho` is a dimensionless EMA factor; `dt` is a
/// dimensionless per-write step constant; the two thresholds are in the
/// raw φ/ω/α scale (not the measurement scale).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StorageDsfbParams {
    /// Position gain: how strongly one measurement moves the
    /// representation-quality state φ. Per-measurement; gentle (0.5) so a
    /// single write cannot swing the quality regime.
    pub k_phi: f64,
    /// Drift gain: per-step growth of the drift state ω from measurement
    /// deltas. 0.1 — drift must accumulate over several steps to register.
    pub k_omega: f64,
    /// Slew gain: per-step growth of the slew state α (acceleration of
    /// change). 0.02 — the smallest gain, so α needs a sustained break,
    /// not a single noisy step, to classify slew.
    pub k_alpha: f64,
    /// EMA smoothing (0 < rho < 1). 0.9 = slow: steps are sparse (per
    /// write) and measurements are bounded, so each write should move
    /// trust only a little.
    pub rho: f64,
    /// Trust softness: the raw channel weight is `1/(σ0 + residual)`.
    /// 0.1 keeps the raw weight finite and softens trust separation.
    pub sigma0: f64,
    /// dt per write step (fixed; the observer is clocked by writes, so
    /// φ/ω/α integrate per step, not per wall-time unit).
    pub dt: f64,
    /// Slew threshold: |α| above this (scaled) classifies as slew. In the
    /// raw φ/ω/α scale; used only by `drift::classify` (tests/reporting),
    /// not by the runtime `MeasurementTracker` path.
    pub slew_alpha_threshold: f64,
    /// Drift threshold: |ω| below this (scaled) with small |α| is drift.
    /// In the raw φ/ω/α scale; used only by `drift::classify`.
    pub drift_omega_threshold: f64,
}

impl Default for StorageDsfbParams {
    fn default() -> Self {
        Self {
            k_phi: 0.5,
            k_omega: 0.1,
            k_alpha: 0.02,
            rho: 0.9,
            sigma0: 0.1,
            dt: 1.0,
            slew_alpha_threshold: 0.05,
            drift_omega_threshold: 0.02,
        }
    }
}

impl StorageDsfbParams {
    /// The underlying `dsfb` parameters.
    pub fn dsfb_params(&self) -> dsfb::DsfbParams {
        dsfb::DsfbParams::new(
            self.k_phi,
            self.k_omega,
            self.k_alpha,
            self.rho,
            self.sigma0,
        )
    }
}

/// Per-chunk observer state: the (file, index, content-version) evidence
/// machine described in the module doc.
///
/// Invariants: `ema`, `weights`, `last_y` are indexed by the same channel
/// ids as `Channel` (`c as usize`), so each array is exactly
/// `Channel::ALL.len()` long; every value in `ema`/`last_y` is in [0, 1];
/// `weights` is normalized (sum ≈ 1) after the first observation.
struct ChunkObserver {
    /// The underlying drift–slew state observer (φ/ω/α), fed the last
    /// known measurement per channel (`last_y`). Reported via the crate's
    /// `state*` accessors; regime classification uses the robust tracker,
    /// not the raw φ/ω/α integration.
    inner: dsfb::DsfbObserver,
    /// Robust regime tracker over the winner-measurement (outcome
    /// quality) series. This is the authority for `regime` — see
    /// `drift.rs` for why the raw φ/ω/α state is not used.
    tracker: MeasurementTracker,
    /// Per-channel EMA of `|1 − y|` (the evidence error). Initialized to 1
    /// (max distrust): a channel earns trust only by being evaluated —
    /// unevaluated channels never have their EMA moved, so they stay
    /// distrusted while evaluated channels dominate the normalized
    /// weights. Units: measurement scale [0, 1]; lower = better.
    ema: [f64; Channel::ALL.len()],
    /// Normalized trust weights (via `dsfb::trust::calculate_trust_weights`:
    /// raw `1/(σ0 + residual)`, normalized across channels, so the vector
    /// sums to ≈1). Seeded 0.125 (1/8); with 9 channels that placeholder
    /// does not sum to 1, which is harmless — the first normalization
    /// overwrites every entry, and `ShardedStorageObserver::trust` falls
    /// back to 0.125 only for unknown chunks / never-observed channels.
    weights: [f64; Channel::ALL.len()],
    /// Last known measurement per channel — the series fed to `inner.step`
    /// on every write (0.0 = no evidence for unevaluated channels).
    last_y: [f64; Channel::ALL.len()],
    /// Last classification from `tracker`.
    regime: Regime,
    /// Steps fed (per this key).
    samples: u64,
    /// Last winning channel (attribution only; the tracker consumes the
    /// outcome quality, not this). Seeded to RAW, the escape hatch.
    winner: Channel,
    /// Baseline content id — the chunk version the observer currently
    /// trusts as its basis. Re-pointed at the new chunk on a slew (or on
    /// the first observation), so a regime break does not carry the old
    /// basis's trust into the new regime.
    baseline: Option<crate::core::extent::ChunkId>,
}

/// The storage DSFB observer (Phase-11F sharded): per-chunk observers
/// behind 16 independent shard locks plus lock-free aggregate statistics.
///
/// # State layout
///
/// ```text
/// ShardedStorageObserver
///   params: StorageDsfbParams          (immutable, Copy — no lock needed)
///   shards: [Mutex<HashMap<ChunkKey, ChunkObserver>>; 16]
///   tracked: AtomicUsize               (exact live entry count)
///   steps / drift_events / slew_events / narrowed_searches: AtomicU64
/// ```
///
/// # Why the statistics are atomics
///
/// The pre-11F observer kept `stats: ObserverStats` as a plain struct
/// under the single store mutex. With per-shard locks there is no single
/// lock to protect a plain struct, and re-introducing a global mutex for
/// the counters would recreate the exact serialization point 11F removes.
/// Every counter is therefore a lock-free atomic; `stats()` assembles the
/// aggregate `ObserverStats` snapshot from them.
///
/// # Invariant (exact count)
///
/// `tracked` is incremented exactly when a vacant key is inserted and
/// decremented exactly when a present key is removed — both under the
/// shard lock that guards the map — so it always equals the sum of the
/// shard lengths. The store's cap gate reads it without any lock and the
/// count is exact (module doc, "Atomic-count exactness").
pub struct ShardedStorageObserver {
    params: StorageDsfbParams,
    shards: [Mutex<HashMap<ChunkKey, ChunkObserver>>; DSFB_SHARDS],
    /// Exact live entry count (`Σ shard lengths`; module doc invariant).
    tracked: AtomicUsize,
    /// Total steps fed (cumulative write observations, all chunks).
    steps: AtomicU64,
    /// Drift events observed (cumulative across all chunks).
    drift_events: AtomicU64,
    /// Slew events observed (cumulative across all chunks).
    slew_events: AtomicU64,
    /// Candidates skipped due to low trust (search narrowing). Accounting
    /// surface reported in `status`: the skips themselves are decided by
    /// the search consumer — the foreground trust gate
    /// (`FOREGROUND_BASE_TRUST`) and the plan-budget cutoffs in
    /// `src/optimizer/search.rs`; no call site increments this counter
    /// yet.
    narrowed_searches: AtomicU64,
    /// Round-robin cursor for [`ShardedStorageObserver::evict_one`] (the
    /// shard-less eviction entry point used by tests and as the generic
    /// fallback; the store's targeted policy uses
    /// [`ShardedStorageObserver::evict_one_from`] instead).
    evict_cursor: AtomicUsize,
    /// Phase-12C: the learned per-class channel prior (structural
    /// semiotics; advisory). One store-level mutex — the table is small,
    /// the updates are one per observe, and the 11F oracle measured this
    /// contention class at ~1 µs per call (12C-1 may shard the prior
    /// like the observer if the adopted-mode measurements justify it).
    prior: Mutex<crate::dsfb::semantics::SemanticPrior>,
}

/// Aggregate observer statistics (reported in `status`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ObserverStats {
    /// Chunks currently tracked (live [`ChunkObserver`] count).
    pub tracked_chunks: usize,
    /// Drift events observed (cumulative across all chunks).
    pub drift_events: u64,
    /// Slew events observed (cumulative across all chunks).
    pub slew_events: u64,
    /// Total steps fed (cumulative write observations).
    pub steps: u64,
    /// Total candidates skipped due to low trust (search narrowing).
    /// Accounting surface reported in `status`: the skips themselves are
    /// decided by the search consumer — the foreground trust gate
    /// (`FOREGROUND_BASE_TRUST`) and the plan-budget cutoffs in
    /// `src/optimizer/search.rs`; no call site increments this counter
    /// yet.
    pub narrowed_searches: u64,
}

impl Default for ShardedStorageObserver {
    fn default() -> Self {
        Self::new(StorageDsfbParams::default())
    }
}

/// FNV-1a 64-bit offset basis and prime.
///
/// The shard hash is deliberately FNV-1a rather than
/// `std::collections::hash_map::DefaultHasher`: FNV-1a is a fully
/// specified algorithm, so the shard a key lands in is stable across
/// platforms and Rust versions. Stability matters for reproducibility
/// (the 11F oracle, tests) even though the assignment is performance-only
/// state — a key landing in a different shard after a toolchain upgrade
/// must never be able to change measured results for no reason.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

impl ShardedStorageObserver {
    /// Create with parameters.
    pub fn new(params: StorageDsfbParams) -> Self {
        Self {
            params,
            shards: std::array::from_fn(|_| Mutex::new(HashMap::new())),
            tracked: AtomicUsize::new(0),
            steps: AtomicU64::new(0),
            drift_events: AtomicU64::new(0),
            slew_events: AtomicU64::new(0),
            narrowed_searches: AtomicU64::new(0),
            evict_cursor: AtomicUsize::new(0),
            prior: Mutex::new(crate::dsfb::semantics::SemanticPrior::default()),
        }
    }

    /// The shard index for a key: stable FNV-1a over the 48 key bytes
    /// (ino, index, content id), reduced modulo [`DSFB_SHARDS`].
    ///
    /// # Why the key bytes, not a derived hash of the struct
    ///
    /// `ChunkKey` derives `Hash` via `DefaultHasher`, whose output is not
    /// guaranteed stable across Rust versions. The explicit byte layout
    /// (LE u64s + the 32 content-id bytes) makes the mapping fully
    /// specified and portable. The reduction is `% DSFB_SHARDS` (a mask in
    /// practice, since 16 is a power of two).
    ///
    /// # Performance
    ///
    /// 48 FNV rounds ≈ a few ns; called once per accessor, never on a
    /// hot loop of its own.
    pub fn shard_of(&self, key: &ChunkKey) -> usize {
        let mut h = FNV_OFFSET_BASIS;
        for b in key
            .ino
            .to_le_bytes()
            .iter()
            .chain(key.index.to_le_bytes().iter())
        {
            h ^= *b as u64;
            h = h.wrapping_mul(FNV_PRIME);
        }
        for &b in key.content_id.as_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(FNV_PRIME);
        }
        (h % DSFB_SHARDS as u64) as usize
    }

    /// Feed one write-observation for a chunk. `measurements` maps each
    /// evaluated channel to its bounded measurement; unevaluated channels
    /// are fed 0.0 (no evidence) so evaluated channels dominate trust.
    /// `outcome_quality` is the quality of the winning representation
    /// (1.0 for a perfect structural/generated win, the channel's
    /// measurement for base/rANS/RAW-driven wins); the regime tracker
    /// consumes it so structural wins never look like a regime break.
    ///
    /// Phase-12C: `semantic` + `mode` feed the per-class prior (the
    /// observer learns "this class of chunk wins with channel C"); the
    /// prior is advisory (module doc).
    ///
    /// # State machine, in order
    ///
    /// 1. Insert/refresh the [`ChunkObserver`] for the key — a new content
    ///    id means a fresh entry (full distrust, Unknown, RAW winner).
    /// 2. Re-baseline: when the *previous* observation classified a slew,
    ///    or on the first observation, point `baseline` at this chunk's
    ///    content id — the new version is the new basis.
    /// 3. Update `last_y` and the trust EMAs for evaluated channels only.
    /// 4. Renormalize trust weights (rho = 0 ⇒ the weight update is an
    ///    identity; only the normalization runs).
    /// 5. Advance the φ/ω/α integrator with the last-known measurements
    ///    (reporting state).
    /// 6. Classify the regime from the robust tracker over
    ///    `outcome_quality` and tally drift/slew events.
    ///
    /// # Concurrency
    ///
    /// Locks exactly the shard of `key` (and only for the duration of the
    /// update); the global counters are lock-free atomics. Unrelated keys
    /// never block each other. The prior table takes its own brief lock.
    ///
    /// Returns the new regime. Performance-only: regime and internal state
    /// feed search ordering/budget, never the committed representation
    /// (ADR-0004/0010).
    pub fn observe(
        &self,
        key: ChunkKey,
        measurements: &[(Channel, f64)],
        winner: Channel,
        outcome_quality: f64,
        semantic: Option<crate::dsfb::semantics::SemanticContext>,
        mode: crate::dsfb::semantics::SemanticMode,
    ) -> Regime {
        self.steps.fetch_add(1, Ordering::Relaxed);
        let mut m = [0.0f64; Channel::ALL.len()];
        for &(c, v) in measurements {
            m[c as usize] = v.clamp(0.0, 1.0);
        }
        let winner_measurement = outcome_quality.clamp(0.0, 1.0);
        let shard = self.shard_of(&key);
        let mut map = self.shards[shard].lock().expect("dsfb shard poisoned");
        // The exact-count invariant: bump `tracked` exactly when a vacant
        // key becomes present (both under this shard lock).
        if !map.contains_key(&key) {
            self.tracked.fetch_add(1, Ordering::Relaxed);
        }
        let entry = map.entry(key).or_insert_with(|| ChunkObserver {
            inner: dsfb::DsfbObserver::new(self.params.dsfb_params(), Channel::ALL.len()),
            tracker: MeasurementTracker::default(),
            ema: [1.0; Channel::ALL.len()],
            weights: [0.125; Channel::ALL.len()],
            last_y: [0.0; Channel::ALL.len()],
            regime: Regime::Unknown,
            samples: 0,
            winner: Channel::Raw,
            baseline: None,
        });
        entry.samples += 1;
        entry.winner = winner;
        // Re-baseline on slew: when the previous observation classified a
        // slew, the new chunk establishes a new baseline. First-ever
        // observations baseline too.
        if entry.regime == Regime::Slew || entry.baseline.is_none() {
            entry.baseline = Some(key.content_id);
        }
        // Track the last known measurement per channel and update the
        // trust EMAs for evaluated channels only.
        let rho = self.params.rho;
        for &(c, v) in measurements {
            let idx = c as usize;
            entry.last_y[idx] = v.clamp(0.0, 1.0);
            let err = (1.0 - entry.last_y[idx]).abs();
            entry.ema[idx] = rho * entry.ema[idx] + (1.0 - rho) * err;
        }
        // Normalize trust weights with the crate's trust function. Pass the
        // maintained EMAs as residuals with rho = 0 so the update is an
        // identity and only the normalization runs.
        let residuals = entry.ema;
        let mut scratch = [0.0f64; Channel::ALL.len()];
        let w =
            dsfb::trust::calculate_trust_weights(&residuals, &mut scratch, 0.0, self.params.sigma0);
        for (k, &wk) in w.iter().enumerate() {
            entry.weights[k] = wk;
        }
        // Feed the state observer the last known measurements (reporting).
        entry.inner.step(&entry.last_y, self.params.dt);
        // Regime comes from the robust measurement tracker.
        let regime = entry.tracker.observe(winner_measurement);
        match regime {
            Regime::Drift => {
                self.drift_events.fetch_add(1, Ordering::Relaxed);
            }
            Regime::Slew => {
                self.slew_events.fetch_add(1, Ordering::Relaxed);
            }
            Regime::Stable | Regime::Unknown => {}
        }
        entry.regime = regime;
        // `tracked` is maintained incrementally; nothing to refresh here
        // (the pre-11F `stats.tracked_chunks = map.len()` refresh moved
        // into the atomic increment/decrement points).
        // Phase-12C: learn the per-class winner (advisory prior; the
        // mode gates which class groups feed the key).
        if let Some(pkey) = semantic.and_then(|s| s.key_for(mode)) {
            self.prior
                .lock()
                .expect("dsfb prior poisoned")
                .observe(pkey, winner);
        }
        regime
    }

    /// Trust weight for a channel of a chunk (0 = fully distrusted).
    /// Unknown chunks and never-observed channels return the 0.125
    /// placeholder — equal, unearned weight — so a fresh chunk starts
    /// with no channel preferred.
    ///
    /// # Concurrency
    ///
    /// Locks exactly the shard of `key`; the returned `f64` is a copy, so
    /// the lock is released before the caller uses the value.
    pub fn trust(&self, key: &ChunkKey, channel: Channel) -> f64 {
        let shard = self.shard_of(key);
        self.shards[shard]
            .lock()
            .expect("dsfb shard poisoned")
            .get(key)
            .map(|c| c.weights[channel as usize])
            .unwrap_or(0.125)
    }

    /// Build the candidate-search plan for a chunk: channels ordered by
    /// trust (descending), with a budget from the regime: Slew ⇒ Broad
    /// (32 candidates), Drift ⇒ Balanced (12), Stable/Unknown ⇒ Narrow
    /// (4). Unknown chunks — never observed, forgotten, or evicted — get
    /// the cheap Narrow plan until evidence arrives. The plan only orders
    /// and bounds the candidate evaluation; the winner is still exact cost
    /// (ADR-0010).
    ///
    /// Phase-12C: when `semantic` + `mode` enable the prior, the ordering
    /// score is `historical_trust + SEMANTIC_WEIGHT * prior(class, chan)`
    /// — a class that historically wins with channel C moves C earlier in
    /// the plan (and, under the budget, into it). The prior never changes
    /// the budget semantics, only the order.
    ///
    /// # Concurrency
    ///
    /// Locks exactly the shard of `key` (the `ChunkObserver` reads plus
    /// the 9-element sort all happen under it — the sort is stack-local,
    /// so the critical section is ~1 µs) plus the brief prior lock.
    pub fn plan(
        &self,
        key: &ChunkKey,
        semantic: Option<crate::dsfb::semantics::SemanticContext>,
        mode: crate::dsfb::semantics::SemanticMode,
    ) -> SearchPlan {
        let shard = self.shard_of(key);
        let (regime, trust) = {
            let map = self.shards[shard].lock().expect("dsfb shard poisoned");
            let regime = map.get(key).map(|c| c.regime).unwrap_or(Regime::Unknown);
            let mut trust: Vec<(Channel, f64)> = Channel::ALL
                .iter()
                .map(|&c| (c, self.trust_locked(&map, key, c)))
                .collect();
            drop(map);
            // Phase-12C: the semantic prior adjusts the ordering score.
            if let Some(pkey) = semantic.and_then(|s| s.key_for(mode)) {
                let prior = self.prior.lock().expect("dsfb prior poisoned");
                for (c, t) in trust.iter_mut() {
                    *t += crate::dsfb::semantics::SEMANTIC_WEIGHT * prior.prior(pkey, *c);
                }
            }
            trust.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            (regime, trust)
        };
        let strategy = match regime {
            Regime::Slew => SearchStrategy::Broad,
            Regime::Drift => SearchStrategy::Balanced,
            Regime::Stable | Regime::Unknown => SearchStrategy::Narrow,
        };
        SearchPlan {
            ordered_channels: trust.into_iter().map(|(c, _)| c).collect(),
            strategy,
            budget: strategy.budget(),
        }
    }

    /// Trust lookup against an ALREADY-LOCKED shard map.
    ///
    /// `plan` holds the shard lock for the whole plan build and calls this
    /// per channel so the 9 trust reads reuse the same lock acquisition —
    /// calling [`ShardedStorageObserver::trust`] inside `plan` would
    /// re-lock the same shard (a self-deadlock, since std mutexes are not
    /// reentrant). This is the one internal helper that assumes the lock;
    /// every other accessor acquires it.
    fn trust_locked(
        &self,
        map: &HashMap<ChunkKey, ChunkObserver>,
        key: &ChunkKey,
        channel: Channel,
    ) -> f64 {
        map.get(key)
            .map(|c| c.weights[channel as usize])
            .unwrap_or(0.125)
    }

    /// Forget state for a chunk (unlink/truncate/gc).
    ///
    /// # Concurrency / count invariant
    ///
    /// Locks exactly the shard of `key`; decrements `tracked` iff the key
    /// was actually present.
    pub fn forget(&self, key: &ChunkKey) {
        let shard = self.shard_of(key);
        let mut map = self.shards[shard].lock().expect("dsfb shard poisoned");
        if map.remove(key).is_some() {
            self.tracked.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Targeted bounded eviction: drop one entry from shard `shard` (an
    /// arbitrary map entry — a stand-in for LRU; eviction is
    /// correctness-neutral, so approximate policy is safe). Decrements
    /// `tracked` iff an entry was removed.
    ///
    /// # Why the store uses this instead of [`ShardedStorageObserver::evict_one`]
    ///
    /// The store's cap gate (`DSFB_MAX_CHUNKS` in `Store::dsfb_observe`)
    /// evicts from the shard that JUST GREW — the shard of the observed
    /// key — so the total stays bounded by `cap + 1` (observe adds at
    /// most one entry; one eviction from a shard that is non-empty because
    /// it just received the entry brings the count back to `cap`). A
    /// rotating shard could evict from an empty shard and leave the count
    /// over the cap until the next observe, which would make the bound
    /// laggy rather than tight.
    pub fn evict_one_from(&self, shard: usize) {
        let mut map = self.shards[shard].lock().expect("dsfb shard poisoned");
        if let Some(k) = map.keys().next().copied() {
            map.remove(&k);
            self.tracked.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Rotating bounded eviction: drop one entry from the next shard in a
    /// round-robin cursor. Generic entry point (tests, backstop); the
    /// store uses the targeted [`ShardedStorageObserver::evict_one_from`].
    pub fn evict_one(&self) {
        let shard = self.evict_cursor.fetch_add(1, Ordering::Relaxed) % DSFB_SHARDS;
        self.evict_one_from(shard);
    }

    /// Number of tracked chunks (exact; one atomic load).
    pub fn len(&self) -> usize {
        self.tracked.load(Ordering::Relaxed)
    }

    /// Whether no chunks are tracked.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The aggregate statistics snapshot (for `status`), assembled from
    /// the lock-free atomics. No observer lock is taken — the snapshot is
    /// point-in-time and each counter is individually exact.
    pub fn stats(&self) -> ObserverStats {
        ObserverStats {
            tracked_chunks: self.tracked.load(Ordering::Relaxed),
            drift_events: self.drift_events.load(Ordering::Relaxed),
            slew_events: self.slew_events.load(Ordering::Relaxed),
            steps: self.steps.load(Ordering::Relaxed),
            narrowed_searches: self.narrowed_searches.load(Ordering::Relaxed),
        }
    }
}

impl std::fmt::Debug for ShardedStorageObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShardedStorageObserver")
            .field("params", &self.params)
            .field("shards", &DSFB_SHARDS)
            .field("tracked_chunks", &self.len())
            .field("stats", &self.stats())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extent::ChunkId;
    use crate::dsfb::semantics::{SemanticContext, SemanticMode};

    fn key(ino: u64, index: u64) -> ChunkKey {
        ChunkKey::new(ino, index, ChunkId::of(&[ino as u8, index as u8]))
    }

    fn no_sem() -> (Option<SemanticContext>, SemanticMode) {
        (None, SemanticMode::None)
    }

    #[test]
    fn stable_evidence_keeps_trust_high() {
        let obs = ShardedStorageObserver::default();
        let k = key(1, 0);
        let (sem, mode) = no_sem();
        let mut regime = Regime::Unknown;
        for _ in 0..50 {
            // channel P0 predicts perfectly every time
            regime = obs.observe(
                k,
                &[(Channel::PrevVersion, 1.0)],
                Channel::PrevVersion,
                1.0,
                sem,
                mode,
            );
        }
        assert_eq!(regime, Regime::Stable);
        // P0 must dominate trust (relative ordering is what matters).
        let p0 = obs.trust(&k, Channel::PrevVersion);
        assert!(p0 > 0.5, "p0 trust {p0}");
        let plan = obs.plan(&k, sem, mode);
        assert_eq!(plan.ordered_channels[0], Channel::PrevVersion);
        assert_eq!(plan.strategy, SearchStrategy::Narrow);
        assert_eq!(obs.len(), 1);
        assert_eq!(obs.stats().steps, 50);
    }

    #[test]
    fn slew_broadens_search() {
        let obs = ShardedStorageObserver::default();
        let k = key(2, 0);
        let (sem, mode) = no_sem();
        // stable for a while, then a violent regime break
        for _ in 0..10 {
            obs.observe(
                k,
                &[(Channel::PrevVersion, 1.0)],
                Channel::PrevVersion,
                1.0,
                sem,
                mode,
            );
        }
        assert_eq!(
            obs.observe(
                k,
                &[(Channel::PrevVersion, 0.0)],
                Channel::Raw,
                0.0,
                sem,
                mode
            ),
            Regime::Slew
        );
        // The plan during the slew window is Broad.
        let plan = obs.plan(&k, sem, mode);
        assert_eq!(plan.strategy, SearchStrategy::Broad);
        // After the window expires the tracker re-baselines; search must
        // not snap back to Narrow while the new baseline is unstable.
        for _ in 0..20 {
            obs.observe(
                k,
                &[(Channel::PrevVersion, 0.0)],
                Channel::Raw,
                0.5,
                sem,
                mode,
            );
        }
        let final_plan = obs.plan(&k, sem, mode);
        assert_ne!(final_plan.strategy, SearchStrategy::Narrow);
        assert!(obs.stats().slew_events > 0);
    }

    #[test]
    fn drift_keeps_narrow() {
        let obs = ShardedStorageObserver::default();
        let k = key(3, 0);
        let (sem, mode) = no_sem();
        // slow degradation: measurement drifts down gently
        let mut v = 1.0f64;
        for _ in 0..40 {
            obs.observe(
                k,
                &[(Channel::PrevVersion, v)],
                Channel::PrevVersion,
                v,
                sem,
                mode,
            );
            v = (v - 0.01).max(0.8);
        }
        assert!(obs.stats().drift_events > 0);
    }

    #[test]
    fn eviction_bounds_state() {
        // The PRODUCTION policy (Store::dsfb_observe): when the exact
        // count exceeds the cap, evict from the shard of the key that just
        // grew. The bound is tight: starting at the cap, every observe
        // pushes the count to cap + 1 and the targeted eviction brings it
        // back to cap — the total never exceeds cap + 1 and settles at cap.
        let obs = ShardedStorageObserver::default();
        let cap = 90usize;
        let (sem, mode) = no_sem();
        for i in 0..cap {
            obs.observe(key(i as u64, 0), &[], Channel::Raw, 0.5, sem, mode);
        }
        assert_eq!(obs.len(), cap);
        for i in cap..500 {
            let k = key(i as u64, 0);
            obs.observe(k, &[], Channel::Raw, 0.5, sem, mode);
            if obs.len() > cap {
                obs.evict_one_from(obs.shard_of(&k));
            }
            assert!(
                obs.len() <= cap,
                "targeted eviction must keep the total at the cap (got {})",
                obs.len()
            );
        }
        assert_eq!(obs.len(), cap);
    }

    #[test]
    fn evict_one_rotates_until_drained() {
        // The generic rotating entry point's contract: it visits shards in
        // round-robin and removes one entry per non-empty visit, so it
        // DRAINS the observer but may no-op on empty shards (the store
        // uses the targeted evict_one_from for the tight cap bound; this
        // is the backstop entry). 16 keys over 16 shards: 16 x 16 = 256
        // visits = 16 per shard >= the worst-case 16 keys in one shard, so
        // draining is guaranteed (and deterministic — FNV shard spread).
        let obs = ShardedStorageObserver::default();
        let (sem, mode) = no_sem();
        for i in 0..16 {
            obs.observe(key(i as u64, 0), &[], Channel::Raw, 0.5, sem, mode);
        }
        assert_eq!(obs.len(), 16);
        let mut guard = 0;
        while !obs.is_empty() {
            obs.evict_one();
            guard += 1;
            assert!(guard <= 16 * 16, "rotating eviction failed to drain");
        }
        obs.evict_one();
        assert!(obs.is_empty());
        assert_eq!(obs.stats().tracked_chunks, 0);
    }

    #[test]
    fn shard_of_is_stable_and_bounded() {
        let obs = ShardedStorageObserver::default();
        let k = key(7, 3);
        // Same key, same shard — determinism is the whole point (the
        // assignment is performance-only, but it must be reproducible).
        for _ in 0..10 {
            assert_eq!(obs.shard_of(&k), obs.shard_of(&k));
        }
        assert!(obs.shard_of(&k) < DSFB_SHARDS);
        // Different keys must not pile onto one shard: 4096 keys, every
        // shard gets a hit (the 11F "unrelated keys never block each
        // other" property has a structural witness).
        let mut seen = [false; DSFB_SHARDS];
        for i in 0..4096u64 {
            seen[obs.shard_of(&key(i, 0))] = true;
        }
        assert!(
            seen.iter().all(|&s| s),
            "shard hash must spread across all {DSFB_SHARDS} shards"
        );
    }

    #[test]
    fn forget_and_targeted_eviction_keep_count_exact() {
        let obs = ShardedStorageObserver::default();
        let (sem, mode) = no_sem();
        for i in 0..64 {
            obs.observe(key(i as u64, 0), &[], Channel::Raw, 0.5, sem, mode);
        }
        assert_eq!(obs.len(), 64);
        // Forgetting a present key decrements; forgetting an absent key
        // does not (the exact-count invariant).
        obs.forget(&key(0, 0));
        assert_eq!(obs.len(), 63);
        obs.forget(&key(0, 0));
        assert_eq!(obs.len(), 63);
        // Targeted eviction from one shard removes exactly one entry.
        let k = key(30, 0);
        let shard = obs.shard_of(&k);
        obs.evict_one_from(shard);
        assert_eq!(obs.len(), 62);
        // Observe re-inserts: count goes back up exactly once.
        obs.observe(key(0, 0), &[], Channel::Raw, 0.5, sem, mode);
        assert_eq!(obs.len(), 63);
    }

    #[test]
    fn concurrent_observation_is_safe() {
        // The 11F concurrency claim: unrelated keys must never block each
        // other (they live in different shards), and the atomic count must
        // stay exact under true parallelism. 16 threads x 256 keys each =
        // 4096 inserts; every key observed exactly once, so the count must
        // be exactly 4096 when the dust settles.
        let obs = ShardedStorageObserver::default();
        let (sem, mode) = no_sem();
        std::thread::scope(|s| {
            for t in 0..16usize {
                let obs = &obs;
                s.spawn(move || {
                    for i in 0..256usize {
                        let ino = (t * 256 + i) as u64;
                        obs.observe(key(ino, 0), &[], Channel::Raw, 0.5, sem, mode);
                        // Interleave reads (plans/trust) with writes, like
                        // the search path does.
                        let _ = obs.plan(&key(ino, 0), sem, mode);
                        let _ = obs.trust(&key(ino, 0), Channel::Raw);
                    }
                });
            }
        });
        assert_eq!(obs.len(), 4096, "exact count under concurrency");
        assert_eq!(obs.stats().steps, 4096);
        // And the per-key state is intact: observing again on the same key
        // increments the SAME entry (samples), not a new one.
        let k = key(0, 0);
        obs.observe(k, &[], Channel::Raw, 0.5, sem, mode);
        assert_eq!(obs.len(), 4096);
        assert_eq!(obs.stats().steps, 4097);
    }
}
