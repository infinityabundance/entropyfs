//! Palette / multinomial configuration encoder: a chunk using only a small
//! number of distinct symbols, encoded as palette + multiplicities +
//! multinomial rank (`n!/(∏ c_i!)`).

#![forbid(unsafe_code)]

use crate::core::candidate::{Candidate, CandidateContext, Encoder};
use crate::core::cost::ByteSplit;
use crate::core::representation::Representation;
use crate::entropy::rank::{RankError, rank_multinomial};

/// Palette encoder: `m ∈ [2, max_palette]` distinct symbols.
#[derive(Debug, Default)]
pub struct PaletteEncoder;

impl Encoder for PaletteEncoder {
    fn name(&self) -> &'static str {
        "PALETTE"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        let n = input.len() as u64;
        if n == 0 || n > ctx.limits.max_chunk_size {
            return Vec::new();
        }
        // Histogram.
        let mut hist = [0u32; 256];
        for &b in input {
            hist[b as usize] += 1;
        }
        let m = hist.iter().filter(|&&c| c > 0).count();
        if m < 2 || m > ctx.limits.max_palette {
            // m == 1 is FILL; m > max is rANS/RAW territory.
            return Vec::new();
        }
        // Palette sorted ascending (deterministic canonical form).
        let palette: Vec<u8> = (0..256u16)
            .filter(|&i| hist[i as usize] > 0)
            .map(|i| i as u8)
            .collect();
        let counts: Vec<u32> = palette.iter().map(|&s| hist[s as usize]).collect();
        // Map input to symbol indices.
        let mut seq = Vec::with_capacity(input.len());
        for &b in input {
            // binary search in palette (small, linear is fine)
            let idx = palette.iter().position(|&s| s == b).unwrap() as u8;
            seq.push(idx);
        }
        let rank = match rank_multinomial(&seq, n, &counts) {
            Ok(r) => r,
            Err(RankError::SpaceOverflow) | Err(RankError::Overflow) => return Vec::new(),
            Err(_) => return Vec::new(),
        };
        let rep = Representation::Palette {
            palette: palette.clone(),
            counts: counts.clone(),
            rank,
            len: n,
        };
        if rep.validate(ctx.limits).is_err() {
            return Vec::new();
        }
        // Account: descriptor keeps palette+counts; rank is configurational.
        let split = ByteSplit {
            configurational: 16,
            ..Default::default()
        };
        let cost = crate::core::cost::estimate(&rep, &split, 0);
        vec![Candidate {
            representation: rep,
            objects: Vec::new(),
            cost,
            content_id: ctx.content_id,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::candidate::CandidateContext;
    use crate::core::cost::Policy;
    use crate::core::extent::ChunkId;
    use crate::core::limits::Limits;
    use crate::core::materialize::materialize_to_vec;
    use crate::tests::helpers::MemResolver;

    #[test]
    fn palette_encoder_roundtrip() {
        // A 64-byte chunk over {0x10, 0x20, 0x30} with skew. (The
        // multinomial state space overflows u128 for large chunks; the
        // honest rejection path is covered by palette_skips_overflow.)
        let mut input = Vec::with_capacity(64);
        for i in 0..64 {
            input.push(match i % 10 {
                0..=5 => 0x10,
                6..=8 => 0x20,
                _ => 0x30,
            });
        }
        let enc = PaletteEncoder;
        let ctx = CandidateContext {
            limits: &Limits::default(),
            policy: &Policy::default(),
            content_id: ChunkId::of(&input),
            bases: &[],
            dedup: None,
        };
        let cands = enc.encode(&input, &ctx);
        assert_eq!(cands.len(), 1);
        let resolver = MemResolver::empty();
        let out =
            materialize_to_vec(&cands[0].representation, &resolver, &Limits::default()).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn palette_skips_overflowing_state_space() {
        // A large low-cardinality chunk: the multinomial state space does
        // not fit u128, so the candidate must be rejected (never truncated)
        // and rANS/RAW handle it instead.
        let mut input = Vec::with_capacity(8192);
        for i in 0..8192 {
            input.push(match i % 3 {
                0 => 0x11,
                1 => 0x22,
                _ => 0x33,
            });
        }
        let enc = PaletteEncoder;
        let ctx = CandidateContext {
            limits: &Limits::default(),
            policy: &Policy::default(),
            content_id: ChunkId::of(&input),
            bases: &[],
            dedup: None,
        };
        assert!(enc.encode(&input, &ctx).is_empty());
    }

    #[test]
    fn palette_skips_high_cardinality() {
        let input: Vec<u8> = (0..256u32).map(|i| (i % 251) as u8).collect();
        let enc = PaletteEncoder;
        let ctx = CandidateContext {
            limits: &Limits::default(),
            policy: &Policy::default(),
            content_id: ChunkId::of(&input),
            bases: &[],
            dedup: None,
        };
        assert!(enc.encode(&input, &ctx).is_empty());
    }
}
