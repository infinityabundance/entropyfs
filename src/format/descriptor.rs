//! Extent descriptor codec: the byte encoding of
//! `core::representation::Representation` (`docs/format/ondisk-v1.md` §7).
//!
//! Mirrors `Representation::encoded_size` exactly — a test asserts the two
//! agree for randomized descriptors so the accounting mirror can never
//! drift from the real codec.

#![forbid(unsafe_code)]

use crate::core::extent::ChunkId;
use crate::core::representation::{RansCodec, Representation, Residual, TransformId, UniverseId};
use crate::format::codec::{CodecError, Reader, Writer};

/// Representation tags (must match `Representation::tag`).
pub const TAG_ZERO: u8 = 0x01;
/// Tag: fill (single repeated byte).
pub const TAG_FILL: u8 = 0x02;
/// Tag: raw literal object.
pub const TAG_RAW: u8 = 0x03;
/// Tag: rANS-encoded stream.
pub const TAG_RANS: u8 = 0x04;
/// Tag: exact sub-range reference.
pub const TAG_EXACT_REF: u8 = 0x05;
/// Tag: base + exact residual.
pub const TAG_BASE_RESIDUAL: u8 = 0x06;
/// Tag: combinatorial sparse configuration.
pub const TAG_SPARSE: u8 = 0x07;
/// Tag: low-cardinality palette configuration.
pub const TAG_PALETTE: u8 = 0x08;
/// Tag: periodic structure.
pub const TAG_PERIODIC: u8 = 0x09;
/// Tag: entropy universe reference.
pub const TAG_ENTROPY_REF: u8 = 0x0A;
/// Tag: inline literal bytes.
pub const TAG_INLINE: u8 = 0x0B;
/// Tag: permutation (factoradic rank).
pub const TAG_PERMUTATION: u8 = 0x0C;
/// Tag: local-match + entropy (three rANS/raw streams).
pub const TAG_SEQUENCE_RANS: u8 = 0x0D;

/// Residual kinds.
pub const RESIDUAL_XOR_SPARSE: u8 = 0x01;
/// Residual kind: sparse range replacement.
pub const RESIDUAL_RANGE_REPLACE: u8 = 0x02;
/// Residual kind: rANS-coded stream.
pub const RESIDUAL_RANS_CODED: u8 = 0x03;

