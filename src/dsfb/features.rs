//! Predictor channels and their evidence features
//! (`docs/theory/dsfb-selection.md`).
//!
//! # Purpose
//!
//! Define the channel vocabulary — the predictor families a candidate can
//! belong to (P0..P8) — and derive the bounded evidence scalar each
//! evaluated channel feeds the observer: [`Features::from_base`] compares
//! a candidate base against the target chunk and reduces the comparison
//! to a `[0, 1]` measurement.
//!
//! # Boundary
//!
//! Evidence extraction is read-only over target/base bytes (diff
//! summaries and histograms): it never encodes, allocates store objects,
//! or alters any candidate. Its output is advisory — it shapes trust and
//! search order only (ADR-0004).
//!
//! # Measurement model and units
//!
//! [`Features::measurement`] maps the residual-ratio proxy `x ∈ [0, 1]`
//! (per-byte residual cost; 0 = perfect predictor, 1 = raw-sized) to
//! `1 − log2(1 + x)/2` — 1.0 for an exact match, 0.5 for a raw-sized
//! residual — the log-scaled, fixed-denominator normalization of the
//! theory §2 formula. `diff_density` is differing positions / n;
//! `hist_change` is the L1 histogram distance over 2n; both are in [0, 1].
//! When no base exists (or the lengths mismatch) the features are the
//! worst case — raw-sized residual, no exact match — so a channel must
//! actually have a usable base to earn trust.
//!
//! # Invariants
//!
//! - [`Features::measurement`] ∈ [0, 1] for any input (clamped).
//! - [`Features::from_base`] never panics: missing or mismatched bases
//!   return worst-case features; empty targets yield density 0.
//! - `Channel` is `#[repr(u8)]` and its discriminant is used as an array
//!   index in `observer.rs` (`c as usize` into the EMA/weight/last-y
//!   arrays) — renumbering channels would silently corrupt observer
//!   state.
//!
//! # Concurrency
//!
//! Pure functions over borrowed slices; no shared state, no locks.
//!
//! # History / evidence
//!
//! Phase 4 defined P0–P5; P8 (SharedDict) joined in Phase-9C (v0.4.0,
//! gated by the `allow_shared_dict` ablation flag).

#![forbid(unsafe_code)]

use crate::core::candidate::BaseChunk;
use crate::core::extent::ChunkId;

/// Predictor channel ids (P0..P8). Each channel is one candidate
/// predictor family; a channel's discriminant doubles as its index into
/// the observer's fixed-size state arrays (`observer.rs`), so the ids are
/// load-bearing — do not renumber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Channel {
    /// P0: previous version of the same logical chunk.
    PrevVersion = 0,
    /// P1: adjacent chunk.
    Adjacent = 1,
    /// P2: exact/shared content (dedup).
    SharedContent = 2,
    /// P3: previous chunk in the same file (write order).
    PrevInFile = 3,
    /// P4: file-family structural base.
    FamilyBase = 4,
    /// P5: entropy/configuration universe.
    Universe = 5,
    /// P6: conventional rANS.
    Rans = 6,
    /// P7: raw (always available).
    Raw = 7,
    /// P8: cross-file shared dictionary (Phase-9C).
    SharedDict = 8,
}

impl Channel {
    /// All channels in order.
    pub const ALL: [Channel; 9] = [
        Channel::PrevVersion,
        Channel::Adjacent,
        Channel::SharedContent,
        Channel::PrevInFile,
        Channel::FamilyBase,
        Channel::Universe,
        Channel::Rans,
        Channel::Raw,
        Channel::SharedDict,
    ];

    /// Channel name (for explain/status output).
    pub const fn name(self) -> &'static str {
        match self {
            Channel::PrevVersion => "prev_version",
            Channel::Adjacent => "adjacent",
            Channel::SharedContent => "shared_content",
            Channel::PrevInFile => "prev_in_file",
            Channel::FamilyBase => "family_base",
            Channel::Universe => "universe",
            Channel::Rans => "rans",
            Channel::Raw => "raw",
            Channel::SharedDict => "shared_dict",
        }
    }
}

