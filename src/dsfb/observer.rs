//! The storage observer: wraps the published `dsfb` crate and adapts it to
//! per-chunk predictor-channel evidence.
//!
//! # Purpose
//!
//! Maintain the per-chunk evidence state that steers candidate search:
//! per-channel trust (from EMA residuals), the drift–slew regime, and the
//! resulting search plan. Fed once per write from `encode_guided`
//! ([`StorageObserver::observe`]), consulted before the budgeted search
//! ([`StorageObserver::plan`], [`StorageObserver::trust`]).
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
//! budget in [`StorageObserver::plan`]: Slew → Broad/32,
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
//!
//! # Concurrency
//!
//! `StorageObserver` is not internally synchronized; the store owns a
//! single mutex around it (`Store::dsfb`). Every accessor runs under that
//! mutex, so operations are serialized and each is O(channels).
//!
//! # Resource bounds
//!
//! One [`ChunkObserver`] per distinct key; the store caps the map at
//! `DSFB_MAX_CHUNKS` (100 000) and calls [`StorageObserver::evict_one`]
//! past the cap. Per-entry state is fixed-size arrays over the 9
//! channels. Distinct content versions are the only growth vector, and
//! the cap bounds it.
//!
//! # Performance
//!
//! [`StorageObserver::observe`] is O(channels) with stack-only scratch;
//! [`StorageObserver::plan`] sorts 9 elements. Both run on the write
//! path, so allocation is avoided. The exhaustive-search alternative is
//! the `no-dsfb` ablation mode (H3).
//!
//! # Failure modes
//!
//! Infallible by construction. Out-of-range measurements are clamped; a
//! NaN would propagate into the EMAs (the caller contract is bounded
//! evidence — `Features::measurement` and `measurement_for_ratio` both
//! clamp). Wrong predictions cost search CPU only.
//!
//! # History / evidence
//!
//! Phase 4 (channels P0–P5), Phase-9C (P8 SharedDict, v0.4.0), ADR-0004,
//! H3 ablation methodology (`docs/theory/dsfb-selection.md` §5), upstream
//! audit (`docs/research/upstream-audit.md` §2).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::dsfb::drift::{MeasurementTracker, Regime};
use crate::dsfb::features::{Channel, ChunkKey};
use crate::dsfb::selection::{SearchPlan, SearchStrategy};

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
    /// overwrites every entry, and `StorageObserver::trust` falls back to
    /// 0.125 only for unknown chunks / never-observed channels.
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

/// The storage DSFB observer: a map of per-chunk observers plus global
/// statistics. Bounded by the chunk count touched: the store evicts past
/// `DSFB_MAX_CHUNKS` via [`StorageObserver::evict_one`]; state is
/// performance-only (ADR-0004).
pub struct StorageObserver {
    params: StorageDsfbParams,
    chunks: HashMap<ChunkKey, ChunkObserver>,
    /// Aggregate stats for `status` output.
    pub stats: ObserverStats,
}

impl std::fmt::Debug for StorageObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageObserver")
            .field("params", &self.params)
            .field("tracked_chunks", &self.chunks.len())
            .field("stats", &self.stats)
            .finish()
    }
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

impl Default for StorageObserver {
    fn default() -> Self {
        Self::new(StorageDsfbParams::default())
    }
}

impl StorageObserver {
    /// Create with parameters.
    pub fn new(params: StorageDsfbParams) -> Self {
        Self {
            params,
            chunks: HashMap::new(),
            stats: ObserverStats::default(),
        }
    }

