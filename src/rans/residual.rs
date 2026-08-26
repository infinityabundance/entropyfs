//! rANS stream encoding/decoding via `ryg-rans-rs`, plus the
//! rANS-coded residual candidate family.
//!
//! # PURPOSE
//!
//! This module is the entropy-coding authority of the rANS layer: it
//! adapts the upstream `ryg-rans-rs` byte surface (single-state and
//! two-state interleaved) into deterministic, typed, allocation-bounded
//! stream encode/decode, and it hosts the two candidate families that
//! propose rANS representations:
//!
//! - `RansEncoder` — rANS the whole chunk (the pure entropy floor);
//! - `RansResidualEncoder` — rANS the XOR difference `D = X ^ B` against a
//!   base chunk (`BASE_RESIDUAL`, residual kind 0x03).
//!
//! The scalar paths are the authority; both codecs share the upstream
//! bitstream contract (`docs/theory/rans-state.md`).
//!
//! # BOUNDARY
//!
//! - Knows: byte streams, `RansModel` instances, `RansCodec`, and the
//!   candidate/object accounting it proposes into (`ObjectRecord`,
//!   `ByteSplit`).
//! - Never knows: match finding / LZ parsing (`sequence.rs`), model
//!   normalization (`model.rs`), model serialization (`metadata.rs`), the
//!   store, or the FUSE layer. The coder logic lives in `ryg-rans-rs`;
//!   this module never forks it.
//!
//! # MODEL
//!
//! A stream is a sequence of byte symbols. A canonical 256-symbol model
//! assigns each symbol a normalized frequency (`scale_bits` default 14 →
//! total 16384); rANS encodes the stream to ≈ `len·H/8` bytes where `H` is
//! the model entropy in bits/symbol. A model is *persisted state*: its
//! serialized bytes (`metadata::encode_model`) count against the win, so
//! the RAW fallback is decided by `enc + model < raw`, not `enc < raw`.
//!
//! # PERSISTENT AUTHORITY
//!
//! The persisted model and encoded stream are the decode authority: decode
//! rebuilds the symbol tables from the model and reproduces exactly
//! `out_len` bytes or fails with a typed error. Model identity is BLAKE3
//! of the serialization (`metadata::model_id`), so identical models
//! collapse to one content-addressed object.
//!
//! # CORRECTNESS INVARIANTS
//!
//! - Encode is deterministic: identical input + model → identical bytes.
//! - Every input byte must have nonzero frequency in the model; the dense
//!   symbol arrays the interleaved API needs hold zero-cost placeholders
//!   for absent symbols that would fail loudly if ever encoded.
//! - Decode takes a validated model and yields exactly `out_len` bytes or
//!   a typed `RansStreamError` — truncated/corrupt streams error, never
//!   overrun or panic.
//! - Worst-case encode output is `4·len + 20` bytes, so the output buffer
//!   is pre-sized and `EncodeBuffer` is unreachable in practice.
//! - A persisted rANS stream always carries a model that pays for itself
//!   (Phase-9G0 gate: `enc + model < raw`).
//!
//! # CONCURRENCY
//!
//! All functions are pure and hold no shared state; encode/decode calls
//! are safe to run concurrently and are deterministic across runs (the
//! scalar path is the authority; SIMD backends live in `dispatch.rs`).
//!
//! # RESOURCE BOUNDS
//!
//! - Encode allocates `4·len + 20` bytes for the output buffer.
//! - Decode allocates `out_len` bytes plus the `1 << scale_bits`
//!   cumulative-frequency table (16384 entries at the default 14).
//! - Stream lengths come from persisted descriptors, so they are
//!   attacker-influenceable; the materialize layer bounds them
//!   (`max_alloc_bytes`, model caps) before this module runs.
//!
//! # PERFORMANCE
//!
//! The candidate families use `Interleaved2` at 14 scale bits. The
//! Phase-9G0 model-cost gate is this module's most consequential
//! optimization: on the real source tree the sequence families' model
//! objects dropped 277.6 KB → 74.3 KB (per-extent overhead 26.5% → 11.1%
//! of footprint; tree court 2.388× → 2.775×; src corpus 4.327×) — sealed
//! `campaign-1787684918-80e36c8/`.
//!
//! # FAILURE MODES
//!
//! All failure is typed (`RansStreamError`): model construction failure,
//! decode of truncated/corrupt streams, decoded-length mismatch. Nothing
//! panics on hostile input.
//!
//! # HISTORY / EVIDENCE
//!
//! - Phase-9G0 — model-cost-aware stream selection: the RAW/rANS gate
//!   includes the persisted model bytes (was: rANS whenever `enc < raw`,
//!   persisting a model that could never pay for itself). Sealed
//!   `campaign-1787684918-80e36c8/`.

