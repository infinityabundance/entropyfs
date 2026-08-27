//! Trust bookkeeping: which predictors are trusted, and the search-plan
//! budget consequences (`docs/theory/dsfb-selection.md` §3).
//!
//! # Purpose
//!
//! The trust record that justifies search ordering: a per-channel weight
//! in [0, 1] (normalized across channels), the residual EMA it derives
//! from, and the observation count behind it. Also the regime→breadth
//! vocabulary (`SearchBreadth`) with its candidate budgets.
//!
//! # Model
//!
//! Weights come from `dsfb::trust::calculate_trust_weights` over the
//! per-channel EMA residuals of `|1 − y|` (evidence error): raw weight
//! `1/(σ0 + residual)`, then normalized across channels — a channel that
//! is consistently evaluated and accurate (residual ≈ 0) dominates; one
//! that is never evaluated keeps its placeholder weight. Trust only
//! orders the search and gates budget spending; it never selects a
//! representation (ADR-0004/0010).
//!
//! # Units and invariants
//!
//! - `weight ∈ [0, 1]`, normalized (vector sum ≈ 1 after the first
//!   observation); `residual_ema ∈ [0, 1]`, lower = better.
//! - Budgets are candidate counts (4 / 12 / 32), the same values as
//!   `SearchStrategy` in `selection.rs`. Note the Unknown case differs
//!   between the two vocabularies: `ShardedStorageObserver::plan` treats
//!   Unknown as Narrow, while `SearchBreadth::for_regime` treats Unknown as
//!   Balanced — the plan path is the one the search actually consumes.
//!
//! # Boundary
//!
//! Trust affects only evaluation order and budget. `TrustSummary` is the
//! per-channel reporting record; `SearchBreadth` is the regime→breadth
//! mapping kept in this module.

#![forbid(unsafe_code)]

use crate::dsfb::features::Channel;

/// Trust-weighted summary of a channel's history.
///
/// Role: the observable per-channel trust record (reporting and
/// `trusted()` gating). Invariants: `weight ∈ [0, 1]` (normalized across
/// channels after the first observation); `residual_ema ∈ [0, 1]` (EMA of
/// `|1 − y|`, lower = better); `observations` counts steps for this
/// channel.
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
    /// Whether this channel is trusted enough to be searched first:
    /// `weight >= threshold`. Thresholds are in the weight scale [0, 1]
    /// (the search's foreground gate uses 0.5 — `FOREGROUND_BASE_TRUST`).
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
    /// Breadth for a regime: Stable → Narrow, Drift → Balanced, Slew →
    /// Broad, Unknown → Balanced (no evidence yet ⇒ no reason to narrow
    /// the search). Note `ShardedStorageObserver::plan` maps Unknown to
    /// Narrow instead — see the module doc.
    pub const fn for_regime(regime: crate::dsfb::drift::Regime) -> SearchBreadth {
        match regime {
            crate::dsfb::drift::Regime::Stable => SearchBreadth::Narrow,
            crate::dsfb::drift::Regime::Drift => SearchBreadth::Balanced,
            crate::dsfb::drift::Regime::Slew => SearchBreadth::Broad,
            crate::dsfb::drift::Regime::Unknown => SearchBreadth::Balanced,
        }
    }

    /// Candidate count budget for this breadth (foreground path):
    /// Narrow 4, Balanced 12, Broad 32. Units: candidate counts, not
    /// bytes.
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
