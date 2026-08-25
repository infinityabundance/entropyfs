//! Optimization options and ablation gates (spec §43).
//!
//! Every claimed benefit must be attributable. `OptimizeOptions` toggles
//! whole candidate families/channels so ablation benchmarks can isolate:
//! RAW-only, RAW+rANS, +exact dedup, +base residuals, +configurational
//! coding, +entropy universes, +DSFB ranking, +background optimizer.

#![forbid(unsafe_code)]

use crate::dsfb::features::Channel;

/// Which candidate families and channels are enabled for a search.
///
/// All toggles default to on; ablation runs flip them off one at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptimizeOptions {
    /// Exact deduplication (P2) via the chunk index.
    pub allow_dedup: bool,
    /// Configurational families: SPARSE, PALETTE, PERIODIC, PERMUTATION.
    pub allow_configurational: bool,
    /// rANS coding (P6).
    pub allow_rans: bool,
    /// Base+residual channels (P0/P1/P3/P4).
    pub allow_bases: bool,
    /// Entropy universe candidates (P5) — negative control.
    pub allow_universe: bool,
    /// DSFB plan ordering + budget (off ⇒ evaluate everything, no budget).
    pub allow_dsfb_ranking: bool,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self {
            allow_dedup: true,
            allow_configurational: true,
            allow_rans: true,
            allow_bases: true,
            allow_universe: true,
            allow_dsfb_ranking: true,
        }
    }
}

impl OptimizeOptions {
    /// The RAW-only ablation (nothing but RAW).
    pub const fn raw_only() -> Self {
        Self {
            allow_dedup: false,
            allow_configurational: false,
            allow_rans: false,
            allow_bases: false,
            allow_universe: false,
            allow_dsfb_ranking: false,
        }
    }

    /// RAW + rANS.
    pub const fn raw_rans() -> Self {
        Self {
            allow_dedup: false,
            allow_configurational: false,
            allow_rans: true,
            allow_bases: false,
            allow_universe: false,
            allow_dsfb_ranking: false,
        }
    }

    /// Whether a channel may be evaluated at all.
    pub const fn channel_allowed(&self, channel: Channel) -> bool {
        match channel {
            Channel::SharedContent => self.allow_dedup,
            Channel::PrevVersion
            | Channel::Adjacent
            | Channel::PrevInFile
            | Channel::FamilyBase => self.allow_bases,
            Channel::Universe => self.allow_universe,
            Channel::Rans => self.allow_rans,
            Channel::Raw => true,
        }
    }

    /// Every ablation configuration (spec §43, methodology §4 ladder
    /// A0–A8): the single source of truth for the CLI and the evidence
    /// campaign.
    pub fn ablation_modes() -> Vec<(&'static str, OptimizeOptions)> {
        vec![
            ("full", OptimizeOptions::default()),
            ("raw", OptimizeOptions::raw_only()),
            ("raw-rans", OptimizeOptions::raw_rans()),
            (
                "no-dedup",
                OptimizeOptions {
                    allow_dedup: false,
                    ..Default::default()
                },
            ),
            (
                "no-base",
                OptimizeOptions {
                    allow_bases: false,
                    ..Default::default()
                },
            ),
            (
                "no-config",
                OptimizeOptions {
                    allow_configurational: false,
                    ..Default::default()
                },
            ),
            (
                "no-rans",
                OptimizeOptions {
                    allow_rans: false,
                    ..Default::default()
                },
            ),
            (
                "no-universe",
                OptimizeOptions {
                    allow_universe: false,
                    ..Default::default()
                },
            ),
            (
                "no-dsfb",
                OptimizeOptions {
                    allow_dsfb_ranking: false,
                    ..Default::default()
                },
            ),
        ]
    }
}