/// Encode a representation descriptor.
pub fn encode(rep: &Representation) -> Result<Vec<u8>, CodecError> {
    let mut w = Writer::with_capacity(rep.encoded_size() as usize);
    match rep {
        Representation::Zero { len } => {
            w.u8(TAG_ZERO);
            w.u32(*len as u32);
        }
        Representation::Fill { value, len } => {
            w.u8(TAG_FILL);
            w.u32(*len as u32);
            w.u8(*value);
        }
        Representation::Inline { data } => {
            w.u8(TAG_INLINE);
            w.u32(data.len() as u32);
            w.bytes(data);
        }
        Representation::Raw { obj, len } => {
            w.u8(TAG_RAW);
            w.u32(*len as u32);
            w.bytes(obj.as_bytes());
        }
        Representation::Rans {
            model,
            enc_obj,
            scale_bits,
            codec,
            len,
        } => {
            w.u8(TAG_RANS);
            w.u32(*len as u32);
            w.bytes(model.as_bytes());
            w.bytes(enc_obj.as_bytes());
            w.u8(*scale_bits);
            w.u8(codec.tag());
        }
        Representation::ExactRef { target, off, len } => {
            w.u8(TAG_EXACT_REF);
            w.u32(*len as u32);
            w.bytes(target.as_bytes());
            w.u32(*off as u32);
        }
        Representation::BaseResidual {
            base,
            base_len,
            residual,
            len,
        } => {
            w.u8(TAG_BASE_RESIDUAL);
            w.u32(*len as u32);
            w.bytes(base.as_bytes());
            w.u32(*base_len as u32);
            encode_residual(&mut w, residual)?;
        }
        Representation::Sparse {
            k,
            rank,
            literals,
            len,
        } => {
            w.u8(TAG_SPARSE);
            w.u32(*len as u32);
            w.u32(*k);
            w.u128(*rank);
            w.bytes(literals);
        }
        Representation::Palette {
            palette,
            counts,
            rank,
            len,
        } => {
            w.u8(TAG_PALETTE);
            w.u32(*len as u32);
            let m = palette.len();
            if m > 255 {
                return Err(CodecError::Malformed);
            }
            w.u8(m as u8);
            w.bytes(palette);
            for &c in counts {
                w.u32(c);
            }
            w.u128(*rank);
        }
        Representation::Periodic {
            period,
            pattern,
            count,
            tail,
            len,
        } => {
            w.u8(TAG_PERIODIC);
            w.u32(*len as u32);
            w.u32(*period);
            w.bytes(pattern);
            w.u32(*count);
            w.u32(tail.len() as u32);
            w.bytes(tail);
        }
        Representation::EntropyRef {
            universe,
            seed,
            coordinate,
            transform,
            residual,
            len,
        } => {
            w.u8(TAG_ENTROPY_REF);
            w.u32(*len as u32);
            w.u8(universe.tag());
            w.bytes(seed);
            w.u64(*coordinate);
            w.u8(transform.tag());
            encode_residual(&mut w, residual)?;
        }
        Representation::Permutation {
            rank,
            alphabet,
            len,
        } => {
            w.u8(TAG_PERMUTATION);
            w.u32(*len as u32);
            w.u128(*rank);
            w.bytes(alphabet);
        }
        Representation::SequenceRans {
            model,
            enc_obj,
            scale_bits,
            codec,
            seq_len,
            lit_len,
            off_len,
            cmds,
            lit_out,
            len,
        } => {
            w.u8(TAG_SEQUENCE_RANS);
            w.u32(*len as u32);
            w.bytes(model.as_bytes());
            w.bytes(enc_obj.as_bytes());
            w.u8(*scale_bits);
            w.u8(codec.tag());
            w.u32(*seq_len);
            w.u32(*lit_len);
            w.u32(*off_len);
            w.u32(*cmds);
            w.u32(*lit_out);
        }
    }
    Ok(w.into_bytes())
}

/// Encode a residual.
pub fn encode_residual(w: &mut Writer, r: &Residual) -> Result<(), CodecError> {
    match r {
        Residual::XorSparse { edits, .. } => {
            w.u8(RESIDUAL_XOR_SPARSE);
            w.u32(edits.len() as u32);
            for e in edits {
                w.u32(e.pos);
                w.u8(e.val);
            }
        }
        Residual::RangeReplace {
            changes, literals, ..
        } => {
            w.u8(RESIDUAL_RANGE_REPLACE);
            w.u32(changes.len() as u32);
            for c in changes {
                w.u32(c.start);
                w.u32(c.end);
            }
            w.bytes(literals);
        }
        Residual::RansCoded {
            enc_obj,
            model,
            scale_bits,
            codec,
            decoded_len,
            ..
        } => {
            w.u8(RESIDUAL_RANS_CODED);
            w.bytes(enc_obj.as_bytes());
            w.bytes(model.as_bytes());
            w.u8(*scale_bits);
            w.u8(codec.tag());
            w.u32(*decoded_len as u32);
        }
    }
    Ok(())
}

