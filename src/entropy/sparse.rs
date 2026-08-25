//! Sparse configuration encoder: `k` marked (non-zero) positions among
//! `n`, position subset ranked by `C(n, k)`, values as literals.

#![forbid(unsafe_code)]

use crate::core::candidate::{Candidate, CandidateContext, Encoder};
use crate::core::cost::ByteSplit;
use crate::core::extent::ChunkId;
use crate::core::representation::Representation;
use crate::entropy::rank::{RankError, rank_comb_subset};

/// Sparse configuration encoder.
///
/// Only proposed when the literal cost plus rank cost beats RAW (the cost
/// function decides; the encoder just proposes and accounts honestly).
#[derive(Debug, Default)]
pub struct SparseEncoder;

impl Encoder for SparseEncoder {
    fn name(&self) -> &'static str {
        "SPARSE"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        let n = input.len() as u64;
        if n == 0 || n > ctx.limits.max_chunk_size {
            return Vec::new();
        }
        // Collect non-zero positions and literals.
        let mut positions: Vec<u32> = Vec::new();
        let mut literals: Vec<u8> = Vec::new();
        for (i, &b) in input.iter().enumerate() {
            if b != 0 {
                positions.push(i as u32);
                literals.push(b);
            }
        }
        let k = positions.len() as u64;
        if k == 0 {
            // ZERO handles this family.
            return Vec::new();
        }
        // Sparse only wins for genuinely sparse data; a cheap density guard
        // keeps the rank/unrank work bounded (rank cost grows with k).
        if k > n / 4 {
            return Vec::new();
        }
        if k > ctx.limits.max_fanout as u64 {
            return Vec::new();
        }
        let rank = match rank_comb_subset(&positions, n) {
            Ok(r) => r,
            Err(RankError::SpaceOverflow) | Err(RankError::Overflow) => return Vec::new(),
            Err(_) => return Vec::new(),
        };
        let rep = Representation::Sparse {
            k: k as u32,
            rank,
            literals: literals.clone(),
            len: n,
        };
        // Descriptor validation double-checks the rank range.
        if rep.validate(ctx.limits).is_err() {
            return Vec::new();
        }
        let split = ByteSplit {
            residual: k,         // literals
            configurational: 16, // u128 rank
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

/// Validate that `input` is a pure permutation chunk (all bytes distinct)
/// and return the sorted distinct symbols — shared with the permutation
/// encoder so the two families do not disagree about canonical form.
pub fn distinct_symbols(input: &[u8]) -> Option<Vec<u8>> {
    let mut seen = [false; 256];
    let mut distinct = Vec::new();
    for &b in input {
        if !seen[b as usize] {
            seen[b as usize] = true;
            distinct.push(b);
        }
    }
    Some(distinct)
}

/// Convenience for tests: content id of a chunk.
pub fn cid_of(input: &[u8]) -> ChunkId {
    ChunkId::of(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::candidate::CandidateContext;
    use crate::core::cost::Policy;
    use crate::core::limits::Limits;

    #[test]
    fn sparse_encoder_proposes_and_roundtrips() {
        let mut input = vec![0u8; 1024];
        input[3] = 0xAB;
        input[100] = 0xCD;
        input[1023] = 0xEF;
        let enc = SparseEncoder;
        let ctx = CandidateContext {
            limits: &Limits::default(),
            policy: &Policy::default(),
            content_id: ChunkId::of(&input),
            bases: &[],
            dedup: None,
        };
        let cands = enc.encode(&input, &ctx);
        assert_eq!(cands.len(), 1);
        assert!(matches!(
            cands[0].representation,
            Representation::Sparse { .. }
        ));
        assert_eq!(cands[0].representation.len(), 1024);
    }

    #[test]
    fn sparse_skips_dense() {
        let input: Vec<u8> = (0..256u32).map(|i| (i % 200) as u8).collect();
        let enc = SparseEncoder;
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
    fn sparse_skips_zero_input() {
        let input = vec![0u8; 512];
        let enc = SparseEncoder;
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
