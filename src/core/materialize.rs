//! The bounded materializer: `X = Materialize(D)`.
//!
//! A deterministic interpreter over representation descriptors. Every loop
//! is length-bounded by persisted lengths validated *before* allocation;
//! a deterministic operation budget bounds CPU; reference depth is capped
//! (ADR-0005, `docs/security/resource-bounds.md`).

#![forbid(unsafe_code)]

use std::ops::Range;

use crate::core::extent::ChunkId;
use crate::core::limits::Limits;
use crate::core::representation::{RansCodec, Representation, Residual, TransformId, UniverseId};

/// External services the materializer needs: object fetch, chunk-descriptor
/// resolution (for EXACT_REF), rANS decode, and universe materialization.
///
/// Implemented by the store layer; `core` only defines the contract.
pub trait DecoderContext {
    /// Fetch a persisted object's bytes by content id.
    fn fetch_object(&self, id: &ChunkId) -> Result<Vec<u8>, MaterializeError>;

    /// Resolve a logical chunk id to its representation descriptor
    /// (chunk index lookup).
    fn fetch_descriptor(&self, id: &ChunkId) -> Result<Representation, MaterializeError>;

    /// Decode a rANS stream with the given model bytes.
    fn decode_rans(
        &self,
        model: &[u8],
        encoded: &[u8],
        scale_bits: u8,
        codec: RansCodec,
        out_len: u64,
    ) -> Result<Vec<u8>, MaterializeError>;

    /// Materialize `range` bytes from an entropy universe.
    fn universe_bytes(
        &self,
        universe: UniverseId,
        seed: [u8; 16],
        coordinate: u64,
        range: Range<u64>,
    ) -> Result<Vec<u8>, MaterializeError>;
}

/// Materialization errors. All are typed; none panics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterializeError {
    /// Descriptor validation failed.
    InvalidDescriptor(String),
    /// Output length exceeds limits.
    OutputTooLarge {
        /// Requested length.
        requested: u64,
        /// Format maximum.
        max: u64,
    },
    /// Allocation exceeds limits.
    AllocTooLarge {
        /// Requested allocation.
        requested: u64,
        /// Format maximum.
        max: u64,
    },
    /// Reference depth exceeded.
    DepthExceeded {
        /// Depth reached.
        depth: u8,
        /// Depth cap.
        max: u8,
    },
    /// Operation budget exceeded.
    BudgetExceeded,
    /// Referenced object missing.
    MissingObject(ChunkId),
    /// Referenced chunk descriptor missing.
    MissingChunk(ChunkId),
    /// Referenced range outside the target chunk.
    RangeOutOfBounds,
    /// rANS decode failed (bad model or stream).
    RansDecode(String),
    /// SequenceRans decode failed (bad model object, streams, or commands).
    Sequence(String),
    /// Universe materialization failed.
    Universe(String),
    /// A residual could not be applied (structurally invalid).
    Residual(String),
}

impl std::fmt::Display for MaterializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for MaterializeError {}

