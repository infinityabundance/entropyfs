//! Sparse configuration encoder: `k` marked (non-zero) positions among
//! `n`, position subset ranked by `C(n, k)`, values as literals.
//!
//! # PURPOSE
//!
//! The SPARSE representation family (tag `0x07`): a chunk is a set of `k`
//! marked positions plus their literal values. The position subset is
//! persisted as its coordinate inside the `C(n, k)`-sized combination
//! space instead of as raw offsets — the saved bits are `ceil(log2 C(n,k))`
//! against `8k` raw offset bytes (`docs/theory/configurational-storage.md`
//! §2).
//!
//! # BOUNDARY
//!
//! A pure candidate encoder. It knows the chunk bytes, the limits, and the
//! rank arithmetic; it never touches the store, never decides whether the
//! family wins (the cost function does, ADR-0010), and never proposes a
//! candidate it cannot account honestly. `k == 0` belongs to ZERO; dense
//! chunks (`k > n/4`) and `k > max_fanout` are skipped; the
//! `u128`-overflow range of the combination rank at 64 KiB
//! (`10 ≤ k ≤ n−10`) is SPARSE_BLOCK64's territory.
//!
//! # MODEL
//!
//! `chunk = k marked positions (strictly ascending) + k literal bytes`.
//! The position subset maps to `[0, C(n, k))` via the combinatorial
//! number system; unrank is its inverse. The descriptor persists
//! `(k, rank, literals, len)`; the materializer regenerates the positions
//! from the rank and places the literals.
//!
//! # PERSISTENT AUTHORITY
//!
//! Yes: the descriptor bytes are persisted verbatim when this candidate
//! wins (`docs/format/ondisk-v1.md`, tag `0x07`: `k u32, rank u128,
//! literals k bytes`). The rank must fit `u128`; a state space that does
//! not is rejected — the family is simply not representable in v1.
//!
//! # CORRECTNESS INVARIANTS
//!
//! - positions strictly ascending and `< n` (the rank functions reject
//!   violations);
//! - `rank < C(n, k)` (`Representation::validate` double-checks the range
//!   before the candidate is proposed);
//! - literals correspond 1:1 with marked positions in scan order;
//! - materialization is byte-exact — enforced by the §32 candidate
//!   validation gate before anything is committed.
//!
//! # CONCURRENCY
//!
//! Stateless encoder; safe to call from any thread (parallel chunk
//! preparation, Phase-10C).
//!
//! # RESOURCE BOUNDS
//!
//! `n ≤ max_chunk_size`; `k ≤ max_fanout`; the density gate `k ≤ n/4`
//! keeps rank/unrank work bounded (rank cost grows with `k`); the state
//! space is `u128`-bounded. Encode is a single `O(n)` scan plus one
//! `O(k)` rank.
//!
//! # PERFORMANCE
//!
//! One pass over the chunk; the rank is computed once per candidate. The
//! honest `ByteSplit` (literals as residual bytes, rank as the 16-byte
//! configurational coordinate) means the cost function sees exactly what
//! would be persisted.
//!
//! # FAILURE MODES
//!
//! `SpaceOverflow` / `Overflow` from the rank, or a failed
//! `Representation::validate`, yields an empty candidate list — the
//! family skips itself and RAW (or another family) wins. Nothing here
//! panics; a wrong candidate would be caught by the §32 gate.
//!
//! # HISTORY / EVIDENCE
//!
//! Phase-1 family (`docs/theory/configurational-storage.md` §2); the
//! `u128` cliff at `10 ≤ k ≤ n−10` for 64 KiB chunks motivated
//! SPARSE_BLOCK64 (Phase-8, tag `0x0E`; `docs/format/ondisk-v1.md`).

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
///
/// The family is single-candidate: a chunk's position subset has exactly
/// one canonical form (ascending scan order), so `encode` returns at most
/// one candidate.
#[derive(Debug, Default)]
pub struct SparseEncoder;

impl Encoder for SparseEncoder {
    fn name(&self) -> &'static str {
        "SPARSE"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        // -------------------------------------------------------------------
        // Stage 1: bounds gate. An empty or oversized chunk is not a
        // representable sparse configuration in v1.
        // -------------------------------------------------------------------
        let n = input.len() as u64;
        if n == 0 || n > ctx.limits.max_chunk_size {
            return Vec::new();
        }
        // -------------------------------------------------------------------
        // Stage 2: single scan collecting marked positions and literals in
        // ascending order — the canonical form both the rank and the
        // materializer depend on.
        // -------------------------------------------------------------------
        let mut positions: Vec<u32> = Vec::new();
        let mut literals: Vec<u8> = Vec::new();
        for (i, &b) in input.iter().enumerate() {
            if b != 0 {
                positions.push(i as u32);
                literals.push(b);
            }
        }
        // -------------------------------------------------------------------
        // Stage 3: family gates. `k == 0` is ZERO's family; dense input is
        // skipped so the rank/unrank work stays bounded (rank cost grows
        // with `k`); `k > max_fanout` would be rejected by Limits anyway.
        // -------------------------------------------------------------------
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
        // -------------------------------------------------------------------
        // Stage 4: rank the position subset. A state space that overflows
        // u128 is rejected — the family is not representable in v1
        // (SPARSE_BLOCK64 exists for the overflow range).
        // -------------------------------------------------------------------
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
        // -------------------------------------------------------------------
        // Stage 5: honest accounting and candidate. The literals are
        // payload (`k` bytes); the rank is the configurational coordinate
        // (16 bytes); the rest of the descriptor (tag, k, len) counts as
        // descriptor bytes. The cost function decides from here.
        // -------------------------------------------------------------------
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

/// Distinct symbols of `input` in first-occurrence order (stable per
/// input; the presence bitmap guarantees each symbol appears once).
///
/// NOTE (updated to match the code): the previous doc described this as a
/// "sorted" distinct-symbol set shared with the permutation encoder; the
/// current code returns first-occurrence order, and `permutation.rs` now
/// builds its own sorted alphabet (canonical form lives there). The
/// `Option` return is vestigial — this function never returns `None`.
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