/// Decode a representation descriptor.
///
/// `max_descriptor_bytes` bounds the input; `max_inline` bounds INLINE
/// payloads; `max_palette` bounds palette cardinality; `max_period` bounds
/// periodic patterns. All structural invariants are validated by the
/// returned representation's own `validate()`.
pub fn decode(
    bytes: &[u8],
    max_descriptor_bytes: u64,
    max_inline: u64,
    max_palette: usize,
    max_period: u32,
    max_chunk_size: u64,
) -> Result<Representation, CodecError> {
    if bytes.len() as u64 > max_descriptor_bytes {
        return Err(CodecError::TooLong);
    }
    let mut r = Reader::new(bytes);
    let tag = r.u8()?;
    let len = r.u32()? as u64;
    if len > max_chunk_size {
        return Err(CodecError::TooLong);
    }
    let rep = match tag {
        TAG_ZERO => Representation::Zero { len },
        TAG_FILL => {
            let value = r.u8()?;
            Representation::Fill { value, len }
        }
        TAG_INLINE => {
            if len > max_inline {
                return Err(CodecError::TooLong);
            }
            let data = r.take(len as usize)?.to_vec();
            Representation::Inline { data }
        }
        TAG_RAW => {
            let obj = read_id(&mut r)?;
            Representation::Raw { obj, len }
        }
        TAG_RANS => {
            let model = read_id(&mut r)?;
            let enc_obj = read_id(&mut r)?;
            let scale_bits = r.u8()?;
            let codec = RansCodec::from_u8(r.u8()?).ok_or(CodecError::Malformed)?;
            Representation::Rans {
                model,
                enc_obj,
                scale_bits,
                codec,
                len,
            }
        }
        TAG_EXACT_REF => {
            let target = read_id(&mut r)?;
            let off = r.u32()? as u64;
            Representation::ExactRef { target, off, len }
        }
        TAG_BASE_RESIDUAL => {
            let base = read_id(&mut r)?;
            let base_len = r.u32()? as u64;
            let residual = decode_residual(&mut r, len)?;
            Representation::BaseResidual {
                base,
                base_len,
                residual,
                len,
            }
        }
        TAG_SPARSE => {
            let k = r.u32()?;
            let rank = r.u128()?;
            let literals = r.take(k as usize)?.to_vec();
            Representation::Sparse {
                k,
                rank,
                literals,
                len,
            }
        }
        TAG_PALETTE => {
            let m = r.u8()? as usize;
            if m > max_palette || m == 0 {
                return Err(CodecError::Malformed);
            }
            let palette = r.take(m)?.to_vec();
            let mut counts = Vec::with_capacity(m);
            for _ in 0..m {
                counts.push(r.u32()?);
            }
            let rank = r.u128()?;
            Representation::Palette {
                palette,
                counts,
                rank,
                len,
            }
        }
        TAG_PERIODIC => {
            let period = r.u32()?;
            if period == 0 || period > max_period {
                return Err(CodecError::Malformed);
            }
            let pattern = r.take(period as usize)?.to_vec();
            let count = r.u32()?;
            let tail_len = r.u32()?;
            if tail_len as u64 >= period as u64 {
                return Err(CodecError::Malformed);
            }
            let tail = r.take(tail_len as usize)?.to_vec();
            Representation::Periodic {
                period,
                pattern,
                count,
                tail,
                len,
            }
        }
        TAG_ENTROPY_REF => {
            let universe = UniverseId::from_u8(r.u8()?).ok_or(CodecError::Malformed)?;
            let seed = read_seed(&mut r)?;
            let coordinate = r.u64()?;
            let transform = TransformId::from_u8(r.u8()?).ok_or(CodecError::Malformed)?;
            let residual = decode_residual(&mut r, len)?;
            Representation::EntropyRef {
                universe,
                seed,
                coordinate,
                transform,
                residual,
                len,
            }
        }
        TAG_PERMUTATION => {
            let rank = r.u128()?;
            let alphabet = r.take(len as usize)?.to_vec();
            Representation::Permutation {
                rank,
                alphabet,
                len,
            }
        }
        TAG_SEQUENCE_RANS => {
            let model = read_id(&mut r)?;
            let enc_obj = read_id(&mut r)?;
            let scale_bits = r.u8()?;
            let codec = RansCodec::from_u8(r.u8()?).ok_or(CodecError::Malformed)?;
            let seq_len = r.u32()?;
            let lit_len = r.u32()?;
            let off_len = r.u32()?;
            let cmds = r.u32()?;
            let lit_out = r.u32()?;
            Representation::SequenceRans {
                model,
                enc_obj,
                scale_bits,
                codec,
                seq_len,
                lit_len,
                off_len,
                cmds,
                lit_out,
                len,
            }
        }
        _ => return Err(CodecError::Malformed),
    };
    if !r.done() {
        return Err(CodecError::Malformed);
    }
    Ok(rep)
}