/// Raw evidence features extracted from an exact candidate evaluation.
///
/// Role: the input to the observer's measurement and to the slew
/// detector. Every field is derived, advisory evidence about how well one
/// predictor family reproduced the target chunk — never an authority over
/// the committed representation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Features {
    /// Channel this evidence belongs to.
    pub channel: Channel,
    /// Encoded residual length ratio (0 = perfect predictor, 1 = raw-sized).
    /// Computed as the diff-density proxy — `from_base` sets
    /// `residual_ratio = diff_density`.
    pub residual_ratio: f64,
    /// Non-zero density of the XOR difference (0..1).
    pub diff_density: f64,
    /// Number of contiguous differing runs.
    pub diff_runs: u32,
    /// Differing position count.
    pub diff_positions: u32,
    /// Histogram-change magnitude (L1 distance / 2n, 0..1).
    pub hist_change: f64,
    /// Whether the predictor matched exactly (residual empty).
    pub exact_match: bool,
}

impl Features {
    /// Bounded measurement scalar fed to the observer: `1 − log2(1 + x)/2`
    /// with `x` the residual-ratio proxy clamped to [0, 1]. Higher =
    /// better predictor: 1.0 for an exact match (`x = 0`), 0.5 for a
    /// raw-sized residual (`x = 1`). This is the log-scaled,
    /// fixed-denominator normalization of the theory §2 formula
    /// (`docs/theory/dsfb-selection.md`): the denominator
    /// `log2(1 + raw_cost)` is held at the constant 2.0 instead of being
    /// computed from the raw candidate. The result lies in [0.5, 1] for
    /// the density proxy, so "as expensive as raw" maps to 0.5, not 0.
    pub fn measurement(&self) -> f64 {
        let x = self.residual_ratio.clamp(0.0, 1.0);
        let v = 1.0 - (1.0 + x).log2() / 2.0;
        v.clamp(0.0, 1.0)
    }

    /// Extract features by comparing a candidate base against the target.
    pub fn from_base(channel: Channel, target: &[u8], base: Option<&BaseChunk>) -> Features {
        match base {
            None => Features {
                channel,
                residual_ratio: 1.0,
                diff_density: 1.0,
                diff_runs: 0,
                diff_positions: 0,
                hist_change: 1.0,
                exact_match: false,
            },
            Some(b) => {
                if b.bytes.len() != target.len() {
                    return Features {
                        channel,
                        residual_ratio: 1.0,
                        diff_density: 1.0,
                        diff_runs: 0,
                        diff_positions: 0,
                        hist_change: 1.0,
                        exact_match: false,
                    };
                }
                let (positions, runs) = crate::entropy::residual::diff_summary(target, &b.bytes);
                let n = target.len() as f64;
                let density = if n == 0.0 { 0.0 } else { positions as f64 / n };
                let residual_ratio = density; // proxy: per-byte residual cost
                let mut hist_change = 0.0;
                if n > 0.0 {
                    let mut ht = [0i64; 256];
                    let mut hb = [0i64; 256];
                    for &x in target {
                        ht[x as usize] += 1;
                    }
                    for &x in &b.bytes {
                        hb[x as usize] += 1;
                    }
                    let mut l1 = 0i64;
                    for i in 0..256 {
                        l1 += (ht[i] - hb[i]).abs();
                    }
                    hist_change = (l1 as f64 / (2.0 * n)).clamp(0.0, 1.0);
                }
                Features {
                    channel,
                    residual_ratio,
                    diff_density: density,
                    diff_runs: runs as u32,
                    diff_positions: positions as u32,
                    hist_change,
                    exact_match: positions == 0,
                }
            }
        }
    }
}

/// Stable key identifying a (file, offset) logical chunk for observer
/// state.
///
/// Role: the identity under which per-chunk observer state lives. Because
/// `content_id` is the *new* chunk's id (built in `encode_guided`), the
/// key is version-scoped: rewriting a chunk with different bytes creates
/// a new observer entry (full distrust, Unknown), while re-writing
/// identical bytes continues the series. Consequence: trust/regime
/// history never bleeds across content changes, and each distinct version
/// costs one bounded entry (capped by `DSFB_MAX_CHUNKS` eviction).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkKey {
    /// File inode identifier (logical, stable across writes).
    pub ino: u64,
    /// Chunk index (offset / chunk_class).
    pub index: u64,
    /// Content id of the current chunk version.
    pub content_id: ChunkId,
}

impl ChunkKey {
    /// New key.
    pub const fn new(ino: u64, index: u64, content_id: ChunkId) -> Self {
        Self {
            ino,
            index,
            content_id,
        }
    }
}
