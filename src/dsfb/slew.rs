//! Slew classification: abrupt residual-structure changes.
//!
//! # Purpose
//!
//! A second-opinion slew detector on raw per-write evidence features
//! ([`Features`]), independent of the observer's α: when the diff density
//! or the histogram change jumps relative to its running EMA baseline,
//! the evidence itself says the chunk's byte structure changed abruptly.
//!
//! # Boundary
//!
//! This detector sees only [`Features`] — no φ/ω/α, no observer state. It
//! is not currently wired into `StorageObserver::observe`: the runtime
//! regime path uses `MeasurementTracker` (`drift.rs`); this is the
//! per-write evidence-domain counterpart kept as the second opinion.
//!
//! # Model
//!
//! Per chunk, two EMAs (ρ = 0.8): `density_ema` over `diff_density` and
//! `hist_ema` over `hist_change`. From the 6th observation on, a jump
//! requires all three gates to pass: the baseline EMA at or above 0.005 (a
//! feature that is consistently ~0 cannot meaningfully "jump"), the new
//! value above `factor × baseline` (default 4×), and an absolute delta
//! above 0.2. The relative and absolute gates together keep tiny baselines
//! from amplifying noise. When both signals fire, `DensityJump` is
//! reported: density is the direct per-byte residual proxy
//! (`residual_ratio == diff_density` in `Features::from_base`), while
//! `hist_change` measures the differing multiset (L1/2n) — a pure
//! permutation of the same bytes has maximal density but zero histogram
//! change, so the two are complementary signals. The EMAs update on every
//! observation, even when a jump is reported, so the baseline re-anchors
//! immediately.
//!
//! # Units
//!
//! `diff_density` and `hist_change` are in [0, 1]; `density_factor` and
//! `hist_factor` are dimensionless multiples of the baseline EMA; the 0.2
//! gate is an absolute difference on the [0, 1] scale; the 0.005 floor is
//! on the EMA scale.
//!
//! # Correctness invariants
//!
//! - A gradual change (per-step increments below both the factor and the
//!   absolute gates) never triggers a signal — regression-tested
//!   (`ignores_gradual_change`).
//! - Deterministic and allocation-free; no shared state.
//!
//! # Failure modes
//!
//! A spurious signal can only broaden a search (CPU); a missed signal
//! leaves the regime to `MeasurementTracker`. Both outcomes are
//! performance-only (ADR-0004).

#![forbid(unsafe_code)]

use crate::dsfb::features::Features;

/// A slew detector operating on per-write evidence (independent of the
/// observer's α, as a second opinion).
///
/// Signal precedence when both fire: `DensityJump` (density is the direct
/// residual proxy; see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlewSignal {
    /// No abrupt change.
    None,
    /// Diff density jumped relative to the running baseline.
    DensityJump,
    /// Histogram change jumped relative to the running baseline.
    HistJump,
}

/// Running slew detector state (per chunk).
///
/// Invariants: the EMAs are in [0, 1] for bounded features; classification
/// is gated by `samples > 5` (the first five observations only build the
/// baselines).
#[derive(Debug, Clone)]
pub struct SlewDetector {
    /// EMA of diff density (ρ = 0.8).
    density_ema: f64,
    /// EMA of histogram change (ρ = 0.8).
    hist_ema: f64,
    /// Jump thresholds (multiples of the EMA): the new value must exceed
    /// factor × baseline to count as a jump.
    density_factor: f64,
    hist_factor: f64,
    /// Samples seen; classification starts at sample 6.
    samples: u64,
}

impl Default for SlewDetector {
    fn default() -> Self {
        Self {
            density_ema: 0.0,
            hist_ema: 0.0,
            density_factor: 4.0,
            hist_factor: 4.0,
            samples: 0,
        }
    }
}

impl SlewDetector {
    /// New detector with custom jump factors.
    pub fn new(density_factor: f64, hist_factor: f64) -> Self {
        Self {
            density_factor,
            hist_factor,
            ..Self::default()
        }
    }

    /// Feed one evidence observation; returns the slew signal. The EMAs
    /// update on every call (even when a jump is reported); jumps report
    /// from sample 6 on (see the module doc for the gates).
    pub fn observe(&mut self, f: &Features) -> SlewSignal {
        self.samples += 1;
        let rho = 0.8f64;
        let mut sig = SlewSignal::None;
        if self.samples > 5 {
            let d_jump = self.density_ema >= 0.005
                && f.diff_density > self.density_ema * self.density_factor
                && f.diff_density - self.density_ema > 0.2;
            let h_jump = self.hist_ema >= 0.005
                && f.hist_change > self.hist_ema * self.hist_factor
                && f.hist_change - self.hist_ema > 0.2;
            if d_jump && h_jump {
                sig = SlewSignal::DensityJump;
            } else if h_jump {
                sig = SlewSignal::HistJump;
            } else if d_jump {
                sig = SlewSignal::DensityJump;
            }
        }
        self.density_ema = rho * self.density_ema + (1.0 - rho) * f.diff_density;
        self.hist_ema = rho * self.hist_ema + (1.0 - rho) * f.hist_change;
        sig
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsfb::features::Channel;

    fn feats(density: f64, hist: f64) -> Features {
        Features {
            channel: Channel::PrevVersion,
            residual_ratio: density,
            diff_density: density,
            diff_runs: 1,
            diff_positions: (density * 100.0) as u32,
            hist_change: hist,
            exact_match: false,
        }
    }

    #[test]
    fn detects_jump() {
        let mut d = SlewDetector::default();
        for _ in 0..10 {
            assert_eq!(d.observe(&feats(0.01, 0.01)), SlewSignal::None);
        }
        // abrupt change
        assert_ne!(d.observe(&feats(0.9, 0.9)), SlewSignal::None);
    }

    #[test]
    fn ignores_gradual_change() {
        let mut d = SlewDetector::default();
        let mut v = 0.01f64;
        for _ in 0..40 {
            d.observe(&feats(v, v));
            v += 0.005;
        }
        // gradual drift stays below the jump factor
        assert_eq!(d.observe(&feats(v, v)), SlewSignal::None);
    }
}