    /// Feed one write-observation for a chunk. `measurements` maps each
    /// evaluated channel to its bounded measurement; unevaluated channels
    /// are fed 0.0 (no evidence) so evaluated channels dominate trust.
    /// `outcome_quality` is the quality of the winning representation
    /// (1.0 for a perfect structural/generated win, the channel's
    /// measurement for base/rANS/RAW-driven wins); the regime tracker
    /// consumes it so structural wins never look like a regime break.
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
    /// Returns the new regime. Performance-only: regime and internal state
    /// feed search ordering/budget, never the committed representation
    /// (ADR-0004/0010).
    pub fn observe(
        &mut self,
        key: ChunkKey,
        measurements: &[(Channel, f64)],
        winner: Channel,
        outcome_quality: f64,
    ) -> Regime {
        self.stats.steps += 1;
        let mut m = [0.0f64; Channel::ALL.len()];
        for &(c, v) in measurements {
            m[c as usize] = v.clamp(0.0, 1.0);
        }
        let winner_measurement = outcome_quality.clamp(0.0, 1.0);
        let entry = self.chunks.entry(key).or_insert_with(|| ChunkObserver {
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
            Regime::Drift => self.stats.drift_events += 1,
            Regime::Slew => self.stats.slew_events += 1,
            Regime::Stable | Regime::Unknown => {}
        }
        entry.regime = regime;
        self.stats.tracked_chunks = self.chunks.len();
        regime
    }

    /// Trust weight for a channel of a chunk (0 = fully distrusted).
    /// Unknown chunks and never-observed channels return the 0.125
    /// placeholder — equal, unearned weight — so a fresh chunk starts
    /// with no channel preferred.
    pub fn trust(&self, key: &ChunkKey, channel: Channel) -> f64 {
        self.chunks
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
    pub fn plan(&self, key: &ChunkKey) -> SearchPlan {
        let regime = self
            .chunks
            .get(key)
            .map(|c| c.regime)
            .unwrap_or(Regime::Unknown);
        let mut trust: Vec<(Channel, f64)> = Channel::ALL
            .iter()
            .map(|&c| (c, self.trust(key, c)))
            .collect();
        trust.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
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

    /// Forget state for a chunk (unlink/truncate/gc).
    pub fn forget(&mut self, key: &ChunkKey) {
        self.chunks.remove(key);
        self.stats.tracked_chunks = self.chunks.len();
    }

    /// Bounded eviction: drop one entry (simplified — the first map entry,
    /// an arbitrary stand-in for LRU). The store owns the real policy
    /// (`DSFB_MAX_CHUNKS` gate in `Store::dsfb_observe`); this is the
    /// safety valve so observer state stays bounded. Any eviction is safe:
    /// the state is performance-only.
    pub fn evict_one(&mut self) {
        if let Some(k) = self.chunks.keys().next().copied() {
            self.chunks.remove(&k);
            self.stats.tracked_chunks = self.chunks.len();
        }
    }

    /// Number of tracked chunks.
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// Whether no chunks are tracked.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extent::ChunkId;

    fn key(ino: u64, index: u64) -> ChunkKey {
        ChunkKey::new(ino, index, ChunkId::of(&[ino as u8, index as u8]))
    }

    #[test]
    fn stable_evidence_keeps_trust_high() {
        let mut obs = StorageObserver::default();
        let k = key(1, 0);
        let mut regime = Regime::Unknown;
        for _ in 0..50 {
            // channel P0 predicts perfectly every time
            regime = obs.observe(k, &[(Channel::PrevVersion, 1.0)], Channel::PrevVersion, 1.0);
        }
        assert_eq!(regime, Regime::Stable);
        // P0 must dominate trust (relative ordering is what matters).
        let p0 = obs.trust(&k, Channel::PrevVersion);
        assert!(p0 > 0.5, "p0 trust {p0}");
        let plan = obs.plan(&k);
        assert_eq!(plan.ordered_channels[0], Channel::PrevVersion);
        assert_eq!(plan.strategy, SearchStrategy::Narrow);
    }

    #[test]
    fn slew_broadens_search() {
        let mut obs = StorageObserver::default();
        let k = key(2, 0);
        // stable for a while, then a violent regime break
        for _ in 0..10 {
            obs.observe(k, &[(Channel::PrevVersion, 1.0)], Channel::PrevVersion, 1.0);
        }
        assert_eq!(
            obs.observe(k, &[(Channel::PrevVersion, 0.0)], Channel::Raw, 0.0),
            Regime::Slew
        );
        // The plan during the slew window is Broad.
        let plan = obs.plan(&k);
        assert_eq!(plan.strategy, SearchStrategy::Broad);
        // After the window expires the tracker re-baselines; search must
        // not snap back to Narrow while the new baseline is unstable.
        for _ in 0..20 {
            obs.observe(k, &[(Channel::PrevVersion, 0.0)], Channel::Raw, 0.5);
        }
        let final_plan = obs.plan(&k);
        assert_ne!(final_plan.strategy, SearchStrategy::Narrow);
        assert!(obs.stats.slew_events > 0);
    }

    #[test]
    fn drift_keeps_narrow() {
        let mut obs = StorageObserver::default();
        let k = key(3, 0);
        // slow degradation: measurement drifts down gently
        let mut v = 1.0f64;
        for _ in 0..40 {
            obs.observe(k, &[(Channel::PrevVersion, v)], Channel::PrevVersion, v);
            v = (v - 0.01).max(0.8);
        }
        assert!(obs.stats.drift_events > 0);
    }

    #[test]
    fn eviction_bounds_state() {
        let mut obs = StorageObserver::default();
        for i in 0..100 {
            obs.observe(key(i, 0), &[], Channel::Raw, 0.5);
        }
        assert_eq!(obs.len(), 100);
        for _ in 0..100 {
            obs.evict_one();
        }
        assert!(obs.is_empty());
    }
}
