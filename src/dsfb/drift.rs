//! Drift classification: the storage meaning of slow residual evolution.
//!
//! # Purpose
//!
//! Turn the bounded measurement series of a chunk's winning
//! representation (the outcome quality) into a regime —
//! Unknown / Stable / Drift / Slew — and the storage guidance that
//! follows. The regime is what the search budgeting consumes
//! (`selection.rs`): slew broadens and re-baselines, drift balances,
//! stable narrows.
//!
//! # Two classifiers
//!
//! - [`classify`] thresholds the raw (φ, ω, α) state: |α| > slew_alpha ⇒
//!   Slew, else |ω| > drift_omega ⇒ Drift, else Stable. It never returns
//!   Unknown and is retained for synthetic-state tests and reporting —
//!   the raw α accumulation has a permanent-velocity problem (a slow,
//!   steady change makes α grow until it trips the slew threshold), so
//!   the runtime path does not trust it.
//! - [`MeasurementTracker`] is the primary runtime classifier: it operates
//!   directly on the measurement series and is robust to that velocity
//!   accumulation. A fast EMA (α = 0.2) tracks current quality; a slow
//!   EMA (α = 0.1) of per-step deltas tracks the drift rate; a single-step
//!   |delta| > 0.25 opens a persistent slew window (8 steps) with early
//!   recovery when the measurement returns within 0.1 of the pre-slew EMA.
//!
//! # Units
//!
//! All tracker thresholds are in the measurement scale [0, 1], per step:
//! `SLEW_DELTA_THRESHOLD` = 0.25 (a quarter of the scale in one step),
//! `DRIFT_RATE_THRESHOLD` = 0.004 (per-step drift rate), `SLEW_WINDOW` =
//! 8 steps, `SLEW_RECOVERY_EPS` = 0.1. [`classify`]'s thresholds are in
//! the raw φ/ω/α scale, which is why the two do not share constants.
//!
//! # State machine (`MeasurementTracker::observe`)
//!
//! 1. First observation: seed the fast EMA, return Unknown.
//! 2. Observations 2–5: warm-up — the EMAs converge but the
//!    `samples > 4` gate keeps slew/drift from firing; the result is
//!    Stable.
//! 3. From observation 6: compute the delta, update both EMAs; a
//!    single-step |delta| > 0.25 opens the slew window (return Slew);
//!    else a drift rate above 0.004 returns Drift; else Stable.
//! 4. Inside a slew window the EMAs keep updating ("always track
//!    reality"), so the window expires after 8 steps and the tracker
//!    re-baselines on the current measurements; early recovery ends it
//!    sooner.
//!
//! # Correctness invariants
//!
//! - The EMAs update on every observation, including inside a slew
//!   window — a slew must end eventually and re-baseline, not stick
//!   forever.
//! - Slew is sticky for the window, never longer: each in-window
//!   observation decrements it and early recovery can clear it.
//! - A misclassification only widens or narrows the search (CPU), never
//!   the committed bytes (ADR-0004).
//!
//! # Failure modes
//!
//! Deterministic f64 arithmetic on bounded [0, 1] inputs; no `Result`, no
//! allocation. A NaN measurement would poison the EMAs (the caller
//! contract is bounded evidence). [`Guidance`] maps each regime to the
//! storage action vocabulary (keep basis / update residuals / new
//! baseline); the search acts on it via the strategy and budget mapping.

#![forbid(unsafe_code)]

use dsfb::DsfbState;

/// Representation-regime classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    /// No evidence yet.
    Unknown,
    /// Stable: residual structure constant, basis effective.
    Stable,
    /// Drift: residual structure changes slowly on a stable basis.
    Drift,
    /// Slew: residual structure changed abruptly (regime break).
    Slew,
}

/// Classify the raw (φ, ω, α) observer state into a regime.
///
/// Semantics (`docs/theory/dsfb-selection.md` §3):
/// - large |α| ⇒ slew (acceleration of change);
/// - small |α| and |ω| above drift floor ⇒ drift;
/// - small |α| and small |ω| ⇒ stable.
///
/// The primary runtime classifier is [`MeasurementTracker`] (which is
/// robust to the observer's permanent-velocity accumulation); this
/// function is retained for synthetic-state tests and reporting.
pub fn classify(state: DsfbState, slew_alpha: f64, drift_omega: f64) -> Regime {
    if state.alpha.abs() > slew_alpha {
        Regime::Slew
    } else if state.omega.abs() > drift_omega {
        Regime::Drift
    } else {
        Regime::Stable
    }
}

/// Two-timescale tracker over the bounded measurement series.
///
/// Robust drift/slew classification that does not depend on the
/// observer's accumulated velocity: a fast EMA tracks current quality, a
/// slow EMA of per-step deltas tracks the drift rate, and a single-step
/// jump beyond the slew threshold (with a persistence window) marks a
/// regime break. All thresholds are in the measurement scale [0, 1].
///
/// Invariants: `ema` and `delta_ema` are in [0, 1] / [−1, 1] respectively
/// for bounded inputs; `slew_window > 0` exactly while a declared slew is
/// in progress.
#[derive(Debug, Clone, Copy)]
pub struct MeasurementTracker {
    /// Fast EMA of the measurement (α = 0.2) — current quality.
    ema: f64,
    /// Slow EMA of per-step deltas (α = 0.1) — the drift rate, in
    /// measurement units per step.
    delta_ema: f64,
    /// Measurements seen. The warm-up gate is `samples > 4`.
    samples: u64,
    /// Slew persistence window remaining (steps). `> 0` means inside a
    /// declared slew; each in-window observation decrements it.
    slew_window: u64,
    /// Fast EMA at the moment the current slew was declared; early
    /// recovery compares new measurements against it.
    pre_slew_ema: f64,
}

