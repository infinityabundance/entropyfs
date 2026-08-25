//! SparseBlock64: blockwise-64 enumerative sparse coding (Phase-8
//! directive §6, ADR-0005 tag 0x0E).
//!
//! The whole-chunk combination rank `C(n, k)` overflows `u128` for
//! `10 <= k <= n - 10` at 64 KiB chunks, which caps the plain SPARSE
//! family. Blockwise-64 removes the cliff: the chunk is split into 64-bit
//! words; each word persists its popcount `k` and the subset rank among
//! `C(64, k)` — and even the largest `C(64, 32)` fits a `u64`. The three
//! streams (popcounts, ranks as u64 LE, literals) use the shared
//! rANS/raw stream codec, so the whole family is bounded,
//! popcount-friendly, and random-accessible per word.

#![forbid(unsafe_code)]

use crate::core::candidate::{Candidate, CandidateContext, Encoder, ObjectRecord};
use crate::core::cost::ByteSplit;
use crate::core::representation::{RansCodec, Representation};
use crate::rans::sequence::{SequenceStreams, encode_streams};

/// Scale bits shared by the three rANS models.
const SCALE_BITS: u8 = 14;
/// Codec shared by the three streams.
const CODEC: RansCodec = RansCodec::Interleaved2;

/// The blockwise-64 sparse candidate family.
#[derive(Debug, Default)]
pub struct SparseBlock64Encoder;

impl Encoder for SparseBlock64Encoder {
    fn name(&self) -> &'static str {
        "SPARSE_BLOCK64"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        let n = input.len();
        if n == 0 || n as u64 > ctx.limits.max_chunk_size {
            return Vec::new();
        }
        let k = input.iter().filter(|&&b| b != 0).count();
        if k == 0 || k == n {
            return Vec::new(); // ZERO / RAW territory
        }
        // When the whole-chunk rank fits, the plain SPARSE family is
        // cheaper (no model/stream overhead); only propose here when the
        // u128 cliff applies or the streams can plausibly win. The cost
        // gate below is the final word, but avoid the CPU when the data is
        // dense (blockwise would need ~all words nonzero).
        if k as u64 <= ctx.limits.max_fanout as u64 && (k as u64) <= 9 {
            // SPARSE handles k <= 9 at 64 KiB (C(65536,9) < u128); skip.
            return Vec::new();
        }
        let words = n.div_ceil(8);
        let mut popcounts: Vec<u8> = Vec::with_capacity(words);
        let mut ranks: Vec<u8> = Vec::new();
        let mut literals: Vec<u8> = Vec::new();
        for w in 0..words {
            let start = w * 8;
            let end = (start + 8).min(n);
            let mut positions: Vec<u32> = Vec::new();
            let mut vals: Vec<u8> = Vec::new();
            for (j, p) in (start..end).enumerate() {
                if input[p] != 0 {
                    positions.push(j as u32);
                    vals.push(input[p]);
                }
            }
            popcounts.push(positions.len() as u8);
            if !positions.is_empty() {
                let rank = match crate::entropy::rank::rank_comb_subset(&positions, 64) {
                    Ok(r) => r,
                    Err(_) => return Vec::new(),
                };
                // C(64, k) <= C(64, 32) < 2^63, so the rank fits a u64.
                ranks.extend_from_slice(&(rank as u64).to_le_bytes());
                literals.extend_from_slice(&vals);
            }
        }
        let nonzero = popcounts.iter().filter(|&&k| k > 0).count() as u32;
        let streams = SequenceStreams {
            commands: popcounts,
            literals,
            offsets: ranks,
        };
        let enc = match encode_streams(&streams) {
            Some(e) => e,
            None => return Vec::new(),
        };
        let model_obj = ObjectRecord::model(enc.model_obj);
        let enc_obj = ObjectRecord::data(enc.enc_obj);
        let rep = Representation::SparseBlock64 {
            model: model_obj.id,
            enc_obj: enc_obj.id,
            scale_bits: SCALE_BITS,
            codec: CODEC,
            pc_len: enc.seq_len,
            rank_len: enc.off_len,
            lit_len: enc.lit_len,
            words: words as u32,
            nonzero,
            lit_out: enc.lit_out,
            len: n as u64,
        };
        // Honest gate: descriptor + model + enc must beat raw.
        let total = rep
            .encoded_size()
            .saturating_add(model_obj.payload.len() as u64)
            .saturating_add(enc_obj.payload.len() as u64);
        if total >= n as u64 {
            return Vec::new();
        }
        let split = ByteSplit {
            reference: 64, // model + enc content ids
            ..Default::default()
        };
        let cost = crate::core::candidate::account_objects(
            crate::core::cost::estimate(&rep, &split, model_obj.payload.len() as u64),
            &[enc_obj.clone(), model_obj.clone()],
        );
        vec![Candidate {
            representation: rep,
            objects: vec![enc_obj, model_obj],
            cost,
            content_id: ctx.content_id,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::candidate::{CandidateContext, validate_candidate};
    use crate::core::cost::Policy;
    use crate::core::limits::Limits;
    use crate::tests::helpers::MemResolver;

    fn ctx_for<'a>(
        input: &'a [u8],
        limits: &'a Limits,
        policy: &'a Policy,
    ) -> CandidateContext<'a> {
        CandidateContext {
            limits,
            policy,
            content_id: crate::core::extent::ChunkId::of(input),
            bases: &[],
            dedup: None,
        }
    }