/// Decode a residual (the representation length is needed to validate).
pub fn decode_residual(r: &mut Reader<'_>, repr_len: u64) -> Result<Residual, CodecError> {
    let kind = r.u8()?;
    match kind {
        RESIDUAL_XOR_SPARSE => {
            let count = r.u32()?;
            let mut edits = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let pos = r.u32()?;
                let val = r.u8()?;
                edits.push(crate::core::representation::Edit { pos, val });
            }
            Ok(Residual::XorSparse {
                len: repr_len,
                edits,
            })
        }
        RESIDUAL_RANGE_REPLACE => {
            let count = r.u32()?;
            let mut changes = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let start = r.u32()?;
                let end = r.u32()?;
                changes.push(crate::core::representation::RangeChange { start, end });
            }
            let mut literal_total: u64 = 0;
            for c in &changes {
                if c.start >= c.end {
                    return Err(CodecError::Malformed);
                }
                literal_total += (c.end - c.start) as u64;
            }
            let literals = r.take(literal_total as usize)?.to_vec();
            Ok(Residual::RangeReplace {
                len: repr_len,
                changes,
                literals,
            })
        }
        RESIDUAL_RANS_CODED => {
            let enc_obj = read_id(r)?;
            let model = read_id(r)?;
            let scale_bits = r.u8()?;
            let codec = RansCodec::from_u8(r.u8()?).ok_or(CodecError::Malformed)?;
            let decoded_len = r.u32()? as u64;
            Ok(Residual::RansCoded {
                len: repr_len,
                enc_obj,
                model,
                scale_bits,
                codec,
                decoded_len,
            })
        }
        _ => Err(CodecError::Malformed),
    }
}

fn read_id(r: &mut Reader<'_>) -> Result<ChunkId, CodecError> {
    let b = r.take(32)?;
    Ok(ChunkId::new(b.try_into().unwrap()))
}

