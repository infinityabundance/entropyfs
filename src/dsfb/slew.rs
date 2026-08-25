//! Slew classification: abrupt residual-structure changes.

#![forbid(unsafe_code)]

use crate::dsfb::features::Features;

/// A slew detector operating on per-write evidence (independent of the
/// observer's α, as a second opinion).
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
#[derive(Debug, Clone)]
pub struct SlewDetector {
    /// EMA of diff density.
    density_ema: f64,
    /// EMA of histogram change.
    hist_ema: f64,
    /// Jump thresholds (multiples of the EMA).
    density_factor: f64,
    hist_factor: f64,
    /// Samples seen.
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

    /// Feed one evidence observation; returns the slew signal.
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