#![forbid(unsafe_code)]

use crate::core::candidate::{Candidate, CandidateContext, Encoder, ObjectRecord};
use crate::core::cost::ByteSplit;
use crate::core::representation::{RansCodec, Representation, Residual};
use crate::entropy::residual::diff_summary;
use crate::rans::metadata;
use crate::rans::model::{RansModel, normalize_histogram};

/// rANS stream errors (typed; hostile input must produce these, never a
/// panic). All lengths are in bytes: `LengthMismatch` compares the
/// expected vs actual decoded byte counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RansStreamError {
    /// Model construction failed.
    Model(String),
    /// Output buffer exhausted (should not happen with worst-case sizing).
    EncodeBuffer,
    /// Decode failed (truncated/corrupt stream or model).
    Decode(String),
    /// Decoded length mismatch.
    LengthMismatch {
        /// Expected decoded length.
        expected: u64,
        /// Actual decoded length.
        actual: u64,
    },
}

impl std::fmt::Display for RansStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for RansStreamError {}

/// Encode `input` (a byte stream) under `model` to a rANS bitstream.
///
/// # What
///
/// Produces the persisted rANS encoding of the stream. Deterministic:
/// identical input + model produce byte-identical output.
///
/// # Invariants
///
/// - Every byte of `input` must have nonzero frequency in `model` (the
///   histogram trained on the stream guarantees this; an external model
///   without symbol coverage must be detected by the caller before the
///   encode — a zero-frequency symbol fails here as `Model`).
/// - Worst-case output is `4·len + 20` bytes (the rANS renormalization
///   envelope), so the buffer is pre-sized and `EncodeBuffer` is
///   unreachable in practice.
pub fn encode_stream(input: &[u8], model: &RansModel) -> Result<Vec<u8>, RansStreamError> {
    // ---------------------------------------------------------------------
    // Stage 1: Build the validated encoder symbols from the model.
    // ---------------------------------------------------------------------
    let esyms = model
        .build_enc_symbols()
        .map_err(|e| RansStreamError::Model(format!("{e:?}")))?;
    // ---------------------------------------------------------------------
    // Stage 2: Pre-size the worst-case output buffer (`4·len + 20` bytes).
    // ---------------------------------------------------------------------
    let max_size = input.len() * 4 + 16 + 4;
    let mut buf = vec![0u8; max_size];
    // ---------------------------------------------------------------------
    // Stage 3: Codec-specific reverse-order encode, then flush the final
    // state (the pinned upstream bitstream contract; see
    // `docs/theory/rans-state.md`).
    // ---------------------------------------------------------------------
    match model.codec {
        RansCodec::Single => {
            let mut writer = ryg_rans_rs::byte::BackwardByteWriter::new(&mut buf);
            let mut state = ryg_rans_rs::byte::RansByteState::new();
            for idx in (0..input.len()).rev() {
                let sym = esyms[input[idx] as usize].as_ref().ok_or_else(|| {
                    RansStreamError::Model(format!("symbol {} missing from model", input[idx]))
                })?;
                ryg_rans_rs::byte::rans_byte_enc_put_symbol(&mut state, &mut writer, sym)
                    .map_err(|_| RansStreamError::EncodeBuffer)?;
            }
            ryg_rans_rs::byte::rans_byte_enc_flush(&state, &mut writer)
                .map_err(|_| RansStreamError::EncodeBuffer)?;
            Ok(writer.encoded().to_vec())
        }
        RansCodec::Interleaved2 => {
            let mut writer = ryg_rans_rs::byte::BackwardByteWriter::new(&mut buf);
            let mut enc = ryg_rans_rs::byte::ByteInterleavedEncoder::new(
                &mut writer,
                model.scale_bits as u32,
            );
            // Build a dense symbol array for the interleaved API (all
            // symbols present in the input have nonzero frequency).
            let dense: Vec<ryg_rans_rs::byte::RansByteEncSymbol> = esyms
                .iter()
                .map(|s| {
                    s.as_ref()
                        .copied()
                        .unwrap_or(ryg_rans_rs::byte::RansByteEncSymbol {
                            // Unreachable in practice: input bytes have nonzero
                            // frequency. Zero-cost placeholder that would fail
                            // loudly if ever encoded.
                            x_max: 0,
                            rcp_freq: 0,
                            bias: 0,
                            cmpl_freq: 0,
                            rcp_shift: 0,
                        })
                })
                .collect();
            enc.encode_reverse(input, &dense)
                .map_err(|_| RansStreamError::EncodeBuffer)?;
            enc.flush().map_err(|_| RansStreamError::EncodeBuffer)?;
            Ok(writer.encoded().to_vec())
        }
    }
}

