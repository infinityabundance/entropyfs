//! Candidate-search plans: DSFB's only output that affects the pipeline —
//! an *ordered* list of channels to try and a budget. Selection of the
//! winning representation is always exact cost (ADR-0004/0010).

#![forbid(unsafe_code)]

use crate::dsfb::features::Channel;

/// Search strategy (regime-derived).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStrategy {
    /// Stable: trusted channels first, cheap candidates, small budget.
    Narrow,
    /// No strong signal: all channels, balanced budget.
    Balanced,
    /// Slew: everything, larger budget, deep search allowed.
    Broad,
}

impl SearchStrategy {
    /// Candidate budget for the strategy.
    pub const fn budget(self) -> usize {
        match self {
            SearchStrategy::Narrow => 4,
            SearchStrategy::Balanced => 12,
            SearchStrategy::Broad => 32,
        }
    }
}

/// The ordered candidate-search plan for one chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPlan {
    /// Channels in the order they should be evaluated (trust-descending).
    pub ordered_channels: Vec<Channel>,
    /// Regime-derived strategy.
    pub strategy: SearchStrategy,
    /// Maximum number of candidates to evaluate.
    pub budget: usize,
}

impl SearchPlan {
    /// Whether a channel is within the plan's evaluation order prefix.
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
