//! Permutation encoder: chunks that are a permutation of `m ≤ 34` distinct
//! bytes, encoded by factoradic rank over the sorted distinct symbols.
//!
//! v1 candidate policy (`docs/theory/configurational-storage.md` §4): the
//! generator only proposes this family for small chunks with all-distinct
//! bytes, where the factoradic coordinate genuinely beats RAW.

#![forbid(unsafe_code)]

use crate::core::candidate::{Candidate, CandidateContext, Encoder};
use crate::core::cost::ByteSplit;
use crate::core::representation::Representation;
use crate::entropy::rank::{RankError, rank_permutation};

/// Permutation encoder (small all-distinct chunks only).
#[derive(Debug, Default)]
pub struct PermutationEncoder;

impl Encoder for PermutationEncoder {
    fn name(&self) -> &'static str {
        "PERMUTATION"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        let n = input.len() as u64;
        if n == 0 || n > 34 {
            return Vec::new();
        }
        // All bytes must be distinct (a genuine permutation of a subset).
        let mut seen = [false; 256];
        for &b in input {
            if seen[b as usize] {
                return Vec::new();
            }
            seen[b as usize] = true;
        }
        // The v1 canonical form maps the sorted distinct symbols to 0..m in
        // natural order. Because all bytes are distinct and sorted order is
        // the identity mapping of values, the index sequence equals the byte
        // values themselves when the values are exactly 0..m. For arbitrary
        // distinct bytes we rank the *relative order*.
        //
        // Implementation: rank over the sequence of indices into the sorted
        // distinct symbols. rank_permutation expects a permutation of
        // `0..m`; we compute the index sequence.
        let mut sorted: Vec<u8> = input.to_vec();
        sorted.sort_unstable();
        let mut seq = Vec::with_capacity(input.len());
        for &b in input {
            let idx = sorted.binary_search(&b).expect("symbol present") as u8;
            seq.push(idx);
        }
        let rank = match rank_permutation(&seq) {
            Ok(r) => r,
            Err(RankError::SpaceOverflow) | Err(RankError::Overflow) => return Vec::new(),
            Err(_) => return Vec::new(),
        };
        let rep = Representation::Permutation {
            rank,
            alphabet: sorted,
            len: n,
        };
        if rep.validate(ctx.limits).is_err() {
            return Vec::new();
        }
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

    fn ctx_for<'a>(input: &[u8], limits: &'a Limits, policy: &'a Policy) -> CandidateContext<'a> {
        CandidateContext {
            limits,
            policy,
            content_id: ChunkId::of(input),
            bases: &[],
            dedup: None,
        }
    }

    #[test]
    fn permutation_roundtrip() {
        let limits = Limits::default();
        let policy = Policy::default();
        // A genuine permutation of 30 distinct bytes: 7 is coprime with 30,
        // so the affine map is a permutation of 0..29, shifted to 200..229.
        let input: Vec<u8> = (0..30u32).map(|i| 200 + ((i * 7 + 3) % 30) as u8).collect();
        let enc = PermutationEncoder;
        let cands = enc.encode(&input, &ctx_for(&input, &limits, &policy));
        assert_eq!(cands.len(), 1);
        let resolver = MemResolver::empty();
        let out =
            materialize_to_vec(&cands[0].representation, &resolver, &Limits::default()).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn permutation_skips_duplicates_and_large() {
        let limits = Limits::default();
        let policy = Policy::default();
        let dup = vec![1u8, 2, 3, 4, 5, 1];
        assert!(
            PermutationEncoder
                .encode(&dup, &ctx_for(&dup, &limits, &policy))
                .is_empty()
        );
        let big = vec![0u8; 64];
        assert!(
            PermutationEncoder
                .encode(&big, &ctx_for(&big, &limits, &policy))
                .is_empty()
        );
    }
}
