//! Optimization options and ablation gates (spec §43).
//!
//! Every claimed benefit must be attributable. `OptimizeOptions` toggles
//! whole candidate families/channels so ablation benchmarks can isolate:
//! RAW-only, RAW+byte-rANS, +EXACT_REF aliasing, +base residuals,
//! +configurational coding, +entropy universes, +DSFB ranking,
//! +background optimizer, and the post-registration SequenceRans floor.
//!
//! Attribution model (Phase-8 review correction):
//!
//! - Content-addressed object sharing is a STORE INVARIANT, not a gate:
//!   identical payloads always hash to one `ChunkId` and the object index
//!   keeps one location per id. `allow_exact_ref` therefore gates only the
//!   *descriptor-level* EXACT_REF aliasing representation, never object
//!   sharing. The two layers are accounted separately in the evidence
//!   (`cas_shared_bytes_saved` vs `exact_ref_bytes_saved`).
//! - Byte-level rANS (`RansEncoder`) and SequenceRans (local-match +
//!   entropy, `SequenceEncoder`) are separate gates. The original
//!   methodology's A1 step is RAW + byte rANS; SequenceRans is a
//!   post-registration extension (ladder step E1) so A1 never silently
//!   includes the match finder.

#![forbid(unsafe_code)]

use crate::dsfb::features::Channel;