/// Materialize a chunk descriptor into `output`.
///
/// `depth` is the current reference depth (0 at top level); each
/// EXACT_REF / BASE_RESIDUAL resolution increments it and enforces the cap.
pub fn materialize(
    desc: &Representation,
    ctx: &dyn DecoderContext,
    limits: &Limits,
    depth: u8,
    budget: &mut u64,
    output: &mut [u8],
) -> Result<(), MaterializeError> {
    if output.len() as u64 != desc.len() {
        return Err(MaterializeError::InvalidDescriptor(
            "output length does not match descriptor length".into(),
        ));
    }
    if desc.len() > limits.max_chunk_size {
        return Err(MaterializeError::OutputTooLarge {
            requested: desc.len(),
            max: limits.max_chunk_size,
        });
    }
    if depth > limits.max_reference_depth {
        return Err(MaterializeError::DepthExceeded {
            depth,
            max: limits.max_reference_depth,
        });
    }
    spend(desc.len() / 8 + 1, budget)?;

    match desc {
        Representation::Zero { .. } => {
            output.fill(0);
            Ok(())
        }
        Representation::Fill { value, .. } => {
            output.fill(*value);
            Ok(())
        }
        Representation::Inline { data } => {
            if data.len() != output.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "inline length mismatch".into(),
                ));
            }
            output.copy_from_slice(data);
            Ok(())
        }
        Representation::Raw { obj, .. } => {
            let bytes = ctx.fetch_object(obj)?;
            if bytes.len() as u64 != desc.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "raw object length mismatch".into(),
                ));
            }
            output.copy_from_slice(&bytes);
            Ok(())
        }
        Representation::Rans {
            model,
            enc_obj,
            scale_bits,
            codec,
            len,
        } => {
            if *len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *len,
                    max: limits.max_alloc_bytes,
                });
            }
            let model_bytes = ctx.fetch_object(model)?;
            if model_bytes.len() as u64 > limits.max_model_bytes {
                return Err(MaterializeError::InvalidDescriptor(
                    "model object too large".into(),
                ));
            }
            let encoded = ctx.fetch_object(enc_obj)?;
            let decoded = ctx.decode_rans(&model_bytes, &encoded, *scale_bits, *codec, *len)?;
            if decoded.len() as u64 != desc.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "rans decoded length mismatch".into(),
                ));
            }
            output.copy_from_slice(&decoded);
            Ok(())
        }
        Representation::ExactRef { target, off, len } => {
            // Fetch the target chunk descriptor and materialize it with
            // depth+1, then copy the sub-range.
            let target_desc = ctx.fetch_descriptor(target)?;
            if *off as u128 + *len as u128 > target_desc.len() as u128 {
                return Err(MaterializeError::RangeOutOfBounds);
            }
            let target_len = target_desc.len();
            if target_len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: target_len,
                    max: limits.max_alloc_bytes,
                });
            }
            let mut full = vec![0u8; target_len as usize];
            materialize(&target_desc, ctx, limits, depth + 1, budget, &mut full)?;
            let start = *off as usize;
            let end = start + *len as usize;
            output.copy_from_slice(&full[start..end]);
            Ok(())
        }
        Representation::BaseResidual {
            base,
            base_len,
            residual,
            ..
        } => {
            if *base_len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *base_len,
                    max: limits.max_alloc_bytes,
                });
            }
            let base_desc = ctx.fetch_descriptor(base)?;
            if base_desc.len() != *base_len {
                return Err(MaterializeError::InvalidDescriptor(
                    "base length mismatch".into(),
                ));
            }
            let mut base_bytes = vec![0u8; *base_len as usize];
            materialize(&base_desc, ctx, limits, depth + 1, budget, &mut base_bytes)?;
            apply_residual(residual, &base_bytes, output, ctx, limits, budget)
        }
        Representation::Sparse {
            k,
            rank,
            literals,
            len,
        } => {
            output.fill(0);
            let k = *k as usize;
            if k as u64 > *len {
                return Err(MaterializeError::InvalidDescriptor(
                    "sparse k exceeds length".into(),
                ));
            }
            let positions = crate::entropy::rank::unrank_comb_subset(*rank, *len, k as u64)
                .map_err(|e| MaterializeError::InvalidDescriptor(e.to_string()))?;
            if positions.len() != k || literals.len() != k {
                return Err(MaterializeError::InvalidDescriptor(
                    "sparse rank/literal mismatch".into(),
                ));
            }
            for (i, &pos) in positions.iter().enumerate() {
                output[pos as usize] = literals[i];
            }
            Ok(())
        }
        Representation::Palette {
            palette,
            counts,
            rank,
            len,
        } => {
            let symbols = crate::entropy::rank::unrank_multinomial(*rank, *len, counts)
                .map_err(|e| MaterializeError::InvalidDescriptor(e.to_string()))?;
            if symbols.len() != output.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "palette unrank length mismatch".into(),
                ));
            }
            for (i, &s) in symbols.iter().enumerate() {
                output[i] = palette[s as usize];
            }
            Ok(())
        }
        Representation::Periodic {
            period,
            pattern,
            count,
            tail,
            ..
        } => {
            let period = *period as usize;
            let count = *count as usize;
            if period == 0 || pattern.len() != period {
                return Err(MaterializeError::InvalidDescriptor(
                    "periodic pattern mismatch".into(),
                ));
            }
            let mut written = 0usize;
            for _ in 0..count {
                let end = written + period;
                if end > output.len() {
                    return Err(MaterializeError::InvalidDescriptor(
                        "periodic overflow".into(),
                    ));
                }
                output[written..end].copy_from_slice(pattern);
                written = end;
                spend(period as u64 / 8 + 1, budget)?;
            }
            if written + tail.len() != output.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "periodic tail mismatch".into(),
                ));
            }
            output[written..].copy_from_slice(tail);
            Ok(())
        }
        Representation::EntropyRef {
            universe,
            seed,
            coordinate,
            transform,
            residual,
            len,
        } => {
            if *transform != TransformId::Identity {
                return Err(MaterializeError::InvalidDescriptor(
                    "unsupported transform".into(),
                ));
            }
            let generated = ctx.universe_bytes(*universe, *seed, *coordinate, 0..*len)?;
            if generated.len() as u64 != *len {
                return Err(MaterializeError::Universe(
                    "universe length mismatch".into(),
                ));
            }
            // X = T(E) ⊕ R with T = identity ⇒ X = E ⊕ R.
            match residual {
                Residual::XorSparse { len: rlen, edits } => {
                    if *rlen != *len {
                        return Err(MaterializeError::Residual(
                            "entropy residual length mismatch".into(),
                        ));
                    }
                    output.copy_from_slice(&generated);
                    for e in edits {
                        if (e.pos as u64) >= *len {
                            return Err(MaterializeError::Residual(
                                "entropy residual edit out of range".into(),
                            ));
                        }
                        output[e.pos as usize] ^= e.val;
                    }
                    Ok(())
                }
                Residual::RangeReplace { .. } => Err(MaterializeError::Residual(
                    "range-replace residual not valid for entropy ref v1".into(),
                )),
                Residual::RansCoded { .. } => {
                    // X = E ⊕ D where D is the decoded residual stream.
                    let mut diff = vec![0u8; *len as usize];
                    apply_residual(residual, &generated, &mut diff, ctx, limits, budget)?;
                    // apply_residual computed X = E ⊕ D into `diff` already
                    // (base = generated, out = diff), so copy it over.
                    output.copy_from_slice(&diff);
                    Ok(())
                }
                Residual::BaseSequence { .. } => Err(MaterializeError::Residual(
                    "base-sequence residual not valid for entropy ref v1".into(),
                )),
            }
        }
        Representation::Permutation {
            rank,
            alphabet,
            len,
        } => {
            let m = *len as usize;
            if m == 0 || m > 34 {
                return Err(MaterializeError::InvalidDescriptor(
                    "permutation length out of range".into(),
                ));
            }
            if alphabet.len() != m {
                return Err(MaterializeError::InvalidDescriptor(
                    "permutation alphabet mismatch".into(),
                ));
            }
            let seq = crate::entropy::rank::unrank_permutation(*rank, m)
                .map_err(|e| MaterializeError::InvalidDescriptor(e.to_string()))?;
            // seq is the permutation of 0..m; map through the sorted
            // alphabet to recover the bytes.
            for (i, &idx) in seq.iter().enumerate() {
                output[i] = alphabet[idx as usize];
            }
            Ok(())
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
            if *len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *len,
                    max: limits.max_alloc_bytes,
                });
            }
            // Every command writes at least one byte, so the command count
            // can never exceed the output length (bounds the decode_rans
            // allocation below).
            if (*cmds as u64) > *len {
                return Err(MaterializeError::InvalidDescriptor(
                    "sequence command count exceeds output length".into(),
                ));
            }
            let d = crate::rans::sequence::decode_three_streams(
                ctx,
                limits,
                crate::rans::sequence::StreamRefs {
                    model: *model,
                    enc_obj: *enc_obj,
                    scale_bits: *scale_bits,
                    codec: *codec,
                },
                crate::rans::sequence::ThreeStreams {
                    seq_len: *seq_len,
                    lit_len: *lit_len,
                    off_len: *off_len,
                    cmds: *cmds,
                    lit_out: *lit_out,
                },
                None,
                2,
            )?;
            let (commands, literals, offsets) = (d.commands, d.literals, d.offsets);

            // Walk the commands (byte-progressive copy; overlap allowed).
            let mut pos = 0usize;
            let mut lit = 0usize;
            let mut off = 0usize;
            for &cmd in &commands {
                if cmd < 0x80 {
                    let run = cmd as usize + 1;
                    if pos + run > output.len() || lit + run > literals.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "literal run overflow".into(),
                        ));
                    }
                    output[pos..pos + run].copy_from_slice(&literals[lit..lit + run]);
                    pos += run;
                    lit += run;
                    spend(run as u64, budget)?;
                } else {
                    let clen = cmd as usize - 0x80 + 4;
                    if off + 2 > offsets.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "copy offset exhausted".into(),
                        ));
                    }
                    let dist = u16::from_le_bytes([offsets[off], offsets[off + 1]]) as usize;
                    off += 2;
                    if dist == 0 || dist > pos {
                        return Err(MaterializeError::InvalidDescriptor(
                            "copy distance out of range".into(),
                        ));
                    }
                    if pos + clen > output.len() {
                        return Err(MaterializeError::InvalidDescriptor("copy overflow".into()));
                    }
                    for _ in 0..clen {
                        output[pos] = output[pos - dist];
                        pos += 1;
                    }
                    spend(clen as u64, budget)?;
                }
            }
            if pos != output.len() || lit != literals.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "sequence command walk did not cover the output".into(),
                ));
            }
            Ok(())
        }
        Representation::SparseBlock64 {
            model,
            enc_obj,
            scale_bits,
            codec,
            pc_len,
            rank_len,
            lit_len,
            words,
            nonzero,
            lit_out,
            len,
        } => {
            if *len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *len,
                    max: limits.max_alloc_bytes,
                });
            }
            // Word coverage: words*8 >= len (validated structurally, but
            // re-checked here — materialize never trusts the descriptor).
            let word_count = *words as usize;
            if word_count.saturating_mul(8) < *len as usize {
                return Err(MaterializeError::InvalidDescriptor(
                    "sparse-block64 word count does not cover the output".into(),
                ));
            }
            // Popcount stream decodes to one byte per word; bound the
            // allocation (popcounts <= 64 fit one byte each).
            let d = crate::rans::sequence::decode_three_streams(
                ctx,
                limits,
                crate::rans::sequence::StreamRefs {
                    model: *model,
                    enc_obj: *enc_obj,
                    scale_bits: *scale_bits,
                    codec: *codec,
                },
                crate::rans::sequence::ThreeStreams {
                    seq_len: *pc_len,
                    lit_len: *lit_len,
                    off_len: *rank_len,
                    cmds: *words,
                    lit_out: *lit_out,
                },
                Some(*nonzero),
                8,
            )
            .map_err(|e| MaterializeError::Sequence(e.to_string()))?;
            let popcounts = d.commands;
            let literals = d.literals;
            let ranks = d.offsets;
            if popcounts.len() != word_count {
                return Err(MaterializeError::InvalidDescriptor(
                    "sparse-block64 popcount count mismatch".into(),
                ));
            }
            output.fill(0);
            let mut lit = 0usize;
            let mut rank = 0usize;
            for (w, &k) in popcounts.iter().enumerate() {
                let k = k as usize;
                if k == 0 {
                    continue;
                }
                if k > 64 || rank + 8 > ranks.len() || lit + k > literals.len() {
                    return Err(MaterializeError::InvalidDescriptor(
                        "sparse-block64 stream inconsistency".into(),
                    ));
                }
                let r = u64::from_le_bytes(
                    ranks[rank..rank + 8].try_into().expect("8-byte rank slice"),
                );
                rank += 8;
                // Unrank the C(64, k) subset of bit positions within the
                // word; each position maps to an output byte.
                let positions = crate::entropy::rank::unrank_comb_subset(r as u128, 64, k as u64)
                    .map_err(|e| MaterializeError::InvalidDescriptor(e.to_string()))?;
                if positions.len() != k {
                    return Err(MaterializeError::InvalidDescriptor(
                        "sparse-block64 rank length mismatch".into(),
                    ));
                }
                let base = w * 8;
                for (j, &p) in positions.iter().enumerate() {
                    let out_pos = base + p as usize;
                    if out_pos >= output.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "sparse-block64 position out of bounds".into(),
                        ));
                    }
                    output[out_pos] = literals[lit + j];
                }
                lit += k;
                spend(k as u64 + 1, budget)?;
            }
            if lit != literals.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "sparse-block64 literal count mismatch".into(),
                ));
            }
            Ok(())
        }
        Representation::SequenceDict {
            dictionary,
            dictionary_len,
            model,
            enc_obj,
            scale_bits,
            codec,
            seq_len,
            lit_len,
            off_len,
            src_len,
            cmds,
            lit_out,
            len,
        } => {
            if *len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *len,
                    max: limits.max_alloc_bytes,
                });
            }
            // Every command writes at least one byte, so the command count
            // can never exceed the output length (bounds the decode_rans
            // allocation below).
            if (*cmds as u64) > *len {
                return Err(MaterializeError::InvalidDescriptor(
                    "sequence command count exceeds output length".into(),
                ));
            }
            if *dictionary_len as u64 > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *dictionary_len as u64,
                    max: limits.max_alloc_bytes,
                });
            }
            // Resolve and materialize the dictionary chunk at depth+1: the
            // depth cap bounds dictionary chains, so cross-chunk references
            // can never defeat bounded random access (Phase-9B constraint).
            let dict_desc = ctx.fetch_descriptor(dictionary)?;
            if dict_desc.len() != *dictionary_len as u64 {
                return Err(MaterializeError::InvalidDescriptor(
                    "dictionary length mismatch".into(),
                ));
            }
            let mut dict_bytes = vec![0u8; *dictionary_len as usize];
            materialize(&dict_desc, ctx, limits, depth + 1, budget, &mut dict_bytes)?;
            let d = crate::rans::sequence::decode_four_streams(
                ctx,
                limits,
                crate::rans::sequence::StreamRefs {
                    model: *model,
                    enc_obj: *enc_obj,
                    scale_bits: *scale_bits,
                    codec: *codec,
                },
                crate::rans::sequence::FourStreams {
                    seq_len: *seq_len,
                    lit_len: *lit_len,
                    off_len: *off_len,
                    src_len: *src_len,
                    cmds: *cmds,
                    lit_out: *lit_out,
                },
            )?;
            let (commands, literals, offsets, sources) =
                (d.commands, d.literals, d.offsets, d.sources);
            // Walk the commands: LITERAL appends verbatim; COPY consumes
            // one source byte and one u16 value — LOCAL copies are
            // byte-progressive backward references into the output, DICT
            // copies are absolute offsets into the materialized dictionary.
            let mut pos = 0usize;
            let mut lit = 0usize;
            let mut off = 0usize;
            let mut src = 0usize;
            for &cmd in &commands {
                if cmd < 0x80 {
                    let run = cmd as usize + 1;
                    if pos + run > output.len() || lit + run > literals.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "literal run overflow".into(),
                        ));
                    }
                    output[pos..pos + run].copy_from_slice(&literals[lit..lit + run]);
                    pos += run;
                    lit += run;
                    spend(run as u64, budget)?;
                } else {
                    let clen = cmd as usize - 0x80 + 4;
                    if src + 1 > sources.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "copy source exhausted".into(),
                        ));
                    }
                    let source = sources[src];
                    src += 1;
                    if off + 2 > offsets.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "copy offset exhausted".into(),
                        ));
                    }
                    let v = u16::from_le_bytes([offsets[off], offsets[off + 1]]) as usize;
                    off += 2;
                    match source {
                        crate::rans::sequence::SRC_LOCAL => {
                            if v == 0 || v > pos {
                                return Err(MaterializeError::InvalidDescriptor(
                                    "copy distance out of range".into(),
                                ));
                            }
                            if pos + clen > output.len() {
                                return Err(MaterializeError::InvalidDescriptor(
                                    "copy overflow".into(),
                                ));
                            }
                            for _ in 0..clen {
                                output[pos] = output[pos - v];
                                pos += 1;
                            }
                        }
                        crate::rans::sequence::SRC_DICT => {
                            if v.checked_add(clen).is_none() || v + clen > dict_bytes.len() {
                                return Err(MaterializeError::InvalidDescriptor(
                                    "dict copy out of dictionary bounds".into(),
                                ));
                            }
                            if pos + clen > output.len() {
                                return Err(MaterializeError::InvalidDescriptor(
                                    "copy overflow".into(),
                                ));
                            }
                            output[pos..pos + clen].copy_from_slice(&dict_bytes[v..v + clen]);
                            pos += clen;
                        }
                        other => {
                            return Err(MaterializeError::InvalidDescriptor(format!(
                                "unknown copy source {other}"
                            )));
                        }
                    }
                    spend(clen as u64, budget)?;
                }
            }
            if pos != output.len() || lit != literals.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "sequence command walk did not cover the output".into(),
                ));
            }
            Ok(())
        }
    }
}

