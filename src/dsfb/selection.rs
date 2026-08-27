//! Candidate-search plans: DSFB's only output that affects the pipeline —
//! an *ordered* list of channels to try and a budget. Selection of the
//! winning representation is always exact cost (ADR-0004/0010).
//!
//! # Purpose
//!
//! Translate a chunk's regime and trust vector into the only artifact the
//! search consumes from DSFB (`src/optimizer/search.rs`): a total order
//! over channels plus a budget that bounds how many ordered positions are
//! evaluated. The winning representation is always chosen by exact
//! deterministic cost (ADR-0010) after byte-exact validation (§32) — this
//! plan only decides what gets *searched for*, and in what order.
//!
//! # Units
//!
//! Budgets are **candidate counts** — the number of ordered plan positions
//! that may be evaluated — not bytes and not a cap on the whole search.
//! The always-on families (exact dedup, structural, rANS, RAW) are
//! evaluated regardless; only the budgeted base/universe channels
//! (`BUDGETED_CHANNELS` in `src/optimizer/search.rs`) are consumed from
//! the plan.
//!
//! # Regime → strategy mapping
//!
//! `ShardedStorageObserver::plan` maps: Stable/Unknown → Narrow (4),
//! Drift → Balanced (12), Slew → Broad (32). Drift keeps the basis — the
//! winning channel ranks first by trust — with a mid budget so slow
//! residual evolution is re-checked without paying full search cost; slew
//! broadens to everything; stable narrows to the trusted head of the
//! order.
//!
//! # Boundary
//!
//! A plan never decides which representation wins; it contains only
//! evaluation order and a budget. It is a value type produced under the
//! store's DSFB mutex and consumed lock-free afterwards.
//!
//! # Invariants
//!
//! - `budget` equals `SearchStrategy::budget()` for the carried strategy.
//! - `should_evaluate` is true exactly for positions `< budget` whose
//!   channel sits at that position — the caller iterates positions in
//!   order, so this is the budget gate.

#![forbid(unsafe_code)]

use crate::dsfb::features::Channel;

/// Search strategy (regime-derived): the breadth the search may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStrategy {
    /// Stable (or no evidence yet): trusted channels first, cheap
    /// candidates, small budget — 4 candidate positions.
    Narrow,
    /// Drift: all channels in trust order, balanced budget — 12 candidate
    /// positions.
    Balanced,
    /// Slew: everything, larger budget, deep search allowed — 32
    /// candidate positions.
    Broad,
}

impl SearchStrategy {
    /// Candidate budget for the strategy. Units: the number of ordered
    /// plan positions that may be evaluated (candidate counts, not
    /// bytes).
    pub const fn budget(self) -> usize {
        match self {
            SearchStrategy::Narrow => 4,
            SearchStrategy::Balanced => 12,
            SearchStrategy::Broad => 32,
        }
    }
}

/// The ordered candidate-search plan for one chunk.
///
/// Role: the complete DSFB output consumed by the search.
/// `ordered_channels` is a total order over all channels
/// (trust-descending); `strategy` and `budget` bound how many of those
/// positions are actually evaluated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPlan {
    /// Channels in the order they should be evaluated (trust-descending).
    pub ordered_channels: Vec<Channel>,
    /// Regime-derived strategy.
    pub strategy: SearchStrategy,
    /// Maximum number of candidates to evaluate (candidate count, not
    /// bytes).
    pub budget: usize,
}

impl SearchPlan {
    /// Whether a channel is within the plan's evaluation-order prefix:
    /// `position < budget` and the channel sits at that position. The
    /// caller walks `ordered_channels` by position, so this is the budget
    /// gate — channels beyond the prefix are not evaluated under DSFB
    /// ranking.
    pub fn should_evaluate(&self, channel: Channel, position: usize) -> bool {
        position < self.budget && self.ordered_channels.get(position) == Some(&channel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_order_is_trust_descending() {
        let plan = SearchPlan {
            ordered_channels: vec![
                Channel::PrevVersion,
                Channel::Rans,
                Channel::Raw,
                Channel::Adjacent,
                Channel::SharedContent,
                Channel::PrevInFile,
                Channel::FamilyBase,
                Channel::Universe,
            ],
            strategy: SearchStrategy::Balanced,
            budget: 12,
        };
        assert!(plan.should_evaluate(Channel::PrevVersion, 0));
        assert!(plan.should_evaluate(Channel::Rans, 1));
    }
}
