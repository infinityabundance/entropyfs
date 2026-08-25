//! rANS stream encoding/decoding via `ryg-rans-rs`, plus the
//! rANS-coded residual candidate family.
//!
//! The scalar paths are the authority; both codecs share the upstream
//! bitstream contract (`docs/theory/rans-state.md`).

#![forbid(unsafe_code)]

use crate::core::candidate::{Candidate, CandidateContext, Encoder, ObjectRecord};
use crate::core::cost::ByteSplit;
use crate::core::representation::{RansCodec, Representation, Residual};
use crate::entropy::residual::diff_summary;
use crate::rans::metadata;
use crate::rans::model::{RansModel, normalize_histogram};

/// rANS stream errors.
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

/// Encode `input` under `model`. Deterministic; worst-case output is
/// `4·len + 20` bytes.
pub fn encode_stream(input: &[u8], model: &RansModel) -> Result<Vec<u8>, RansStreamError> {
    let esyms = model
        .build_enc_symbols()
        .map_err(|e| RansStreamError::Model(format!("{e:?}")))?;
    let max_size = input.len() * 4 + 16 + 4;
    let mut buf = vec![0u8; max_size];
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

/// Decode a stream to exactly `out_len` bytes. The model must already be
/// validated. The cumulative-frequency table is rebuilt per call (the store
/// model cache memoizes decoded models, so this is not on the hot path for
/// repeated reads).
pub fn decode_stream(
    model: &RansModel,
    encoded: &[u8],
    out_len: u64,
) -> Result<Vec<u8>, RansStreamError> {
    let dsyms = model
        .build_dec_symbols()
        .map_err(|e| RansStreamError::Model(format!("{e:?}")))?;
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

/// Build the cumulative-frequency → symbol table (`cum2sym`), size
/// `1 << scale_bits`.
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

/// The rANS candidate family for a whole chunk.
#[derive(Debug, Default)]
pub struct RansEncoder;

impl Encoder for RansEncoder {
    fn name(&self) -> &'static str {
        "RANS"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        if input.is_empty() || input.len() as u64 > ctx.limits.max_chunk_size {
            return Vec::new();
        }
        let mut hist = [0u32; 256];
        for &b in input {
            hist[b as usize] += 1;
        }
        let codec = RansCodec::Interleaved2;
        let model = match normalize_histogram(&hist, 14, codec) {
            Some(m) => m,
            None => return Vec::new(),
        };
        // Cheap guard: if expected length is not clearly below raw, skip.
        match model.expected_encoded_len(input.len() as u64) {
            Some(e) if e + 256 < input.len() as u64 => {}
            _ => return Vec::new(),
        }
        let encoded = match encode_stream(input, &model) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        if encoded.len() >= input.len() {
            return Vec::new();
        }
        let model_bytes = metadata::encode_model(&model);
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
#[derive(Debug, Default)]
pub struct RansResidualEncoder;

impl Encoder for RansResidualEncoder {
    fn name(&self) -> &'static str {
        "RANS_RESIDUAL"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        let mut out = Vec::new();
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
            if encoded.len() >= diff.len() {
                continue;
            }
            let model_bytes = metadata::encode_model(&model);
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
