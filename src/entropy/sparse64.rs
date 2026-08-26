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
//!
//! # PURPOSE
//!
//! The SPARSE_BLOCK64 candidate family (tag `0x0E`, feature bit 11):
//! sparse chunks with *any* marked-byte count are representable, closing
//! the plain-SPARSE `u128` cliff at 64 KiB (`10 ≤ k ≤ n−10`;
//! `docs/format/ondisk-v1.md` §7).
//!
//! # BOUNDARY
//!
//! A pure candidate encoder. It proposes the model + enc objects via the
//! shared rANS/raw stream codec ([`encode_streams`]); it never touches the
//! store and never decides whether the family wins (the honest gate
//! below, the cost function, and the §32 validation decide). It delegates
//! `k ≤ 9` back to plain SPARSE (no stream overhead needed) and skips
//! dense input outright.
//!
//! # MODEL
//!
//! The chunk is split into 64-bit words (`words = ceil(n/8)`). Per word:
//! popcount `k` (one byte), the subset rank among `C(64, k)` as `u64 LE`
//! (8 bytes), and the literal values (one byte per marked position). The
//! three streams (popcounts, ranks, literals) are rANS-coded with one
//! shared model (`SCALE_BITS = 14`, `Interleaved2`) — or stored raw by
//! the stream codec when that is cheaper. `nonzero` = words with `k > 0`;
//! the rank stream decodes to `nonzero × 8` bytes; `lit_out` = total
//! marked bytes. The descriptor references the model and enc objects by
//! `ChunkId`; materialization is random-accessible per word.
//!
//! # PERSISTENT AUTHORITY
//!
//! Yes: the descriptor and its two content-addressed objects (model + enc)
//! are persisted when this candidate wins (`docs/format/ondisk-v1.md`,
//! tag `0x0E`). Every rank fits a `u64` because `C(64, 32) < 2^63`, so
//! there is no `u128` cliff anywhere in the family. Feature bit 11 gates
//! format compatibility (v0.2.0 changelog).
//!
//! # CORRECTNESS INVARIANTS
//!
//! - each word's popcount equals the number of marked positions in that
//!   word, and its rank is `< C(64, k)` (checked by the rank functions);
//! - the descriptor's `pc_len/rank_len/lit_len` equal the encoded stream
//!   lengths, and `rank_len` decodes to `nonzero × 8` bytes
//!   (`docs/format/ondisk-v1.md` §7);
//! - the honest gate `descriptor + model + enc < n` must hold — a
//!   candidate that cannot beat raw is not proposed;
//! - materialization is byte-exact — enforced by the §32 candidate
//!   validation gate (the round-trip test covers the whole overflow
//!   range, `k ∈ {10, 37, 200, 4096, 8192}` at 64 KiB).
//!
//! # CONCURRENCY
//!
//! Stateless encoder; safe to call from any thread (parallel chunk
//! preparation, Phase-10C).
//!
//! # RESOURCE BOUNDS
//!
//! `n ≤ max_chunk_size`; the density pre-gate (`k·2 ≥ n` ⇒ skip) and the
//! small-`k` delegation (`k ≤ 9` and `k ≤ max_fanout` ⇒ plain SPARSE)
//! bound the CPU spent building streams; per-word ranks are `u64`-sized
//! by construction. Encode is `O(n)` for the scan plus one stream encode.
//!
//! # PERFORMANCE
//!
//! The density pre-gate exists for throughput: without it the write path
//! builds doomed streams on dense/random chunks. Phase-8 M5 caught a ~3×
//! write-throughput regression from exactly that; the `k ≥ n/2` gate
//! fixed it and is regression-tested (`dense_and_zero_skip`). The honest
//! gate is the final word on proposing, but the pre-gates keep the CPU
//! out of the way before the streams exist.
//!
//! # FAILURE MODES
//!
//! A rank error, a failed `encode_streams`, or a failed honest gate
//! yields an empty candidate list — the family skips itself and plain
//! SPARSE / rANS / RAW handle the chunk. Nothing here panics; a wrong
//! candidate would be caught by the §32 gate.
//!
//! # HISTORY / EVIDENCE
//!
//! Phase-8 directive §6 / ADR-0005 tag `0x0E`; the campaign caught and
//! fixed the dense-input write-throughput regression (CHANGELOG phase
//! 8-M5, sealed `campaign-1787666589-e895fcf`); the overflow-range
//! round-trips are pinned by `overflow_range_roundtrips`.

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
///
/// Proposes at most one candidate; delegates small-`k` and dense inputs
/// to the families that handle them better (plain SPARSE, rANS, RAW).
#[derive(Debug, Default)]
pub struct SparseBlock64Encoder;

impl Encoder for SparseBlock64Encoder {
    fn name(&self) -> &'static str {
        "SPARSE_BLOCK64"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        // -------------------------------------------------------------------
        // Stage 1: bounds gate.
        // -------------------------------------------------------------------
        let n = input.len();
        if n == 0 || n as u64 > ctx.limits.max_chunk_size {
            return Vec::new();
        }
        // -------------------------------------------------------------------
        // Stage 2: k-family gates — ZERO / RAW delegation, the density
        // pre-gate, and plain-SPARSE delegation. These keep the streams
        // from being built when a cheaper family already covers the chunk.
        // -------------------------------------------------------------------
        let k = input.iter().filter(|&&b| b != 0).count();
        if k == 0 || k == n {
            return Vec::new(); // ZERO / RAW territory
        }
        // Density pre-gate: the raw streams cost at least `k` literal
        // bytes plus `8 × nonzero` rank bytes (nonzero >= k/8), so once
        // k >= n/2 the streams cannot beat raw regardless of rANS
        // compression. Skipping early keeps the write path from paying
        // for doomed stream construction on dense/random chunks (the
        // campaign caught a ~3x write-throughput regression here).
        if k as u64 * 2 >= n as u64 {
            return Vec::new();
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
        // -------------------------------------------------------------------
        // Stage 3: per-word scan — popcounts, per-word subset ranks, and
        // literals, one 8-byte window at a time.
        // -------------------------------------------------------------------
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
        // -------------------------------------------------------------------
        // Stage 4: encode the three streams into the model + enc objects.
        // -------------------------------------------------------------------
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
        // -------------------------------------------------------------------
        // Stage 5: honest gate and accounting. The family must actually
        // beat raw (descriptor + model + enc < n) to be proposed. The
        // ByteSplit counts the two content ids as reference bytes; the
        // model payload is charged via `estimate`'s model_bytes and the
        // enc payload via `account_objects`.
        // -------------------------------------------------------------------
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
        // Half-dense (k >= n/2): the raw streams cannot beat raw; the
        // density pre-gate must skip without building streams (the
        // campaign caught a ~3x write-throughput regression here).
        let mut half = vec![0u8; 65536];
        for p in 0..32768usize {
            half[p * 2] = 7;
        }
        assert!(
            SparseBlock64Encoder
                .encode(&half, &ctx_for(&half, &limits, &policy))
                .is_empty()
        );
    }
}
