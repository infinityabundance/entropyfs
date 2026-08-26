//! Palette / multinomial configuration encoder: a chunk using only a small
//! number of distinct symbols, encoded as palette + multiplicities +
//! multinomial rank (`n!/(∏ c_i!)`).
//!
//! # PURPOSE
//!
//! The PALETTE representation family (tag `0x08`): a low-cardinality chunk
//! is a small alphabet plus the multinomial coordinate of the symbol-index
//! sequence. The state space is `|F| = n!/(∏ c_i!)`; the saved bits are
//! `ceil(log2 |F|)` against `n` raw bytes
//! (`docs/theory/configurational-storage.md` §3).
//!
//! # BOUNDARY
//!
//! A pure candidate encoder. It maps chunk bytes → (ascending palette,
//! multiplicities, symbol-index sequence), asks `rank.rs` for the
//! multinomial coordinate, and accounts honestly; it never touches the
//! store and never decides whether the family wins (ADR-0010).
//! `m == 1` is FILL's family; `m > max_palette` is rANS/RAW territory.
//!
//! # MODEL
//!
//! `chunk = Σ over positions of palette[seq[pos]]`, with `seq` the
//! symbol-index sequence and `counts` the multiplicities (`Σ c_i = n`).
//! The descriptor persists `(palette, counts, rank, len)`; the
//! materializer unranks `seq` and maps each index through the palette.
//!
//! # PERSISTENT AUTHORITY
//!
//! Yes: the descriptor is persisted verbatim when this candidate wins
//! (`docs/format/ondisk-v1.md`, tag `0x08`: `m u8 (≤16), palette m bytes,
//! counts m×u32, rank u128`). The multinomial state space must fit `u128`;
//! a space that does not is rejected — the family is not representable in
//! v1 and rANS/RAW handle the chunk instead.
//!
//! # CORRECTNESS INVARIANTS
//!
//! - `m ∈ [2, max_palette]`;
//! - palette strictly ascending (the canonical form: the same chunk always
//!   yields the same descriptor) and `counts[i]` is the multiplicity of
//!   `palette[i]`;
//! - `Σ counts == n` and `rank < n!/(∏ c_i!)` (checked by
//!   `Representation::validate`);
//! - materialization is byte-exact — enforced by the §32 candidate
//!   validation gate.
//!
//! # CONCURRENCY
//!
//! Stateless encoder; safe to call from any thread (parallel chunk
//! preparation, Phase-10C).
//!
//! # RESOURCE BOUNDS
//!
//! `n ≤ max_chunk_size`; `m ≤ max_palette` (16); state space `u128`-bound
//! (for large low-cardinality chunks the multinomial overflows and the
//! candidate is honestly rejected). Encode is an `O(n)` histogram, an
//! `O(256)` palette extraction, and an `O(n·m)` index mapping (linear
//! palette lookup — fine for `m ≤ 16`).
//!
//! # PERFORMANCE
//!
//! One histogram pass; the multinomial rank is computed once per
//! candidate. The honest `ByteSplit` (rank as the 16-byte configurational
//! coordinate; palette + counts remain descriptor bytes) means the cost
//! function sees exactly what would be persisted.
//!
//! # FAILURE MODES
//!
//! `SpaceOverflow` / `Overflow` from the rank, or a failed
//! `Representation::validate`, yields an empty candidate list — the
//! family skips itself and rANS/RAW win. Nothing here panics; a wrong
//! candidate would be caught by the §32 gate.
//!
//! # HISTORY / EVIDENCE
//!
//! Phase-1 family (`docs/theory/configurational-storage.md` §3); the
//! honest overflow-rejection path for large low-cardinality chunks is
//! pinned by `palette_skips_overflowing_state_space`.

#![forbid(unsafe_code)]

use crate::core::candidate::{Candidate, CandidateContext, Encoder};
use crate::core::cost::ByteSplit;
use crate::core::representation::Representation;
use crate::entropy::rank::{RankError, rank_multinomial};

/// Palette encoder: `m ∈ [2, max_palette]` distinct symbols.
///
/// Single-candidate family: the palette is canonicalized ascending, so a
/// chunk has exactly one palette descriptor. `m == 1` is FILL's family;
/// `m > max_palette` is rANS/RAW territory — and for large low-cardinality
/// chunks the multinomial state space overflows u128, which is an honest
/// rejection (never truncated).
#[derive(Debug, Default)]
pub struct PaletteEncoder;

impl Encoder for PaletteEncoder {
    fn name(&self) -> &'static str {
        "PALETTE"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        // -------------------------------------------------------------------
        // Stage 1: bounds gate. An empty or oversized chunk is not a
        // representable palette configuration in v1.
        // -------------------------------------------------------------------
        let n = input.len() as u64;
        if n == 0 || n > ctx.limits.max_chunk_size {
            return Vec::new();
        }
        // -------------------------------------------------------------------
        // Stage 2: histogram and cardinality gate.
        // -------------------------------------------------------------------
        let mut hist = [0u32; 256];
        for &b in input {
            hist[b as usize] += 1;
        }
        let m = hist.iter().filter(|&&c| c > 0).count();
        if m < 2 || m > ctx.limits.max_palette {
            // m == 1 is FILL; m > max is rANS/RAW territory.
            return Vec::new();
        }
        // -------------------------------------------------------------------
        // Stage 3: canonical form — the palette in ascending byte order,
        // its multiplicities, and the symbol-index sequence the multinomial
        // rank operates on.
        // -------------------------------------------------------------------
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
        // -------------------------------------------------------------------
        // Stage 4: multinomial rank. The state space `n!/(∏c_i!)` must fit
        // u128; overflow rejects the candidate (never truncated) — for
        // large low-cardinality chunks this is the honest path, and
        // rANS/RAW handle the chunk instead.
        // -------------------------------------------------------------------
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
        // -------------------------------------------------------------------
        // Stage 5: honest accounting and candidate. The rank (16 bytes) is
        // the configurational coordinate; palette + counts remain
        // descriptor bytes (`encoded_size` counts them). The cost function
        // decides from here.
        // -------------------------------------------------------------------
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
