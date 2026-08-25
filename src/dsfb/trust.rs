//! Trust bookkeeping: which predictors are trusted, and the search-plan
//! budget consequences (`docs/theory/dsfb-selection.md` §3).

#![forbid(unsafe_code)]

use crate::dsfb::features::Channel;

/// Trust-weighted summary of a channel's history.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrustSummary {
    /// The channel.
    pub channel: Channel,
    /// Current trust weight in [0, 1].
    pub weight: f64,
    /// EMA of absolute residual evidence (lower = better).
    pub residual_ema: f64,
    /// Number of observations.
    pub observations: u64,
}

impl TrustSummary {
    /// Whether this channel is trusted enough to be searched first.
    pub fn trusted(&self, threshold: f64) -> bool {
        self.weight >= threshold
    }
}

/// Search breadth implied by the regime
/// (`docs/theory/dsfb-selection.md` §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchBreadth {
    /// Try only the top trusted channels, cheap families first.
    Narrow,
    /// Try all channels, all cheap families.
    Balanced,
    /// Try everything, including expensive families and deep search.
    Broad,
}

impl SearchBreadth {
    /// Breadth for a regime.
    pub const fn for_regime(regime: crate::dsfb::drift::Regime) -> SearchBreadth {
        match regime {
            crate::dsfb::drift::Regime::Stable => SearchBreadth::Narrow,
            crate::dsfb::drift::Regime::Drift => SearchBreadth::Balanced,
            crate::dsfb::drift::Regime::Slew => SearchBreadth::Broad,
            crate::dsfb::drift::Regime::Unknown => SearchBreadth::Balanced,
        }
    }

    /// Candidate count budget for this breadth (foreground path).
    pub const fn candidate_budget(self) -> usize {
        match self {
            SearchBreadth::Narrow => 4,
            SearchBreadth::Balanced => 12,
            SearchBreadth::Broad => 32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breadth_budget_ordering() {
        assert!(
            SearchBreadth::Broad.candidate_budget() > SearchBreadth::Balanced.candidate_budget()
        );
        assert!(
            SearchBreadth::Balanced.candidate_budget() > SearchBreadth::Narrow.candidate_budget()
        );
    }
}