/// Apply a residual over `base` into `out`.
///
/// For `XorSparse` and `RangeReplace` this is pure algebra; for
/// `RansCoded` it fetches and decodes the residual stream.
pub fn apply_residual(
    residual: &Residual,
    base: &[u8],
    out: &mut [u8],
    ctx: &dyn DecoderContext,
    limits: &Limits,
    budget: &mut u64,
) -> Result<(), MaterializeError> {
    let len = residual.len();
    if out.len() as u64 != len {
        return Err(MaterializeError::Residual(
            "residual output length mismatch".into(),
        ));
    }
    // Positional residuals overlay the base and need base >= len; the
    // BaseSequence copy/literal delta validates its own copy bounds.
    if !matches!(residual, Residual::BaseSequence { .. }) && (base.len() as u64) < len {
        return Err(MaterializeError::Residual(
            "base shorter than residual".into(),
        ));
    }
    match residual {
        Residual::XorSparse { edits, .. } => {
            out[..len as usize].copy_from_slice(&base[..len as usize]);
            for e in edits {
                if (e.pos as u64) >= len {
                    return Err(MaterializeError::Residual("edit out of range".into()));
                }
                out[e.pos as usize] ^= e.val;
                spend(1, budget)?;
            }
            Ok(())
        }
        Residual::RangeReplace {
            changes, literals, ..
        } => {
            out[..len as usize].copy_from_slice(&base[..len as usize]);
            let mut lit = 0usize;
            for c in changes {
                let start = c.start as usize;
                let end = c.end as usize;
                if end > len as usize || start >= end {
                    return Err(MaterializeError::Residual("range out of bounds".into()));
                }
                let take = end - start;
                if lit + take > literals.len() {
                    return Err(MaterializeError::Residual("literal exhaustion".into()));
                }
                out[start..end].copy_from_slice(&literals[lit..lit + take]);
                lit += take;
                spend(take as u64, budget)?;
            }
            Ok(())
        }
        Residual::RansCoded {
            enc_obj,
            model,
            scale_bits,
            codec,
            decoded_len,
            ..
        } => {
            if *decoded_len != len {
                return Err(MaterializeError::Residual(
                    "residual decoded length mismatch".into(),
                ));
            }
            if *decoded_len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *decoded_len,
                    max: limits.max_alloc_bytes,
                });
            }
            let model_bytes = ctx.fetch_object(model)?;
            if model_bytes.len() as u64 > limits.max_model_bytes {
                return Err(MaterializeError::InvalidDescriptor(
                    "residual model object too large".into(),
                ));
            }
            let encoded = ctx.fetch_object(enc_obj)?;
            let decoded = ctx.decode_rans(&model_bytes, &encoded, *scale_bits, *codec, len)?;
            if decoded.len() as u64 != len {
                return Err(MaterializeError::Residual(
                    "residual rans length mismatch".into(),
                ));
            }
            for i in 0..len as usize {
                out[i] = base[i] ^ decoded[i];
            }
            Ok(())
        }
        Residual::BaseSequence {
            len,
            enc_obj,
            model,
            scale_bits,
            codec,
            seq_len,
            lit_len,
            off_len,
            cmds,
            lit_out,
            ..
        } => {
            if *len != out.len() as u64 {
                return Err(MaterializeError::Residual(
                    "base-sequence residual output length mismatch".into(),
                ));
            }
            // Every command writes at least one byte, so the command count
            // cannot exceed the output length (bounds the decode_rans
            // allocation).
            if (*cmds as u64) > *len {
                return Err(MaterializeError::Residual(
                    "base-sequence command count exceeds output length".into(),
                ));
            }
            let d = crate::rans::sequence::decode_three_streams(
                ctx,
                limits,
                crate::rans::sequence::StreamRefs {
                    model: *model,
                    enc_obj: *enc_obj,
                    scale_bits: *scale_bits,
                    codec: *codec,
                },
                crate::rans::sequence::ThreeStreams {
                    seq_len: *seq_len,
                    lit_len: *lit_len,
                    off_len: *off_len,
                    cmds: *cmds,
                    lit_out: *lit_out,
                },
                None,
                4,
            )
            .map_err(|e| MaterializeError::Residual(e.to_string()))?;
            let (commands, literals, offsets) = (d.commands, d.literals, d.offsets);
            // Walk the commands: LITERAL appends; COPY reads the base at a
            // u32 LE base offset (the output is built from scratch — the
            // residual is the full copy/literal recipe, not a diff).
            let mut pos = 0usize;
            let mut lit = 0usize;
            let mut off = 0usize;
            for &cmd in &commands {
                if cmd < 0x80 {
                    let run = cmd as usize + 1;
                    if pos + run > out.len() || lit + run > literals.len() {
                        return Err(MaterializeError::Residual(
                            "base-sequence literal run overflow".into(),
                        ));
                    }
                    out[pos..pos + run].copy_from_slice(&literals[lit..lit + run]);
                    pos += run;
                    lit += run;
                    spend(run as u64, budget)?;
                } else {
                    let clen = cmd as usize - 0x80 + 4;
                    if off + 4 > offsets.len() {
                        return Err(MaterializeError::Residual(
                            "base-sequence copy offset exhausted".into(),
                        ));
                    }
                    let boff = u32::from_le_bytes(
                        offsets[off..off + 4]
                            .try_into()
                            .expect("4-byte offset slice"),
                    ) as usize;
                    off += 4;
                    if boff.checked_add(clen).is_none() || boff + clen > base.len() {
                        return Err(MaterializeError::Residual(
                            "base-sequence copy out of base bounds".into(),
                        ));
                    }
                    if pos + clen > out.len() {
                        return Err(MaterializeError::Residual(
                            "base-sequence copy overflow".into(),
                        ));
                    }
                    out[pos..pos + clen].copy_from_slice(&base[boff..boff + clen]);
                    pos += clen;
                    spend(clen as u64, budget)?;
                }
            }
            if pos != out.len() {
                return Err(MaterializeError::Residual(
                    "base-sequence command walk did not cover the output".into(),
                ));
            }
            Ok(())
        }
    }
}