/// Which candidate families and channels are enabled for a search.
///
/// All toggles default to on; ablation runs flip them off one at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptimizeOptions {
    /// EXACT_REF aliasing (P2) via the chunk index: a duplicate logical
    /// chunk is stored as a reference to the canonical content id. This
    /// gates only the *representation*; content-addressed object sharing
    /// (identical payload → one `ChunkId`) is a store invariant and is
    /// never disabled by this flag.
    pub allow_exact_ref: bool,
    /// Configurational families: SPARSE, PALETTE, PERIODIC, PERMUTATION,
    /// SPARSE_BLOCK64.
    pub allow_configurational: bool,
    /// Byte-level rANS coding (`RansEncoder`, P6): the original
    /// methodology's "rANS" step (A1).
    pub allow_byte_rans: bool,
    /// SequenceRans — the post-registration local-match + entropy floor
    /// (`SequenceEncoder`, ladder step E1). Gated separately so A1 and the
    /// "direct rANS" baseline measure pure byte rANS.
    pub allow_sequence_rans: bool,
    /// SequenceDeep — the post-registration deep-match family
    /// (`SequenceDeepEncoder`, ladder step E4, Phase-9E): repcodes +
    /// extended length codes + a deep background matcher (chain 256, lazy
    /// parse, rep-distance priority). Background-only; independent gate so
    /// the fast floor (E1) and the deep floor (E4) attribute separately.
    pub allow_sequence_rans_deep: bool,
    /// SequenceDict — the post-registration cross-chunk dictionary family
    /// (`SequenceDictEncoder`, ladder step E2): local-history + external
    /// same-file dictionary in one stream. Gated separately from base
    /// residuals so the temporal (BaseSequence) and contextual (Sequence-
    /// Dict) attribution boundaries stay clean.
    pub allow_sequence_dict: bool,
    /// SequenceSharedDict — the post-registration shared amortized
    /// dictionary family (`SequenceSharedDictEncoder`, ladder step E3,
    /// Phase-9C): local-history + optional same-file dictionary + a
    /// cross-file shared dictionary chosen by the background optimizer,
    /// counted as persistent state.
    pub allow_shared_dict: bool,
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
            allow_exact_ref: true,
            allow_configurational: true,
            allow_byte_rans: true,
            allow_sequence_rans: true,
            allow_sequence_rans_deep: true,
            allow_sequence_dict: true,
            allow_shared_dict: true,
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
            allow_exact_ref: false,
            allow_configurational: false,
            allow_byte_rans: false,
            allow_sequence_rans: false,
            allow_sequence_rans_deep: false,
            allow_sequence_dict: false,
            allow_shared_dict: false,
            allow_bases: false,
            allow_temporal_bases: false,
            allow_universe: false,
            allow_dsfb_ranking: false,
        }
    }

    /// RAW + byte rANS only (the original methodology's A1; pure — no
    /// SequenceRans, no EXACT_REF, no structural families).
    pub const fn raw_rans() -> Self {
        Self {
            allow_exact_ref: false,
            allow_configurational: false,
            allow_byte_rans: true,
            allow_sequence_rans: false,
            allow_sequence_rans_deep: false,
            allow_sequence_dict: false,
            allow_shared_dict: false,
            allow_bases: false,
            allow_temporal_bases: false,
            allow_universe: false,
            allow_dsfb_ranking: false,
        }
    }

    /// RAW + SequenceRans only (the standalone fast-floor baseline: the
    /// match finder over nothing else).
    pub const fn raw_sequence() -> Self {
        Self {
            allow_exact_ref: false,
            allow_configurational: false,
            allow_byte_rans: false,
            allow_sequence_rans: true,
            allow_sequence_rans_deep: false,
            allow_sequence_dict: false,
            allow_shared_dict: false,
            allow_bases: false,
            allow_temporal_bases: false,
            allow_universe: false,
            allow_dsfb_ranking: false,
        }
    }

    /// RAW + SequenceDeep only (the standalone deep-floor baseline:
    /// repcodes + extended lengths + the deep matcher over nothing else).
    pub const fn raw_sequence_deep() -> Self {
        Self {
            allow_exact_ref: false,
            allow_configurational: false,
            allow_byte_rans: false,
            allow_sequence_rans: false,
            allow_sequence_rans_deep: true,
            allow_sequence_dict: false,
            allow_shared_dict: false,
            allow_bases: false,
            allow_temporal_bases: false,
            allow_universe: false,
            allow_dsfb_ranking: false,
        }
    }

    /// Whether a channel may be evaluated at all.
    pub const fn channel_allowed(&self, channel: Channel) -> bool {
        match channel {
            Channel::SharedContent => self.allow_exact_ref,
            Channel::PrevVersion => self.allow_bases,
            Channel::Adjacent | Channel::PrevInFile | Channel::FamilyBase => {
                self.allow_bases && self.allow_temporal_bases
            }
            Channel::Universe => self.allow_universe,
            Channel::Rans => self.allow_byte_rans || self.allow_sequence_rans,
            Channel::Raw => true,
            Channel::SharedDict => self.allow_shared_dict,
        }
    }

    /// Whether a canonical descriptor may be REUSED for a duplicate
    /// logical chunk under these options. CAS object sharing is a store
    /// invariant (always on); canonical descriptor reuse is representation
    /// reuse, so it must stay within the families this configuration
    /// admits — otherwise the RAW-only ablation would silently store
    /// ZERO/PERIODIC descriptors and the ladder steps would conflate.
    pub const fn representation_allowed(
        &self,
        d: &crate::core::representation::Representation,
    ) -> bool {
        use crate::core::representation::Representation;
        match d {
            Representation::Raw { .. } => true,
            Representation::Rans { .. } => self.allow_byte_rans,
            Representation::SequenceRans { .. } => self.allow_sequence_rans,
            Representation::SequenceDeep { .. } => self.allow_sequence_rans_deep,
            Representation::SequenceDict { .. } => self.allow_sequence_dict,
            Representation::SequenceSharedDict { .. } => self.allow_shared_dict,
            Representation::ExactRef { .. } => self.allow_exact_ref,
            Representation::BaseResidual { .. } => self.allow_bases,
            Representation::EntropyRef { .. } => self.allow_universe,
            Representation::Zero { .. }
            | Representation::Fill { .. }
            | Representation::Inline { .. }
            | Representation::Sparse { .. }
            | Representation::Palette { .. }
            | Representation::Periodic { .. }
            | Representation::Permutation { .. }
            | Representation::SparseBlock64 { .. } => self.allow_configurational,
        }
    }

    /// Every ablation configuration (spec §43): leave-one-out gates — the
    /// single source of truth for the CLI and the evidence campaign.
    pub fn ablation_modes() -> Vec<(&'static str, OptimizeOptions)> {
        vec![
            ("full", OptimizeOptions::default()),
            ("raw", OptimizeOptions::raw_only()),
            ("raw-byte-rans", OptimizeOptions::raw_rans()),
            (
                "no-exact-ref",
                OptimizeOptions {
                    allow_exact_ref: false,
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
                    allow_byte_rans: false,
                    allow_sequence_rans: false,
                    ..Default::default()
                },
            ),
            (
                "no-byte-rans",
                OptimizeOptions {
                    allow_byte_rans: false,
                    ..Default::default()
                },
            ),
            (
                "no-sequence-rans",
                OptimizeOptions {
                    allow_sequence_rans: false,
                    ..Default::default()
                },
            ),
            (
                "no-deep",
                OptimizeOptions {
                    allow_sequence_rans_deep: false,
                    ..Default::default()
                },
            ),
            (
                "no-sequence-dict",
                OptimizeOptions {
                    allow_sequence_dict: false,
                    ..Default::default()
                },
            ),
            (
                "no-shared-dict",
                OptimizeOptions {
                    allow_shared_dict: false,
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
    /// each step adds exactly one mechanism on top of the previous. Steps
    /// A0–A8 follow the ORIGINAL methodology (A1 = RAW + byte rANS);
    /// SequenceRans is a post-registration extension, E1 (the current
    /// production pipeline = full + background pass).
    ///
    /// The engine has one base gate for the in-hand previous-version
    /// residual coding (A3) and one for the temporal base channels that
    /// materialize other chunks (A5); both map to the generic ladder's
    /// "base" steps without loss of granularity.
    pub fn cumulative_ladder_modes() -> Vec<(&'static str, OptimizeOptions, bool)> {
        vec![
            ("A0-raw", OptimizeOptions::raw_only(), false),
            // A1 = A0 + byte rANS (pure; the original methodology's step).
            ("A1-byte-rans", OptimizeOptions::raw_rans(), false),
            // A2 = A1 + EXACT_REF aliasing (descriptor-level dedup).
            (
                "A2-exact-ref",
                OptimizeOptions {
                    allow_exact_ref: true,
                    allow_configurational: false,
                    allow_byte_rans: true,
                    allow_sequence_rans: false,
                    allow_sequence_rans_deep: false,
                    allow_sequence_dict: false,
                    allow_shared_dict: false,
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
                    allow_exact_ref: true,
                    allow_configurational: false,
                    allow_byte_rans: true,
                    allow_sequence_rans: false,
                    allow_sequence_rans_deep: false,
                    allow_sequence_dict: false,
                    allow_shared_dict: false,
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
                    allow_exact_ref: true,
                    allow_configurational: true,
                    allow_byte_rans: true,
                    allow_sequence_rans: false,
                    allow_sequence_rans_deep: false,
                    allow_sequence_dict: false,
                    allow_shared_dict: false,
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
                    allow_exact_ref: true,
                    allow_configurational: true,
                    allow_byte_rans: true,
                    allow_sequence_rans: false,
                    allow_sequence_rans_deep: false,
                    allow_sequence_dict: false,
                    allow_shared_dict: false,
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
                    allow_exact_ref: true,
                    allow_configurational: true,
                    allow_byte_rans: true,
                    allow_sequence_rans: false,
                    allow_sequence_rans_deep: false,
                    allow_sequence_dict: false,
                    allow_shared_dict: false,
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
                    allow_exact_ref: true,
                    allow_configurational: true,
                    allow_byte_rans: true,
                    allow_sequence_rans: false,
                    allow_sequence_rans_deep: false,
                    allow_sequence_dict: false,
                    allow_shared_dict: false,
                    allow_bases: true,
                    allow_temporal_bases: true,
                    allow_universe: true,
                    allow_dsfb_ranking: true,
                },
                false,
            ),
            // A8 = A7 + background re-optimization pass after the write.
            (
                "A8-background",
                OptimizeOptions {
                    allow_exact_ref: true,
                    allow_configurational: true,
                    allow_byte_rans: true,
                    allow_sequence_rans: false,
                    allow_sequence_rans_deep: false,
                    allow_sequence_dict: false,
                    allow_shared_dict: false,
                    allow_bases: true,
                    allow_temporal_bases: true,
                    allow_universe: true,
                    allow_dsfb_ranking: true,
                },
                true,
            ),
            // E1 = the post-registration SequenceRans floor (fast matcher
            // only; the deep family is excluded so the E1 boundary stays
            // the fast floor).
            (
                "E1-sequence-rans",
                OptimizeOptions {
                    allow_sequence_rans_deep: false,
                    ..Default::default()
                },
                true,
            ),
            // E2 = E1 + the cross-chunk dictionary family (SequenceDict,
            // Phase-9B): local-history + external same-file dictionary in
            // one stream, depth-capped. The shared-dictionary family is
            // excluded so the E2 boundary stays the same-file dictionary.
            (
                "E2-sequence-dict",
                OptimizeOptions {
                    allow_shared_dict: false,
                    allow_sequence_rans_deep: false,
                    ..Default::default()
                },
                true,
            ),
            // E3 = E2 + the shared amortized dictionary family
            // (SequenceSharedDict, Phase-9C): a cross-file shared
            // dictionary chosen by the background optimizer, counted as
            // persistent state.
            (
                "E3-shared-dict",
                OptimizeOptions {
                    allow_sequence_rans_deep: false,
                    ..Default::default()
                },
                true,
            ),
            // E4 = E3 + the deep-match family (SequenceDeep, Phase-9E):
            // repcodes + extended length codes + the deep background
            // matcher. The current production pipeline.
            ("E4-deep", OptimizeOptions::default(), true),
        ]
    }
}