    /// A chunk with `k` marked bytes at deterministic positions.
    fn sparse_chunk(n: usize, k: usize) -> Vec<u8> {
        let mut v = vec![0u8; n];
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut placed = 0usize;
        while placed < k {
            x = x.wrapping_add(0xBF58_476D_1CE4_E5B9);
            let pos = ((x >> 32) as usize) % n;
            if v[pos] == 0 {
                v[pos] = (placed % 251) as u8 + 1;
                placed += 1;
            }
        }
        v
    }

    #[test]
    fn overflow_range_roundtrips() {
        // k in the u128-overflow range of plain SPARSE at 64 KiB
        // (10 <= k <= n-10): the blockwise family must propose and
        // round-trip byte-exactly.
        for &k in &[10usize, 37, 200, 4096, 8192] {
            let input = sparse_chunk(65536, k);
            let limits = Limits::default();
            let policy = Policy::default();
            let cands = SparseBlock64Encoder.encode(&input, &ctx_for(&input, &limits, &policy));
            assert_eq!(cands.len(), 1, "k={k}: expected one candidate");
            let cand = &cands[0];
            let resolver = MemResolver::from_map(
                cand.objects
                    .iter()
                    .map(|o| (o.id, o.payload.clone()))
                    .collect(),
            );
            validate_candidate(cand, &input, &resolver, &limits)
                .unwrap_or_else(|e| panic!("k={k}: validation failed: {e:?}"));
            assert!(
                cand.cost.persisted_bytes() < 65536,
                "k={k}: persisted {} not below raw",
                cand.cost.persisted_bytes()
            );
        }
    }

    #[test]
    fn small_k_delegates_to_sparse() {
        // k <= 9 is plain SPARSE territory (no stream overhead).
        let input = sparse_chunk(65536, 3);
        let limits = Limits::default();
        let policy = Policy::default();
        let cands = SparseBlock64Encoder.encode(&input, &ctx_for(&input, &limits, &policy));
        assert!(cands.is_empty(), "k=3 must be delegated to plain SPARSE");
    }

    #[test]
    fn dense_and_zero_skip() {
        let limits = Limits::default();
        let policy = Policy::default();
        let zero = vec![0u8; 65536];
        assert!(
            SparseBlock64Encoder
                .encode(&zero, &ctx_for(&zero, &limits, &policy))
                .is_empty()
        );
        let mut dense = vec![1u8; 65536];
        dense[0] = 0;
        assert!(
            SparseBlock64Encoder
                .encode(&dense, &ctx_for(&dense, &limits, &policy))
                .is_empty()
        );
    }
}
