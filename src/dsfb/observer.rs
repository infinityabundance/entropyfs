//! The storage observer: wraps the published `dsfb` crate and adapts it to
//! per-chunk predictor-channel evidence.
//!
//! Per logical chunk (file, index) we maintain one `dsfb::DsfbObserver`
//! with 8 channels (P0..P7). Each write feeds one `step` with the bounded
//! measurement of every channel that was evaluated. The state (φ, ω, α)
//! drives regime classification (drift vs slew) and search budgeting.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::dsfb::drift::{MeasurementTracker, Regime};
use crate::dsfb::features::{Channel, ChunkKey};
use crate::dsfb::selection::{SearchPlan, SearchStrategy};

/// DSFB observer parameters for storage evidence.
///
/// Values chosen for the storage domain: measurements are bounded in
/// [0, 1], steps are sparse (per write, not per µs), so the EMA is slow
/// (high rho) and gains are gentle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StorageDsfbParams {
    /// Position gain (representation quality regime).
    pub k_phi: f64,
    /// Drift gain.
    pub k_omega: f64,
    /// Slew gain.
    pub k_alpha: f64,
    /// EMA smoothing (0 < rho < 1).
    pub rho: f64,
    /// Trust softness.
    pub sigma0: f64,
    /// dt per write step (fixed; the observer is clocked by writes).
    pub dt: f64,
    /// Slew threshold: |α| above this (scaled) classifies as slew.
    pub slew_alpha_threshold: f64,
    /// Drift threshold: |ω| below this (scaled) with small |α| is drift.
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

/// Per-chunk observer state.
struct ChunkObserver {
    /// The underlying drift–slew state observer (φ/ω/α), fed the last
    /// known measurement per channel. Reported via `state_*` accessors;
    /// regime classification uses the robust tracker.
    inner: dsfb::DsfbObserver,
    /// Robust regime tracker over the winner measurement series.
    tracker: MeasurementTracker,
    /// Per-channel EMA of `|1 − y|` (the evidence error). Initialized to 1
    /// (max distrust): channels earn trust by being evaluated.
    ema: [f64; 8],
    /// Normalized trust weights (via `dsfb::trust::calculate_trust_weights`).
    weights: [f64; 8],
    /// Last known measurement per channel.
    last_y: [f64; 8],
    /// Last classification.
    regime: Regime,
    /// Samples seen.
    samples: u64,
    /// Last winning channel.
    winner: Channel,
    /// Baseline content id (the basis the observer currently trusts).
    baseline: Option<crate::core::extent::ChunkId>,
}

/// The storage DSFB observer: a map of per-chunk observers plus global
/// statistics. Bounded by the chunk count touched (evicted by the cache
/// layer policy; state is performance-only).
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
    /// Chunks currently tracked.
    pub tracked_chunks: usize,
    /// Drift events observed.
    pub drift_events: u64,
    /// Slew events observed.
    pub slew_events: u64,
    /// Total steps fed.
    pub steps: u64,
    /// Total candidates skipped due to low trust (search narrowing).
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
    pub fn observe(
        &mut self,
        key: ChunkKey,
        measurements: &[(Channel, f64)],
        winner: Channel,
    ) -> Regime {
        self.stats.steps += 1;
        let mut m = [0.0f64; 8];
        for &(c, v) in measurements {
            m[c as usize] = v.clamp(0.0, 1.0);
        }
        let winner_measurement = m[winner as usize];
        let entry = self.chunks.entry(key).or_insert_with(|| ChunkObserver {
            inner: dsfb::DsfbObserver::new(self.params.dsfb_params(), 8),
            tracker: MeasurementTracker::default(),
            ema: [1.0; 8],
            weights: [0.125; 8],
            last_y: [0.0; 8],
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
        let mut scratch = [0.0f64; 8];
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
    pub fn trust(&self, key: &ChunkKey, channel: Channel) -> f64 {
        self.chunks
            .get(key)
            .map(|c| c.weights[channel as usize])
            .unwrap_or(0.125)
    }

    /// Build the candidate-search plan for a chunk: ordered channels by
    /// trust, with a budget scaled by regime (slew ⇒ broaden, drift ⇒
    /// narrow).
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

    /// Bounded eviction: drop the LRU-ish entry (simplified: drop the first
    /// entry). The cache layer owns the real eviction policy; this is a
    /// safety valve so observer state stays bounded.
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
            regime = obs.observe(k, &[(Channel::PrevVersion, 1.0)], Channel::PrevVersion);
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
            obs.observe(k, &[(Channel::PrevVersion, 1.0)], Channel::PrevVersion);
        }
        assert_eq!(
            obs.observe(k, &[(Channel::PrevVersion, 0.0)], Channel::Raw),
            Regime::Slew
        );
        // The plan during the slew window is Broad.
        let plan = obs.plan(&k);
        assert_eq!(plan.strategy, SearchStrategy::Broad);
        // After the window expires the tracker re-baselines; search must
        // not snap back to Narrow while the new baseline is unstable.
        for _ in 0..20 {
            obs.observe(k, &[(Channel::PrevVersion, 0.0)], Channel::Raw);
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
            obs.observe(k, &[(Channel::PrevVersion, v)], Channel::PrevVersion);
            v = (v - 0.01).max(0.8);
        }
        assert!(obs.stats.drift_events > 0);
    }

    #[test]
    fn eviction_bounds_state() {
        let mut obs = StorageObserver::default();
        for i in 0..100 {
            obs.observe(key(i, 0), &[], Channel::Raw);
        }
        assert_eq!(obs.len(), 100);
        for _ in 0..100 {
            obs.evict_one();
        }
        assert!(obs.is_empty());
    }
}