/// Slew trigger: a single-step |delta| above this fraction of the scale.
pub const SLEW_DELTA_THRESHOLD: f64 = 0.25;
/// Drift trigger: |drift rate| above this (per-step, on [0,1] scale).
pub const DRIFT_RATE_THRESHOLD: f64 = 0.004;
/// Slew persistence window (steps).
pub const SLEW_WINDOW: u64 = 8;
/// Recovery: measurement within this distance of the pre-slew EMA ends a
/// slew early.
pub const SLEW_RECOVERY_EPS: f64 = 0.1;

impl Default for MeasurementTracker {
    fn default() -> Self {
        Self {
            ema: 0.0,
            delta_ema: 0.0,
            samples: 0,
            slew_window: 0,
            pre_slew_ema: 0.0,
        }
    }
}

impl MeasurementTracker {
    /// Feed one measurement; returns the classified regime.
    ///
    /// # Algorithm
    ///
    /// 1. First sample seeds the fast EMA and returns Unknown.
    /// 2. Update both EMAs from the new measurement and its delta.
    /// 3. If inside a slew window: decrement it, check early recovery,
    ///    and keep reporting Slew while it remains.
    /// 4. After the 5-sample warm-up, a single-step jump > 0.25 declares a
    ///    new slew (recording the pre-slew EMA); otherwise the drift rate
    ///    above 0.004 reports Drift, else Stable.
    ///
    /// The EMAs always update, even inside a slew window, so the window
    /// eventually expires and the tracker re-baselines.
    pub fn observe(&mut self, m: f64) -> Regime {
        self.samples += 1;
        if self.samples == 1 {
            self.ema = m;
            return Regime::Unknown;
        }
        let delta = m - self.ema;
        // Always track reality (the EMAs update even inside a slew window,
        // so the window eventually expires and the tracker re-baselines).
        self.delta_ema = 0.9 * self.delta_ema + 0.1 * delta;
        self.ema = 0.8 * self.ema + 0.2 * m;
        if self.slew_window > 0 {
            self.slew_window -= 1;
            // Early recovery ends the slew.
            if (m - self.pre_slew_ema).abs() < SLEW_RECOVERY_EPS {
                self.slew_window = 0;
            } else if self.slew_window > 0 {
                return Regime::Slew;
            }
        }
        if self.samples > 4 && delta.abs() > SLEW_DELTA_THRESHOLD {
            self.slew_window = SLEW_WINDOW;
            self.pre_slew_ema = self.ema;
            return Regime::Slew;
        }
        if self.samples > 4 && self.delta_ema.abs() > DRIFT_RATE_THRESHOLD {
            Regime::Drift
        } else {
            Regime::Stable
        }
    }
}

/// Storage guidance for a regime (consumed by the optimizer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guidance {
    /// Keep the basis; update small residuals; narrow search.
    KeepBasis,
    /// Update residuals; modest search breadth.
    UpdateResiduals,
    /// Drop the basis; broaden candidate search; establish new baseline.
    NewBaseline,
}

impl Guidance {
    /// Map a regime to guidance.
    pub const fn for_regime(regime: Regime) -> Guidance {
        match regime {
            Regime::Stable => Guidance::KeepBasis,
            Regime::Drift => Guidance::UpdateResiduals,
            Regime::Slew => Guidance::NewBaseline,
            Regime::Unknown => Guidance::UpdateResiduals,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(
            classify(DsfbState::new(0.0, 0.001, 0.0), 0.05, 0.02),
            Regime::Stable
        );
        assert_eq!(
            classify(DsfbState::new(0.0, 0.05, 0.0), 0.05, 0.02),
            Regime::Drift
        );
        assert_eq!(
            classify(DsfbState::new(0.0, 0.05, 0.1), 0.05, 0.02),
            Regime::Slew
        );
    }

    #[test]
    fn tracker_stable_on_constant() {
        let mut t = MeasurementTracker::default();
        assert_eq!(t.observe(1.0), Regime::Unknown);
        for _ in 0..20 {
            assert_eq!(t.observe(1.0), Regime::Stable);
        }
    }

    #[test]
    fn tracker_drift_on_gradual_decline() {
        let mut t = MeasurementTracker::default();
        t.observe(1.0);
        let mut v = 1.0;
        let mut saw_drift = false;
        for _ in 0..40 {
            v -= 0.01;
            if t.observe(v) == Regime::Drift {
                saw_drift = true;
            }
        }
        assert!(saw_drift);
    }

    #[test]
    fn tracker_slew_on_jump_with_persistence() {
        let mut t = MeasurementTracker::default();
        t.observe(1.0);
        for _ in 0..10 {
            t.observe(1.0);
        }
        assert_eq!(t.observe(0.0), Regime::Slew);
        // persists for the window
        for _ in 0..3 {
            assert_eq!(t.observe(0.0), Regime::Slew);
        }
        // eventually leaves the window
        let mut saw_exit = false;
        for _ in 0..20 {
            if t.observe(0.0) != Regime::Slew {
                saw_exit = true;
            }
        }
        assert!(saw_exit);
    }
}
