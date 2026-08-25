//! Predictor channels and their evidence features
//! (`docs/theory/dsfb-selection.md`).

#![forbid(unsafe_code)]

use crate::core::candidate::BaseChunk;
use crate::core::extent::ChunkId;

/// Predictor channel ids (P0..P7).
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Features {
    /// Channel this evidence belongs to.
    pub channel: Channel,
    /// Encoded residual length ratio (0 = perfect predictor, 1 = raw-sized).
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
    /// Bounded measurement scalar fed to the observer: `1 − log2(1+residual
    /// cost)/log2(1+raw cost)` in `[0, 1]`, where residual cost is the
    /// residual-ratio proxy. Higher = better predictor.
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

/// Stable key identifying a (file, offset) logical chunk for observer state.
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