/// Decode a stream to exactly `out_len` decoded bytes (the caller's
/// descriptor field, in bytes). The model must already be validated; every
/// symbol in the stream must be covered by the model.
///
/// # What
///
/// Rebuilds the decoder symbol tables from the persisted model and turns
/// the encoded bitstream back into the original bytes.
///
/// # Guarantees
///
/// Returns exactly `out_len` bytes or a typed error — the codecs fill a
/// fixed `out_len` buffer and fail (never overrun) on truncated or corrupt
/// streams.
///
/// # Performance
///
/// The cumulative-frequency table is rebuilt per call; the store model
/// cache memoizes decoded models, so this is not on the hot path for
/// repeated reads.
pub fn decode_stream(
    model: &RansModel,
    encoded: &[u8],
    out_len: u64,
) -> Result<Vec<u8>, RansStreamError> {
    // ---------------------------------------------------------------------
    // Stage 1: Rebuild the validated decoder symbols from the model.
    // ---------------------------------------------------------------------
    let dsyms = model
        .build_dec_symbols()
        .map_err(|e| RansStreamError::Model(format!("{e:?}")))?;
    // ---------------------------------------------------------------------
    // Stage 2: Build the cumulative-frequency → symbol table
    // (`1 << scale_bits` entries) and decode per codec.
    // ---------------------------------------------------------------------
    let cum2sym = build_cum2sym(model);
    let mut reader = ryg_rans_rs::byte::ByteReader::new(encoded);
    let mut out = vec![0u8; out_len as usize];
    match model.codec {
        RansCodec::Single => {
            let mut state = ryg_rans_rs::byte::rans_byte_dec_init(&mut reader)
                .map_err(|e| RansStreamError::Decode(format!("{e:?}")))?;
            for slot in out.iter_mut() {
                let cf = ryg_rans_rs::byte::rans_byte_dec_get(&state, model.scale_bits as u32);
                let s = cum2sym[cf as usize] as usize;
                let sym = dsyms[s].as_ref().ok_or_else(|| {
                    RansStreamError::Decode(format!("decoder symbol {s} missing"))
                })?;
                *slot = s as u8;
                ryg_rans_rs::byte::rans_byte_dec_advance_symbol(
                    &mut state,
                    &mut reader,
                    sym,
                    model.scale_bits as u32,
                )
                .map_err(|e| RansStreamError::Decode(format!("{e:?}")))?;
            }
        }
        RansCodec::Interleaved2 => {
            let mut dec = ryg_rans_rs::byte::ByteInterleavedDecoder::new(
                &mut reader,
                model.scale_bits as u32,
            )
            .map_err(|e| RansStreamError::Decode(format!("{e:?}")))?;
            // The interleaved decoder indexes dsyms by symbol; build a
            // dense array (all reachable symbols have nonzero frequency).
            let dense: Vec<ryg_rans_rs::byte::RansByteDecSymbol> = dsyms
                .iter()
                .map(|s| s.unwrap_or(ryg_rans_rs::byte::RansByteDecSymbol { start: 0, freq: 0 }))
                .collect();
            dec.decode(&mut out, &cum2sym, &dense)
                .map_err(|e| RansStreamError::Decode(format!("{e:?}")))?;
        }
    }
    Ok(out)
}

/// Build the cumulative-frequency → symbol table (`cum2sym`): one `u8`
/// symbol per cumulative-frequency slot, `1 << scale_bits` entries in
/// total (16384 at the default scale). Decoding maps the state's
/// cumulative frequency through this table to the emitted byte.
fn build_cum2sym(model: &RansModel) -> Vec<u8> {
    let total = model.total() as usize;
    let mut table = vec![0u8; total];
    let mut start = 0usize;
    for (s, &f) in model.freqs.iter().enumerate() {
        if f == 0 {
            continue;
        }
        let range = start..start + f as usize;
        table[range].fill(s as u8);
        start += f as usize;
    }
    table
}