fn read_seed(r: &mut Reader<'_>) -> Result<[u8; 16], CodecError> {
    let b = r.take(16)?;
    Ok(b.try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::representation::{Edit, RangeChange};
    use proptest::prelude::*;

    fn sample_reps() -> Vec<Representation> {
        let id = ChunkId::of(b"sample");
        vec![
            Representation::Zero { len: 65536 },
            Representation::Fill {
                value: 7,
                len: 1024,
            },
            Representation::Inline {
                data: b"hello".to_vec(),
            },
            Representation::Raw {
                obj: id,
                len: 65536,
            },
            Representation::Rans {
                model: id,
                enc_obj: id,
                scale_bits: 14,
                codec: RansCodec::Interleaved2,
                len: 4096,
            },
            Representation::ExactRef {
                target: id,
                off: 100,
                len: 512,
            },
            Representation::BaseResidual {
                base: id,
                base_len: 4096,
                residual: Residual::XorSparse {
                    len: 4096,
                    edits: vec![
                        Edit { pos: 1, val: 2 },
                        Edit {
                            pos: 300,
                            val: 0xAA,
                        },
                    ],
                },
                len: 4096,
            },
            Representation::BaseResidual {
                base: id,
                base_len: 64,
                residual: Residual::RangeReplace {
                    len: 64,
                    changes: vec![RangeChange { start: 4, end: 10 }],
                    literals: vec![9; 6],
                },
                len: 64,
            },
            Representation::Sparse {
                k: 2,
                rank: 17,
                literals: vec![1, 2],
                len: 64,
            },
            Representation::Palette {
                palette: vec![0x10, 0x20, 0x30],
                counts: vec![40, 20, 4],
                rank: 5,
                len: 64,
            },
            Representation::Periodic {
                period: 4,
                pattern: b"abcd".to_vec(),
                count: 3,
                tail: b"xy".to_vec(),
                len: 14,
            },
            Representation::EntropyRef {
                universe: UniverseId::UniformXofV1,
                seed: [3u8; 16],
                coordinate: 9,
                transform: TransformId::Identity,
                residual: Residual::XorSparse {
                    len: 64,
                    edits: Vec::new(),
                },
                len: 64,
            },
            Representation::Permutation {
                rank: 42,
                alphabet: (200u8..230).collect(),
                len: 30,
            },
            Representation::SequenceRans {
                model: id,
                enc_obj: id,
                scale_bits: 14,
                codec: RansCodec::Interleaved2,
                seq_len: 100,
                lit_len: 50,
                off_len: 20,
                cmds: 30,
                lit_out: 40,
                len: 4096,
            },
        ]
    }

    #[test]
    fn roundtrip_all_families() {
        for rep in sample_reps() {
            let bytes = encode(&rep).unwrap();
            let back = decode(&bytes, 8192, 4096, 16, 1024, 262144).unwrap();
            assert_eq!(back, rep, "family roundtrip failed for {rep:?}");
        }
    }

    #[test]
    fn encoded_size_matches_codec() {
        for rep in sample_reps() {
            let bytes = encode(&rep).unwrap();
            assert_eq!(
                bytes.len() as u64,
                rep.encoded_size(),
                "encoded_size mirror drifted for {rep:?}"
            );
        }
    }

    #[test]
    fn corrupt_descriptors_error() {
        for rep in sample_reps() {
            let bytes = encode(&rep).unwrap();
            for flip in [0usize, 1, 2, bytes.len() - 1] {
                if bytes.len() < 2 {
                    continue;
                }
                let mut bad = bytes.clone();
                bad[flip] ^= 0xFF;
                // Some flips produce a *valid different* descriptor (e.g. a
                // different fill value); the contract is: never panic, and
                // either a typed error or a structurally valid descriptor.
                if let Ok(rep2) = decode(&bad, 8192, 4096, 16, 1024, 262144) {
                    // A flipped byte may produce a valid descriptor (e.g.
                    // a different fill value); the contract is: never
                    // panic, and the result must pass structural
                    // validation (or be rejected — either is fine here).
                    let _ = rep2.validate(&crate::core::limits::Limits::default());
                }
            }
        }
    }

    #[test]
    fn truncated_errors() {
        for rep in sample_reps() {
            let bytes = encode(&rep).unwrap();
            for cut in 0..bytes.len() {
                assert!(
                    decode(&bytes[..cut], 8192, 4096, 16, 1024, 262144).is_err(),
                    "cut at {cut} of {} for {rep:?}",
                    bytes.len()
                );
            }
        }
    }

    #[test]
    fn oversized_descriptor_rejected() {
        let rep = Representation::Sparse {
            k: 3,
            rank: 5,
            literals: vec![1, 2, 3],
            len: 64,
        };
        let bytes = encode(&rep).unwrap();
        assert_eq!(
            decode(&bytes, bytes.len() as u64 - 1, 4096, 16, 1024, 262144),
            Err(CodecError::TooLong)
        );
    }

    proptest! {
        #[test]
        fn roundtrip_randomized(len in 0u32..65536u32, a in any::<u8>(), b in any::<u8>(), off in 0u32..65536u32) {
            let id = ChunkId::of(&[a, b]);
            let reps = vec![
                Representation::Zero { len: len as u64 },
                Representation::Fill { value: a, len: len as u64 },
                Representation::Raw { obj: id, len: len as u64 },
                Representation::ExactRef { target: id, off: off as u64, len: len as u64 },
                Representation::Sparse { k: 1, rank: (a as u128) % 2, literals: vec![b], len: len.max(1) as u64 },
                Representation::Periodic { period: 1, pattern: vec![a], count: len, tail: vec![], len: len as u64 },
            ];
            for rep in reps {
                if rep.validate(&crate::core::limits::Limits::default()).is_err() {
                    continue;
                }
                let bytes = encode(&rep).unwrap();
                let back = decode(&bytes, 8192, 4096, 16, 1024, 262144).unwrap();
                assert_eq!(back, rep);
                assert_eq!(bytes.len() as u64, rep.encoded_size());
            }
        }
    }
}
