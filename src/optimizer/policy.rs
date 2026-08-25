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
    /// Configurational families: SPARSE, PALETTE, PERIODIC, PERMUTATION,
    /// SPARSE_BLOCK64.
    pub allow_configurational: bool,
    /// rANS coding (P6).
    pub allow_rans: bool,
    /// Base+residual coding against the in-hand previous version (P0): the
    /// "base residuals" step of the cumulative ladder (methodology §4 A3).
    pub allow_bases: bool,
    /// Temporal candidate-base channels (P1 adjacent / P3 prev-in-file /
    /// P4 family-base) that materialize other chunks from the store (A5).
    pub allow_temporal_bases: bool,
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
            allow_temporal_bases: true,
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
            allow_temporal_bases: false,
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
            allow_temporal_bases: false,
            allow_universe: false,
            allow_dsfb_ranking: false,
        }
    }

    /// Whether a channel may be evaluated at all.
    pub const fn channel_allowed(&self, channel: Channel) -> bool {
        match channel {
            Channel::SharedContent => self.allow_dedup,
            Channel::PrevVersion => self.allow_bases,
            Channel::Adjacent | Channel::PrevInFile | Channel::FamilyBase => {
                self.allow_bases && self.allow_temporal_bases
            }
            Channel::Universe => self.allow_universe,
            Channel::Rans => self.allow_rans,
            Channel::Raw => true,
        }
    }

    /// Every ablation configuration (spec §43): leave-one-out gates — the
    /// single source of truth for the CLI and the evidence campaign.
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
                    allow_temporal_bases: false,
                    ..Default::default()
                },
            ),
            (
                "no-temporal",
                OptimizeOptions {
                    allow_temporal_bases: false,
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

    /// The strict cumulative ablation ladder (methodology §4, spec §43):
    /// each step adds exactly one mechanism on top of the previous. Step
    /// A8 additionally runs the background optimizer pass after the write
    /// (the only step the foreground write path does not include).
    ///
    /// Note: the engine has one base gate for the in-hand previous-version
    /// residual coding (A3) and one for the temporal base channels that
    /// materialize other chunks (A5); both map to the generic ladder's
    /// "base" steps without loss of granularity.
    pub fn cumulative_ladder_modes() -> Vec<(&'static str, OptimizeOptions, bool)> {
        vec![
            ("A0-raw", OptimizeOptions::raw_only(), false),
            ("A1-rans", OptimizeOptions::raw_rans(), false),
            // A2 = A1 + exact dedup.
            (
                "A2-dedup",
                OptimizeOptions {
                    allow_dedup: true,
                    allow_configurational: false,
                    allow_rans: true,
                    allow_bases: false,
                    allow_temporal_bases: false,
                    allow_universe: false,
                    allow_dsfb_ranking: false,
                },
                false,
            ),
            // A3 = A2 + base residuals (in-hand previous-version P0).
            (
                "A3-base-residual",
                OptimizeOptions {
                    allow_dedup: true,
                    allow_configurational: false,
                    allow_rans: true,
                    allow_bases: true,
                    allow_temporal_bases: false,
                    allow_universe: false,
                    allow_dsfb_ranking: false,
                },
                false,
            ),
            // A4 = A3 + configurational rank/unrank families.
            (
                "A4-config",
                OptimizeOptions {
                    allow_dedup: true,
                    allow_configurational: true,
                    allow_rans: true,
                    allow_bases: true,
                    allow_temporal_bases: false,
                    allow_universe: false,
                    allow_dsfb_ranking: false,
                },
                false,
            ),
            // A5 = A4 + temporal base channels (P1/P3/P4 materialization).
            (
                "A5-temporal-bases",
                OptimizeOptions {
                    allow_dedup: true,
                    allow_configurational: true,
                    allow_rans: true,
                    allow_bases: true,
                    allow_temporal_bases: true,
                    allow_universe: false,
                    allow_dsfb_ranking: false,
                },
                false,
            ),
            // A6 = A5 + entropy universes (negative control).
            (
                "A6-universe",
                OptimizeOptions {
                    allow_dedup: true,
                    allow_configurational: true,
                    allow_rans: true,
                    allow_bases: true,
                    allow_temporal_bases: true,
                    allow_universe: true,
                    allow_dsfb_ranking: false,
                },
                false,
            ),
            // A7 = A6 + DSFB candidate guidance.
            (
                "A7-dsfb",
                OptimizeOptions {
                    allow_dedup: true,
                    allow_configurational: true,
                    allow_rans: true,
                    allow_bases: true,
                    allow_temporal_bases: true,
                    allow_universe: true,
                    allow_dsfb_ranking: true,
                },
                false,
            ),
            // A8 = A7 + background re-optimization pass after the write.
            ("A8-full+background", OptimizeOptions::default(), true),
        ]
    }
}