/// Decrement the work budget by `n`; error when exhausted.
fn spend(n: u64, budget: &mut u64) -> Result<(), MaterializeError> {
    if *budget < n {
        return Err(MaterializeError::BudgetExceeded);
    }
    *budget -= n;
    Ok(())
}

/// Convenience: materialize into a fresh `Vec<u8>`.
pub fn materialize_to_vec(
    desc: &Representation,
    ctx: &dyn DecoderContext,
    limits: &Limits,
) -> Result<Vec<u8>, MaterializeError> {
    let len = desc.len();
    if len > limits.max_alloc_bytes {
        return Err(MaterializeError::AllocTooLarge {
            requested: len,
            max: limits.max_alloc_bytes,
        });
    }
    let mut out = vec![0u8; len as usize];
    let mut budget = limits.max_decode_work;
    materialize(desc, ctx, limits, 0, &mut budget, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::representation::{Edit, Residual};
    use std::collections::HashMap;

    /// Minimal in-memory decoder context for tests.
    struct MemCtx {
        objects: HashMap<ChunkId, Vec<u8>>,
        chunks: HashMap<ChunkId, Representation>,
    }

    impl DecoderContext for MemCtx {
        fn fetch_object(&self, id: &ChunkId) -> Result<Vec<u8>, MaterializeError> {
            self.objects
                .get(id)
                .cloned()
                .ok_or(MaterializeError::MissingObject(*id))
        }
        fn fetch_descriptor(&self, id: &ChunkId) -> Result<Representation, MaterializeError> {
            self.chunks
                .get(id)
                .cloned()
                .ok_or(MaterializeError::MissingChunk(*id))
        }
        fn decode_rans(
            &self,
            _model: &[u8],
            _encoded: &[u8],
            _scale_bits: u8,
            _codec: RansCodec,
            _out_len: u64,
        ) -> Result<Vec<u8>, MaterializeError> {
            Err(MaterializeError::RansDecode(
                "not wired in unit test".into(),
            ))
        }
        fn universe_bytes(
            &self,
            _universe: UniverseId,
            _seed: [u8; 16],
            _coordinate: u64,
            range: Range<u64>,
        ) -> Result<Vec<u8>, MaterializeError> {
            Ok(vec![0xAB; (range.end - range.start) as usize])
        }
    }

    fn limits() -> Limits {
        Limits::default()
    }

    #[test]
    fn zero_and_fill() {
        let ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        let z = Representation::Zero { len: 1024 };
        assert_eq!(
            materialize_to_vec(&z, &ctx, &limits()).unwrap(),
            vec![0u8; 1024]
        );
        let f = Representation::Fill { value: 7, len: 512 };
        assert_eq!(
            materialize_to_vec(&f, &ctx, &limits()).unwrap(),
            vec![7u8; 512]
        );
    }

    #[test]
    fn inline_and_raw() {
        let data = b"hello entropy".to_vec();
        let id = ChunkId::of(&data);
        let mut ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        ctx.objects.insert(id, data.clone());
        let raw = Representation::Raw {
            obj: id,
            len: data.len() as u64,
        };
        assert_eq!(materialize_to_vec(&raw, &ctx, &limits()).unwrap(), data);

        let inl = Representation::Inline {
            data: b"abc".to_vec(),
        };
        assert_eq!(materialize_to_vec(&inl, &ctx, &limits()).unwrap(), b"abc");
    }

    #[test]
    fn exact_ref_subrange() {
        // target chunk: 256 bytes of 0x11
        let target = Representation::Fill {
            value: 0x11,
            len: 256,
        };
        let tid = ChunkId::of(&vec![0x11u8; 256]);
        let mut ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        ctx.chunks.insert(tid, target);
        let r = Representation::ExactRef {
            target: tid,
            off: 100,
            len: 10,
        };
        assert_eq!(
            materialize_to_vec(&r, &ctx, &limits()).unwrap(),
            vec![0x11u8; 10]
        );
    }

    #[test]
    fn base_residual_xor() {
        let base_bytes = vec![0u8; 64];
        let base_id = ChunkId::of(&base_bytes);
        let mut ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        ctx.chunks.insert(base_id, Representation::Zero { len: 64 });
        // X[i] = B[i] ^ val: bytes 0 and 63 differ
        let r = Representation::BaseResidual {
            base: base_id,
            base_len: 64,
            residual: Residual::XorSparse {
                len: 64,
                edits: vec![Edit { pos: 0, val: 0xFF }, Edit { pos: 63, val: 0x01 }],
            },
            len: 64,
        };
        let out = materialize_to_vec(&r, &ctx, &limits()).unwrap();
        assert_eq!(out[0], 0xFF);
        assert_eq!(out[63], 0x01);
        assert_eq!(out[1], 0x00);
    }

    #[test]
    fn range_replace_residual() {
        let base_bytes = vec![1u8; 32];
        let base_id = ChunkId::of(&base_bytes);
        let mut ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        ctx.chunks
            .insert(base_id, Representation::Fill { value: 1, len: 32 });
        let r = Representation::BaseResidual {
            base: base_id,
            base_len: 32,
            residual: Residual::RangeReplace {
                len: 32,
                changes: vec![
                    crate::core::representation::RangeChange { start: 4, end: 8 },
                    crate::core::representation::RangeChange { start: 16, end: 18 },
                ],
                literals: vec![9, 9, 9, 9, 7, 7],
            },
            len: 32,
        };
        let out = materialize_to_vec(&r, &ctx, &limits()).unwrap();
        assert_eq!(&out[0..4], &[1, 1, 1, 1]);
        assert_eq!(&out[4..8], &[9, 9, 9, 9]);
        assert_eq!(&out[8..16], &[1; 8]);
        assert_eq!(&out[16..18], &[7, 7]);
        assert_eq!(&out[18..], &[1; 14]);
    }

    #[test]
    fn depth_cap_enforced() {
        // A chain of exact refs longer than the depth cap must error.
        let mut ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        // Build a 6-long chain: c0 -> c1 -> ... -> c5
        let n = 6;
        for i in (0..n).rev() {
            let id = ChunkId::of(&[i as u8; 16]);
            let desc = if i == n - 1 {
                Representation::Fill {
                    value: 0x42,
                    len: 16,
                }
            } else {
                Representation::ExactRef {
                    target: ChunkId::of(&[(i + 1) as u8; 16]),
                    off: 0,
                    len: 16,
                }
            };
            ctx.chunks.insert(id, desc);
        }
        let top = Representation::ExactRef {
            target: ChunkId::of(&[0u8; 16]),
            off: 0,
            len: 16,
        };
        let limits = Limits {
            max_reference_depth: 4,
            ..Default::default()
        };
        let res = materialize_to_vec(&top, &ctx, &limits);
        assert!(matches!(res, Err(MaterializeError::DepthExceeded { .. })));
    }

    #[test]
    fn sparse_roundtrip_via_engine() {
        // Build a sparse chunk: 64 bytes, nonzeros at 2, 17, 55.
        let mut input = vec![0u8; 64];
        input[2] = 0xAA;
        input[17] = 0xBB;
        input[55] = 0xCC;
        let positions: Vec<u32> = input
            .iter()
            .enumerate()
            .filter(|(_, b)| **b != 0)
            .map(|(i, _)| i as u32)
            .collect();
        let rank = crate::entropy::rank::rank_comb_subset(&positions, 64).unwrap();
        let literals: Vec<u8> = input.iter().copied().filter(|&b| b != 0).collect();
        let desc = Representation::Sparse {
            k: 3,
            rank,
            literals,
            len: 64,
        };
        desc.validate(&limits()).unwrap();
        let ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        assert_eq!(materialize_to_vec(&desc, &ctx, &limits()).unwrap(), input);
    }

    #[test]
    fn work_budget_exceeded() {
        let ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        let z = Representation::Zero { len: 65536 };
        let mut budget = 1u64; // tiny budget
        let mut out = vec![0u8; 65536];
        let res = materialize(&z, &ctx, &limits(), 0, &mut budget, &mut out);
        assert_eq!(res, Err(MaterializeError::BudgetExceeded));
    }
}