/// The rANS candidate family for a whole chunk: the pure entropy floor.
///
/// Role: propose `Representation::Rans` (model object + enc object) when
/// the chunk has entropy structure. This family does no match finding —
/// `sequence.rs` supplies that. Scale is fixed at 14 bits with the
/// interleaved codec, matching the sequence families' shared constants.
#[derive(Debug, Default)]
pub struct RansEncoder;

impl Encoder for RansEncoder {
    fn name(&self) -> &'static str {
        "RANS"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        // -----------------------------------------------------------------
        // Stage 1: Input guards — empty/oversized chunks cannot win (a
        // whole-chunk rANS needs at least a model object + encoded stream
        // to beat RAW).
        // -----------------------------------------------------------------
        if input.is_empty() || input.len() as u64 > ctx.limits.max_chunk_size {
            return Vec::new();
        }
        // -----------------------------------------------------------------
        // Stage 2: Train the canonical model on the chunk histogram.
        // Degenerate histograms (≤ 1 distinct symbol) return `None` here —
        // ZERO/FILL/PERIODIC represent those.
        // -----------------------------------------------------------------
        let mut hist = [0u32; 256];
        for &b in input {
            hist[b as usize] += 1;
        }
        let codec = RansCodec::Interleaved2;
        let model = match normalize_histogram(&hist, 14, codec) {
            Some(m) => m,
            None => return Vec::new(),
        };
        // -----------------------------------------------------------------
        // Stage 3: Cheap pre-filter on the entropy estimate. The +256-byte
        // slack approximates the serialized model size before the exact
        // Phase-9G0 gate below; a chunk not clearly below RAW here is
        // skipped without paying for the real encode.
        // -----------------------------------------------------------------
        match model.expected_encoded_len(input.len() as u64) {
            Some(e) if e + 256 < input.len() as u64 => {}
            _ => return Vec::new(),
        }
        let encoded = match encode_stream(input, &model) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        let model_bytes = metadata::encode_model(&model);
        // -----------------------------------------------------------------
        // Stage 4: PHASE-9G0 GATE — the persisted model bytes count
        // against the win. All lengths are bytes: `input.len()` is the
        // logical chunk, `encoded.len() + model_bytes.len()` the persisted
        // payload.
        //
        // attempted:  gate on the encoded payload alone (`enc < raw`)
        // measured:   sequence-family model objects on the real source
        //             tree 277.6 KB -> 74.3 KB after this fix; per-extent
        //             overhead 26.5% -> 11.1% of footprint; tree court
        //             2.388x -> 2.775x post shared-dict; src corpus 4.327x
        // reason:     under `enc < raw`, a stream whose rANS gain was
        //             smaller than its own serialized model was still
        //             stored rANS — persisting a model that could never
        //             pay for itself. The representation then lost to RAW
        //             on total persisted bytes (model + enc + descriptor).
        // decision:   keep rANS only when `enc + model < raw`, so the
        //             model pays for itself out of the encoding gain.
        // limitation: this is the payload gate (enc + model vs raw); the
        //             descriptor bytes are accounted separately in the
        //             candidate cost, which decides final selection
        //             against RAW and the other families.
        // evidence:   sealed campaign-1787684918-80e36c8 (Phase 9G0).
        // -----------------------------------------------------------------
        if encoded.len().saturating_add(model_bytes.len()) >= input.len() {
            return Vec::new();
        }
        // -----------------------------------------------------------------
        // Stage 5: Assemble the content-addressed objects, descriptor, and
        // exact persisted-byte cost.
        // -----------------------------------------------------------------
        let enc_obj = ObjectRecord::data(encoded);
        let model_obj = ObjectRecord::model(model_bytes);
        let rep = Representation::Rans {
            model: model_obj.id,
            enc_obj: enc_obj.id,
            scale_bits: model.scale_bits,
            codec,
            len: input.len() as u64,
        };
        let split = ByteSplit {
            reference: 64,
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

/// The rANS-coded residual candidate family: `X = B ⊕ D` where `D` is a
/// rANS-coded XOR difference stream (`docs/format/ondisk-v1.md` §7,
/// residual kind 0x03).
///
/// Role: propose `Representation::BaseResidual` with a `RansCoded`
/// residual when the XOR difference has entropy structure. Unlike
/// `BaseSequence` (delta.rs), this is a *positional* residual: base and
/// target must have equal length.
#[derive(Debug, Default)]
pub struct RansResidualEncoder;

impl Encoder for RansResidualEncoder {
    fn name(&self) -> &'static str {
        "RANS_RESIDUAL"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        let mut out = Vec::new();
        // -----------------------------------------------------------------
        // Stage 1: One pass per base — depth-capped, equal-length bases
        // only (positional residual), and only when the diff has real
        // structure.
        // -----------------------------------------------------------------
        for base in ctx.bases {
            if base.depth >= ctx.limits.max_reference_depth {
                continue;
            }
            if base.bytes.len() != input.len() {
                continue;
            }
            let (diffs, _) = diff_summary(input, &base.bytes);
            // Residual rANS only makes sense when there is real difference
            // structure; skip near-identical and near-random.
            if diffs == 0 || diffs == input.len() {
                continue;
            }
            // -----------------------------------------------------------------
            // Stage 2: XOR diff → histogram → canonical model.
            // -----------------------------------------------------------------
            let diff: Vec<u8> = input
                .iter()
                .zip(base.bytes.iter())
                .map(|(&x, &b)| x ^ b)
                .collect();
            let mut hist = [0u32; 256];
            for &b in diff.iter() {
                hist[b as usize] += 1;
            }
            let codec = RansCodec::Interleaved2;
            let model = match normalize_histogram(&hist, 14, codec) {
                Some(m) => m,
                None => continue,
            };
            // -----------------------------------------------------------------
            // Stage 3: Entropy-estimate pre-filter (the +256-byte slack
            // approximates the model size; see `RansEncoder::encode`).
            // -----------------------------------------------------------------
            let expected = match model.expected_encoded_len(diff.len() as u64) {
                Some(e) => e,
                None => continue,
            };
            if expected + 256 >= diff.len() as u64 {
                continue;
            }
            let encoded = match encode_stream(&diff, &model) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let model_bytes = metadata::encode_model(&model);
            // -----------------------------------------------------------------
            // Stage 4: PHASE-9G0 GATE — the persisted model bytes count
            // against the win (mirror of `RansEncoder::encode`): keep rANS
            // only when `enc + model < diff.len()` — `diff.len()` is the
            // raw size of the XOR stream. A model that cannot pay for
            // itself from the encoding gain must not be persisted (sealed
            // campaign-1787684918-80e36c8; sequence model objects on the
            // real tree 277.6 KB -> 74.3 KB).
            // -----------------------------------------------------------------
            if encoded.len().saturating_add(model_bytes.len()) >= diff.len() {
                continue;
            }
            let enc_obj = ObjectRecord::data(encoded);
            let model_obj = ObjectRecord::model(model_bytes);
            let residual = Residual::RansCoded {
                len: input.len() as u64,
                enc_obj: enc_obj.id,
                model: model_obj.id,
                scale_bits: model.scale_bits,
                codec,
                decoded_len: input.len() as u64,
            };
            let rep = Representation::BaseResidual {
                base: base.id,
                base_len: base.bytes.len() as u64,
                residual,
                len: input.len() as u64,
            };
            let split = ByteSplit {
                reference: 32 + 64,
                ..Default::default()
            };
            let cost = crate::core::candidate::account_objects(
                crate::core::cost::estimate(&rep, &split, model_obj.payload.len() as u64),
                &[enc_obj.clone(), model_obj.clone()],
            );
            out.push(Candidate {
                representation: rep,
                objects: vec![enc_obj, model_obj],
                cost,
                content_id: ctx.content_id,
            });
        }
        out
    }
}

/// Helper: encode + decode a chunk under a model, verifying the round trip
/// byte-for-byte (used by tests).
pub fn verify_roundtrip(input: &[u8], model: &RansModel) -> Result<(), RansStreamError> {
    let encoded = encode_stream(input, model)?;
    let decoded = decode_stream(model, &encoded, input.len() as u64)?;
    if decoded != input {
        return Err(RansStreamError::LengthMismatch {
            expected: input.len() as u64,
            actual: decoded.len() as u64,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::candidate::{BaseChunk, CandidateContext};
    use crate::core::cost::Policy;
    use crate::core::extent::ChunkId;
    use crate::core::limits::Limits;
    use crate::core::materialize::materialize_to_vec;

    fn hist_of(data: &[u8]) -> [u32; 256] {
        let mut h = [0u32; 256];
        for &b in data {
            h[b as usize] += 1;
        }
        h
    }

    #[test]
    fn single_roundtrip() {
        let data: Vec<u8> = (0..20000u32).map(|i| ((i * 13) % 53) as u8).collect();
        let model = normalize_histogram(&hist_of(&data), 14, RansCodec::Single).unwrap();
        let encoded = encode_stream(&data, &model).unwrap();
        let decoded = decode_stream(&model, &encoded, data.len() as u64).unwrap();
        assert_eq!(decoded, data);
        assert!(encoded.len() < data.len());
    }

    #[test]
    fn interleaved2_roundtrip() {
        let data: Vec<u8> = (0..30000u32).map(|i| ((i * 11) % 61) as u8).collect();
        let model = normalize_histogram(&hist_of(&data), 14, RansCodec::Interleaved2).unwrap();
        let encoded = encode_stream(&data, &model).unwrap();
        let decoded = decode_stream(&model, &encoded, data.len() as u64).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn truncated_stream_errors() {
        let data: Vec<u8> = (0..5000u32).map(|i| ((i * 3) % 17) as u8).collect();
        let model = normalize_histogram(&hist_of(&data), 14, RansCodec::Single).unwrap();
        let encoded = encode_stream(&data, &model).unwrap();
        // Truncate to half and expect a typed error, not a panic.
        let truncated = &encoded[..encoded.len() / 2];
        let res = decode_stream(&model, truncated, data.len() as u64);
        assert!(res.is_err());
    }

    #[test]
    fn corrupt_stream_errors() {
        let data: Vec<u8> = (0..5000u32).map(|i| ((i * 3) % 17) as u8).collect();
        let model = normalize_histogram(&hist_of(&data), 14, RansCodec::Single).unwrap();
        let mut encoded = encode_stream(&data, &model).unwrap();
        let mid = encoded.len() / 2;
        encoded[mid] ^= 0xFF;
        let res = decode_stream(&model, &encoded, data.len() as u64);
        // Either a typed error or wrong bytes — never a panic.
        if let Ok(decoded) = res {
            assert_ne!(decoded, data);
        }
    }

    #[test]
    fn rans_encoder_proposes_and_validates() {
        let data: Vec<u8> = (0..65536u32).map(|i| ((i * 5) % 97) as u8).collect();
        let enc = RansEncoder;
        let ctx = CandidateContext {
            limits: &Limits::default(),
            policy: &Policy::default(),
            content_id: ChunkId::of(&data),
            bases: &[],
            dedup: None,
        };
        let cands = enc.encode(&data, &ctx);
        assert_eq!(cands.len(), 1);
        // Materialize via a resolver that has the candidate's own objects.
        let map: std::collections::HashMap<ChunkId, Vec<u8>> = cands[0]
            .objects
            .iter()
            .map(|o| (o.id, o.payload.clone()))
            .collect();
        let resolver = crate::tests::helpers::MemResolver::from_map(map);
        let out =
            materialize_to_vec(&cands[0].representation, &resolver, &Limits::default()).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn rans_skips_incompressible() {
        // uniform-ish data: rANS cannot beat raw
        let data: Vec<u8> = (0..65536u32)
            .map(|i| ((i * 7 + i / 256) % 256) as u8)
            .collect();
        let enc = RansEncoder;
        let ctx = CandidateContext {
            limits: &Limits::default(),
            policy: &Policy::default(),
            content_id: ChunkId::of(&data),
            bases: &[],
            dedup: None,
        };
        assert!(enc.encode(&data, &ctx).is_empty());
    }

    #[test]
    fn rans_residual_encoder_proposes() {
        // base = zeros; target = structured XOR diff
        let base = vec![0u8; 8192];
        let mut target = vec![0u8; 8192];
        for (i, slot) in target.iter_mut().enumerate() {
            *slot = (i % 9) as u8;
        }
        let base_id = ChunkId::of(&base);
        let base_chunk = BaseChunk {
            id: base_id,
            bytes: base,
            depth: 0,
        };
        let enc = RansResidualEncoder;
        let ctx = CandidateContext {
            limits: &Limits::default(),
            policy: &Policy::default(),
            content_id: ChunkId::of(&target),
            bases: &[base_chunk],
            dedup: None,
        };
        let cands = enc.encode(&target, &ctx);
        assert_eq!(cands.len(), 1);
        assert!(matches!(
            cands[0].representation,
            Representation::BaseResidual { .. }
        ));
    }
}
