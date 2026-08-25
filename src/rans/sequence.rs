//! SequenceRans: the local-match + entropy compression floor (Phase-8
//! directive §4; ADR-0005).
//!
//! An LZ77-style hash-chain match finder turns a chunk into three byte
//! streams — *commands*, *literals*, *offsets* — each of which is either
//! rANS-coded with `ryg-rans-rs` or stored raw when that is cheaper. Pure
//! rANS is an entropy coder, not a match finder; this family supplies the
//! sequence matching that gives general-purpose compressors (zstd-class)
//! most of their power, keeping `ryg-rans-rs` as the entropy backend.
//!
//! Command encoding (one byte per command):
//!
//! - `0x00..=0x7F`: literal run of `b + 1` (1..=128) bytes.
//! - `0x80..=0xFF`: copy of length `b - 0x80 + 4` (4..=131); the offset
//!   (u16 LE, relative to the current output position) follows in the
//!   offset stream.
//!
//! Copy semantics are byte-progressive (overlap allowed): `out[p+i] =
//! out[p+i-d]` for `i in 0..len` — the standard LZ77 contract that makes
//! RLE and arbitrarily long matches representable by repeated copies at
//! one distance. The only validity constraint is `d <= p`.
//!
//! Model object layout (three slots, one per stream):
//!
//! ```text
//! slot: [kind u8][len u16 LE][bytes]
//!   kind 0x00 = rANS model   (bytes = encode_model output)
//!   kind 0x01 = raw stream   (no bytes in the slot; the raw stream lives
//!                             in the enc object)
//!   kind 0x02 = empty stream (len must be 0)
//! ```
//!
//! The enc object is the three streams concatenated; `seq_len + lit_len +
//! off_len` (descriptor fields) must equal the enc object length. A stream
//! is rANS-encoded or raw per its slot. Everything is content-addressed
//! and counted in the candidate's persisted byte total — the raw fallback
//! inside the family never hides bytes.

#![forbid(unsafe_code)]

use crate::core::candidate::{Candidate, CandidateContext, Encoder, ObjectRecord};
use crate::core::cost::ByteSplit;
use crate::core::representation::{RansCodec, Representation};
use crate::rans::metadata;
use crate::rans::model::{RansModel, normalize_histogram};
use crate::rans::residual::encode_stream;

/// Minimum match length (a 4-byte copy costs 3 bytes pre-entropy: command
/// byte + u16 offset; 4 literal bytes cost 4).
pub const MIN_MATCH: usize = 4;
/// Maximum copy length per command (`0xFF - 0x80 + 4`).
pub const MAX_COPY: usize = 131;
/// Maximum literal run per command (`0x7F + 1`).
pub const MAX_LIT_RUN: usize = 128;
/// Maximum copy distance (u16 LE offset).
pub const MAX_DIST: usize = 65535;
/// Hash-chain depth cap (deterministic, bounded match search).
const CHAIN_DEPTH: usize = 16;
/// Scale bits shared by the three rANS models.
const SCALE_BITS: u8 = 14;
/// Codec shared by the three streams.
const CODEC: RansCodec = RansCodec::Interleaved2;

/// Model-object slot kinds.
const SLOT_RANS: u8 = 0x00;
const SLOT_RAW: u8 = 0x01;
const SLOT_EMPTY: u8 = 0x02;

/// The three raw streams before entropy coding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceStreams {
    /// One command byte per command.
    pub commands: Vec<u8>,
    /// Literal-run bytes in command order.
    pub literals: Vec<u8>,
    /// One u16 LE offset per copy command.
    pub offsets: Vec<u8>,
}

/// The fully encoded family: model object, enc object, and the descriptor
/// stream lengths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedSequence {
    /// Model object payload (three slots).
    pub model_obj: Vec<u8>,
    /// Enc object payload (three concatenated streams).
    pub enc_obj: Vec<u8>,
    /// Encoded command-stream length.
    pub seq_len: u32,
    /// Encoded literal-stream length.
    pub lit_len: u32,
    /// Encoded offset-stream length.
    pub off_len: u32,
    /// Decoded command count.
    pub cmds: u32,
    /// Decoded literal byte count.
    pub lit_out: u32,
}

/// Sequence stream errors (typed; never panic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequenceError {
    /// Model object exceeds the format bound.
    TooLarge {
        /// Declared length.
        len: u64,
        /// Format bound.
        max: u64,
    },
    /// Truncated model object.
    Truncated,
    /// Unknown slot kind.
    UnknownKind(u8),
    /// Raw/empty slot with a nonzero length.
    Malformed,
    /// Trailing bytes after the third slot.
    TrailingBytes,
    /// rANS model encode/decode failure.
    Rans(String),
    /// rANS stream encode failure.
    Stream(String),
    /// The streams cannot represent a non-empty chunk.
    NoCommands,
}

impl std::fmt::Display for SequenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for SequenceError {}

/// The raw LZ77 streams for `input` (greedy, hash-chain matcher).
///
/// Deterministic and bounded: chain depth capped, offsets capped at 16
/// bits, every loop length-bounded by `input.len()`.
pub fn encode_sequence(input: &[u8]) -> SequenceStreams {
    let n = input.len();
    let mut commands = Vec::new();
    let mut literals = Vec::new();
    let mut offsets = Vec::new();
    if n == 0 {
        return SequenceStreams {
            commands,
            literals,
            offsets,
        };
    }
    let hsize = 1usize << 16;
    let mut head = vec![u32::MAX; hsize];
    let mut chain = vec![u32::MAX; n];
    let mut pos = 0usize;
    while pos < n {
        // A match starting at pos?
        if pos + MIN_MATCH <= n {
            if let Some((dist, len)) = find_match(input, pos, &head, &chain) {
                // A copy command encodes 4..=131 bytes; clip the match so
                // the tail remainder after 131-byte chunks never lands in
                // 1..=3 (that would encode as `0x80 + 3 - 4 = 0x7F`, which
                // the decoder reads as a 128-byte literal run — a corrupt
                // stream). The clipped tail is emitted as literals by the
                // next iteration; byte-exactness is preserved.
                let mut len = len;
                let rem = len % MAX_COPY;
                if rem > 0 && rem < MIN_MATCH {
                    len -= rem;
                }
                // Emit copy command(s); a long match continues at the same
                // distance (byte-progressive copy makes this exact).
                let mut remaining = len;
                while remaining > 0 {
                    let take = remaining.min(MAX_COPY);
                    debug_assert!((MIN_MATCH..=MAX_COPY).contains(&take));
                    commands.push((0x80 + take - MIN_MATCH) as u8);
                    offsets.extend_from_slice(&(dist as u16).to_le_bytes());
                    remaining -= take;
                }
                // Hash every covered position for future matches.
                let end = pos + len;
                while pos < end {
                    if pos + MIN_MATCH <= n {
                        let h = hash_at(input, pos);
                        chain[pos] = head[h];
                        head[h] = pos as u32;
                    }
                    pos += 1;
                }
                continue;
            }
        }
        // Literal run: consume positions with no match, capped at 128.
        let start = pos;
        let mut run = 0usize;
        while pos < n && run < MAX_LIT_RUN {
            let has_match = pos + MIN_MATCH <= n && find_match(input, pos, &head, &chain).is_some();
            if has_match {
                break;
            }
            if pos + MIN_MATCH <= n {
                let h = hash_at(input, pos);
                chain[pos] = head[h];
                head[h] = pos as u32;
            }
            pos += 1;
            run += 1;
        }
        if run > 0 {
            commands.push((run - 1) as u8);
            literals.extend_from_slice(&input[start..pos]);
        }
        // run == 0 means a match exists at pos; the loop top handles it.
    }
    SequenceStreams {
        commands,
        literals,
        offsets,
    }
}

/// Find the longest match at `pos`, capped by `CHAIN_DEPTH` chain walks.
/// Returns `(dist, len)` with `len >= MIN_MATCH`. Deterministic tie-break:
/// longer wins; equal lengths keep the most recent candidate (first in the
/// chain).
fn find_match(input: &[u8], pos: usize, head: &[u32], chain: &[u32]) -> Option<(usize, usize)> {
    let n = input.len();
    let h = hash_at(input, pos);
    let mut c = head[h];
    let max_len = n - pos;
    let mut best_len = 0usize;
    let mut best_dist = 0usize;
    let mut depth = 0usize;
    while c != u32::MAX && depth < CHAIN_DEPTH {
        let cpos = c as usize;
        let dist = pos - cpos;
        if dist <= MAX_DIST {
            let mut l = 0usize;
            while l < max_len && input[cpos + l] == input[pos + l] {
                l += 1;
            }
            if l >= MIN_MATCH && l > best_len {
                best_len = l;
                best_dist = dist;
                if l == max_len {
                    break;
                }
            }
        }
        c = chain[cpos];
        depth += 1;
    }
    if best_len >= MIN_MATCH {
        Some((best_dist, best_len))
    } else {
        None
    }
}

/// Hash of the 4 bytes at `pos`: a 16-bit key.
pub(crate) fn hash_at(input: &[u8], pos: usize) -> usize {
    let h = u32::from_le_bytes(input[pos..pos + 4].try_into().expect("4-byte slice"));
    (h.wrapping_mul(0x9E37_79B1) >> 16) as usize
}

/// Which encoding a stream uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamSlot {
    /// rANS-coded; holds the encoded model bytes.
    Rans(Vec<u8>),
    /// Stored raw in the enc object.
    Raw,
    /// Empty stream (decoded length must be 0).
    Empty,
}

/// Encode the three raw streams: per-stream histogram, degenerate streams
/// stored raw, rANS where it wins. Returns `None` when the chunk cannot be
/// represented (empty command stream).
pub fn encode_streams(streams: &SequenceStreams) -> Option<EncodedSequence> {
    if streams.commands.is_empty() {
        return None;
    }
    let mut model_obj = Vec::with_capacity(3 * 3 + 3 * 512);
    let mut enc_obj = Vec::new();
    let mut lens = [0u32; 3];
    for (i, stream) in [&streams.commands, &streams.literals, &streams.offsets]
        .iter()
        .enumerate()
    {
        let (slot, payload) = encode_one_stream(stream)?;
        match &slot {
            StreamSlot::Rans(model_bytes) => {
                model_obj.push(SLOT_RANS);
                model_obj.extend_from_slice(&(model_bytes.len() as u16).to_le_bytes());
                model_obj.extend_from_slice(model_bytes);
            }
            StreamSlot::Raw => {
                model_obj.push(SLOT_RAW);
                model_obj.extend_from_slice(&0u16.to_le_bytes());
            }
            StreamSlot::Empty => {
                model_obj.push(SLOT_EMPTY);
                model_obj.extend_from_slice(&0u16.to_le_bytes());
            }
        }
        lens[i] = payload.len() as u32;
        enc_obj.extend_from_slice(&payload);
    }
    Some(EncodedSequence {
        model_obj,
        enc_obj,
        seq_len: lens[0],
        lit_len: lens[1],
        off_len: lens[2],
        cmds: streams.commands.len() as u32,
        lit_out: streams.literals.len() as u32,
    })
}

/// Encode one stream: histogram decides Empty / Raw / rANS (rANS only when
/// strictly smaller than the raw stream). Returns the slot and the stored
/// stream payload (raw bytes or the rANS encoding).
fn encode_one_stream(stream: &[u8]) -> Option<(StreamSlot, Vec<u8>)> {
    let mut hist = [0u32; 256];
    for &b in stream {
        hist[b as usize] += 1;
    }
    let distinct = hist.iter().filter(|&&h| h > 0).count();
    match distinct {
        0 => Some((StreamSlot::Empty, Vec::new())),
        1 => Some((StreamSlot::Raw, stream.to_vec())),
        _ => {
            let model: RansModel = normalize_histogram(&hist, SCALE_BITS, CODEC)?;
            match encode_stream(stream, &model) {
                Ok(enc) if enc.len() < stream.len() => {
                    Some((StreamSlot::Rans(metadata::encode_model(&model)), enc))
                }
                _ => Some((StreamSlot::Raw, stream.to_vec())),
            }
        }
    }
}

/// Parse the three-slot model object. `max_bytes` bounds the input; the
/// per-slot model payloads are additionally capped by the format's
/// per-model bound.
pub fn parse_model_object(bytes: &[u8], max_bytes: u64) -> Result<[StreamSlot; 3], SequenceError> {
    if bytes.len() as u64 > max_bytes {
        return Err(SequenceError::TooLarge {
            len: bytes.len() as u64,
            max: max_bytes,
        });
    }
    let mut out = [StreamSlot::Empty, StreamSlot::Empty, StreamSlot::Empty];
    let mut pos = 0usize;
    for slot in out.iter_mut() {
        let kind = *bytes.get(pos).ok_or(SequenceError::Truncated)?;
        pos += 1;
        let len = u16::from_le_bytes(
            bytes
                .get(pos..pos + 2)
                .ok_or(SequenceError::Truncated)?
                .try_into()
                .expect("2-byte slice"),
        ) as usize;
        pos += 2;
        match kind {
            SLOT_RANS => {
                let b = bytes.get(pos..pos + len).ok_or(SequenceError::Truncated)?;
                *slot = StreamSlot::Rans(b.to_vec());
            }
            SLOT_RAW => {
                if len != 0 {
                    return Err(SequenceError::Malformed);
                }
                *slot = StreamSlot::Raw;
            }
            SLOT_EMPTY => {
                if len != 0 {
                    return Err(SequenceError::Malformed);
                }
            }
            other => return Err(SequenceError::UnknownKind(other)),
        }
        pos += len;
    }
    if pos != bytes.len() {
        return Err(SequenceError::TrailingBytes);
    }
    Ok(out)
}

/// The descriptor stream-length fields shared by SEQUENCE_RANS, the
/// BASE_SEQUENCE residual, and SPARSE_BLOCK64.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreeStreams {
    /// Encoded command-stream length.
    pub seq_len: u32,
    /// Encoded literal-stream length.
    pub lit_len: u32,
    /// Encoded offset-stream length.
    pub off_len: u32,
    /// Decoded command count.
    pub cmds: u32,
    /// Decoded literal byte count.
    pub lit_out: u32,
}

/// The three decoded streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedStreams {
    /// Decoded command bytes.
    pub commands: Vec<u8>,
    /// Decoded literal bytes.
    pub literals: Vec<u8>,
    /// Decoded offset bytes.
    pub offsets: Vec<u8>,
}

/// The object references shared by the three-stream families.
#[derive(Debug, Clone, Copy)]
pub struct StreamRefs {
    /// Content id of the model object.
    pub model: crate::core::extent::ChunkId,
    /// Content id of the enc object.
    pub enc_obj: crate::core::extent::ChunkId,
    /// Model scale bits.
    pub scale_bits: u8,
    /// Codec.
    pub codec: RansCodec,
}

/// Decode the three streams (commands, literals, offsets) from the model
/// and enc objects, validating every length. Shared by the SEQUENCE_RANS
/// / BASE_SEQUENCE materialize paths and the SPARSE_BLOCK64 arm.
///
/// `units` is the number of offset-stream entries: `None` derives it from
/// the command stream (count of copy commands — the SequenceRans/Base-
/// Sequence convention), `Some(n)` uses the caller's known count (the
/// SPARSE_BLOCK64 nonzero-word count, a descriptor field). The decoded
/// offset stream length must be exactly `units × off_per_copy`.
pub fn decode_three_streams(
    ctx: &dyn crate::core::materialize::DecoderContext,
    limits: &crate::core::limits::Limits,
    refs: StreamRefs,
    lens: ThreeStreams,
    units: Option<u32>,
    off_per_copy: u32,
) -> Result<DecodedStreams, crate::core::materialize::MaterializeError> {
    use crate::core::materialize::MaterializeError;
    let StreamRefs {
        model,
        enc_obj,
        scale_bits,
        codec,
    } = refs;
    let ThreeStreams {
        seq_len,
        lit_len,
        off_len,
        cmds,
        lit_out,
    } = lens;
    // Stream lengths must compose exactly to the enc object.
    let enc_total = (seq_len as u64)
        .checked_add(lit_len as u64)
        .and_then(|v| v.checked_add(off_len as u64))
        .ok_or(MaterializeError::InvalidDescriptor(
            "stream lengths overflow".into(),
        ))?;
    if enc_total > limits.max_alloc_bytes {
        return Err(MaterializeError::AllocTooLarge {
            requested: enc_total,
            max: limits.max_alloc_bytes,
        });
    }
    let model_bytes = ctx.fetch_object(&model)?;
    let slots = parse_model_object(&model_bytes, max_model_object_bytes(limits.max_model_bytes))
        .map_err(|e| MaterializeError::Sequence(e.to_string()))?;
    let enc = ctx.fetch_object(&enc_obj)?;
    if enc.len() as u64 != enc_total {
        return Err(MaterializeError::InvalidDescriptor(
            "enc object length mismatch".into(),
        ));
    }
    let seq_slice = &enc[..seq_len as usize];
    let lit_slice = &enc[seq_len as usize..seq_len as usize + lit_len as usize];
    let off_slice = &enc[seq_len as usize + lit_len as usize..];

    // Commands.
    let commands: Vec<u8> = match &slots[0] {
        StreamSlot::Rans(m) => ctx.decode_rans(m, seq_slice, scale_bits, codec, cmds as u64)?,
        StreamSlot::Raw => {
            if seq_len != cmds {
                return Err(MaterializeError::InvalidDescriptor(
                    "raw command stream length mismatch".into(),
                ));
            }
            seq_slice.to_vec()
        }
        StreamSlot::Empty => {
            return Err(MaterializeError::InvalidDescriptor(
                "empty command stream".into(),
            ));
        }
    };
    if commands.len() as u64 != cmds as u64 {
        return Err(MaterializeError::InvalidDescriptor(
            "command stream decoded length mismatch".into(),
        ));
    }
    let copies = match units {
        Some(u) => u as usize,
        None => commands.iter().filter(|&&b| b >= 0x80).count(),
    };
    let off_out = (copies as u64).checked_mul(off_per_copy as u64).ok_or(
        MaterializeError::InvalidDescriptor("offset stream length overflow".into()),
    )?;
    if off_out > limits.max_alloc_bytes {
        return Err(MaterializeError::AllocTooLarge {
            requested: off_out,
            max: limits.max_alloc_bytes,
        });
    }
    // Literals.
    let literals: Vec<u8> = match &slots[1] {
        StreamSlot::Rans(m) => ctx.decode_rans(m, lit_slice, scale_bits, codec, lit_out as u64)?,
        StreamSlot::Raw => {
            if lit_len != lit_out {
                return Err(MaterializeError::InvalidDescriptor(
                    "raw literal stream length mismatch".into(),
                ));
            }
            lit_slice.to_vec()
        }
        StreamSlot::Empty => {
            if lit_out != 0 || lit_len != 0 {
                return Err(MaterializeError::InvalidDescriptor(
                    "non-empty literal stream without a model".into(),
                ));
            }
            Vec::new()
        }
    };
    if literals.len() as u64 != lit_out as u64 {
        return Err(MaterializeError::InvalidDescriptor(
            "literal stream decoded length mismatch".into(),
        ));
    }
    // Offsets.
    let offsets: Vec<u8> = match &slots[2] {
        StreamSlot::Rans(m) => ctx.decode_rans(m, off_slice, scale_bits, codec, off_out)?,
        StreamSlot::Raw => {
            if off_len as u64 != off_out {
                return Err(MaterializeError::InvalidDescriptor(
                    "raw offset stream length mismatch".into(),
                ));
            }
            off_slice.to_vec()
        }
        StreamSlot::Empty => {
            if off_out != 0 {
                return Err(MaterializeError::InvalidDescriptor(
                    "non-empty offset stream without a model".into(),
                ));
            }
            Vec::new()
        }
    };
    if offsets.len() as u64 != off_out {
        return Err(MaterializeError::InvalidDescriptor(
            "offset stream decoded length mismatch".into(),
        ));
    }
    Ok(DecodedStreams {
        commands,
        literals,
        offsets,
    })
}

/// Max model-object size for one chunk: three per-stream models plus the
/// slot headers, bounded against the format's per-model cap.
pub const fn max_model_object_bytes(per_model: u64) -> u64 {
    per_model.saturating_mul(3).saturating_add(64)
}

/// The SequenceRans candidate family (foreground + background).
#[derive(Debug, Default)]
pub struct SequenceEncoder;

impl Encoder for SequenceEncoder {
    fn name(&self) -> &'static str {
        "SEQUENCE_RANS"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        if input.is_empty() || input.len() as u64 > ctx.limits.max_chunk_size {
            return Vec::new();
        }
        // LZ overhead (three models + three streams) cannot win on tiny
        // inputs; skip the CPU.
        if input.len() < 128 {
            return Vec::new();
        }
        let streams = encode_sequence(input);
        let enc = match encode_streams(&streams) {
            Some(e) => e,
            None => return Vec::new(),
        };
        let model_obj = ObjectRecord::model(enc.model_obj);
        let enc_obj = ObjectRecord::data(enc.enc_obj);
        let rep = Representation::SequenceRans {
            model: model_obj.id,
            enc_obj: enc_obj.id,
            scale_bits: SCALE_BITS,
            codec: CODEC,
            seq_len: enc.seq_len,
            lit_len: enc.lit_len,
            off_len: enc.off_len,
            cmds: enc.cmds,
            lit_out: enc.lit_out,
            len: input.len() as u64,
        };
        // Honest gate: descriptor + model object + enc object must beat
        // the raw bytes, else RAW/RANS wins on cost anyway (§15).
        let total = rep
            .encoded_size()
            .saturating_add(model_obj.payload.len() as u64)
            .saturating_add(enc_obj.payload.len() as u64);
        if total >= input.len() as u64 {
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

    fn text_chunk() -> Vec<u8> {
        // English-ish text with long-distance repeats (a sentence repeated
        // many times, lightly edited) — the case SequenceRans exists for.
        let sentence =
            b"the quick brown fox jumps over the lazy dog and then walks back to the riverbed ";
        let mut out = Vec::new();
        for i in 0..40 {
            out.extend_from_slice(sentence);
            out.extend_from_slice(format!("sentence number {i} has a unique tail ").as_bytes());
        }
        out
    }

    /// Deterministic byte-uniform PRNG with no 4-byte repeats (SplitMix64).
    fn noise(n: usize) -> Vec<u8> {
        let mut state: u64 = 0x243F_6A88_85A3_08D3;
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            let b = z.to_le_bytes();
            let take = (n - out.len()).min(8);
            out.extend_from_slice(&b[..take]);
        }
        out
    }

    #[test]
    fn rle_is_all_copies_after_prefix() {
        let input = vec![b'a'; 4096];
        let streams = encode_sequence(&input);
        // One literal byte (no prior output to reference), then copies at
        // distance 1 covering the rest.
        assert_eq!(streams.literals, b"a");
        assert_eq!(streams.commands[0], 0x00);
        assert!(streams.commands[1..].iter().all(|&b| b >= 0x80));
        assert_eq!(streams.offsets.len() / 2, streams.commands.len() - 1);
        assert!(
            streams
                .offsets
                .chunks_exact(2)
                .all(|o| u16::from_le_bytes([o[0], o[1]]) == 1)
        );
    }

    #[test]
    fn literal_only_input() {
        let input = noise(4096);
        let streams = encode_sequence(&input);
        assert!(streams.offsets.is_empty());
        assert!(streams.commands.iter().all(|&b| b < 0x80));
        assert_eq!(streams.literals.len(), input.len());
    }

    #[test]
    fn long_match_continues_at_same_distance() {
        // 600 bytes of a repeated 200-byte pattern: the tail match exceeds
        // MAX_COPY, forcing continuation commands at the same distance.
        let mut input = Vec::new();
        let pattern: Vec<u8> = (0..200u32).map(|i| i as u8).collect();
        for _ in 0..3 {
            input.extend_from_slice(&pattern);
        }
        let streams = encode_sequence(&input);
        let copies: Vec<u16> = streams
            .offsets
            .chunks_exact(2)
            .map(|o| u16::from_le_bytes([o[0], o[1]]))
            .collect();
        // 400 copy bytes / 131 per command => 4 continuation commands
        assert!(copies.len() >= 2);
        assert!(copies.iter().all(|&d| d == 200));
        let copy_len: usize = streams
            .commands
            .iter()
            .filter(|&&b| b >= 0x80)
            .map(|&b| b as usize - 0x80 + MIN_MATCH)
            .sum();
        assert_eq!(copy_len, 400);
        assert_eq!(copy_len + streams.literals.len(), input.len());
    }

    #[test]
    fn model_object_roundtrip() {
        let streams = encode_sequence(&text_chunk());
        let enc = encode_streams(&streams).unwrap();
        let parsed = parse_model_object(&enc.model_obj, 4096).unwrap();
        assert_eq!(parsed.len(), 3);
        // commands must have a model or be raw; never empty for text
        assert!(!matches!(parsed[0], StreamSlot::Empty));
    }

    #[test]
    fn sequence_encoder_wins_on_text() {
        let limits = Limits::default();
        let policy = Policy::default();
        let input = text_chunk();
        let ctx = ctx_for(&input, &limits, &policy);
        let cands = SequenceEncoder.encode(&input, &ctx);
        assert_eq!(cands.len(), 1);
        let cand = &cands[0];
        let resolver = MemResolver::from_map(
            cand.objects
                .iter()
                .map(|o| (o.id, o.payload.clone()))
                .collect(),
        );
        validate_candidate(cand, &input, &resolver, &limits).unwrap();
        // Must beat rANS-only: a meaningful persisted saving.
        assert!(
            cand.cost.persisted_bytes() < input.len() as u64,
            "sequence rans persisted {} >= raw {}",
            cand.cost.persisted_bytes(),
            input.len()
        );
        // And must beat plain rANS on this corpus (the whole point of the
        // floor): plain RANS typically lands ~0.6-0.7 ratio on text.
        let rans = crate::rans::residual::RansEncoder.encode(&input, &ctx);
        let best_rans = rans
            .iter()
            .min_by_key(|c| c.total(&policy))
            .map(|c| c.cost.persisted_bytes())
            .unwrap_or(input.len() as u64);
        assert!(
            cand.cost.persisted_bytes() < best_rans,
            "sequence {} not better than plain rans {}",
            cand.cost.persisted_bytes(),
            best_rans
        );
    }

    #[test]
    fn sequence_skips_urandom() {
        let limits = Limits::default();
        let policy = Policy::default();
        let input = noise(65536);
        let cands = SequenceEncoder.encode(&input, &ctx_for(&input, &limits, &policy));
        assert!(
            cands.is_empty(),
            "urandom must not produce a sequence candidate"
        );
        // And RAW must remain the winner (payload + descriptor + crc).
        let raw = crate::core::candidate::raw_candidate(
            &input,
            crate::core::extent::ChunkId::of(&input),
            &limits,
        )
        .unwrap();
        assert_eq!(raw.cost.persisted_bytes(), input.len() as u64 + 41);
    }

    #[test]
    fn versioned_class2_mutated_roundtrips() {
        // The H2 versioned corpus, version 2, chunk class 2 (period-7
        // pattern with 84 XOR mutations): the SequenceRans encoder must
        // produce streams whose own walk reproduces the chunk exactly.
        let corpus = crate::evidence::corpus::versioned(1, 4);
        let chunk = &corpus.versions[2][2 * 65536..3 * 65536];
        let streams = encode_sequence(chunk);
        let mut lits = 0usize;
        let mut offs = 0usize;
        let mut out = Vec::with_capacity(chunk.len());
        for (cmd_no, &cmd) in streams.commands.iter().enumerate() {
            if cmd < 0x80 {
                let run = cmd as usize + 1;
                if lits + run > streams.literals.len() || out.len() + run > chunk.len() {
                    panic!(
                        "literal overflow at command {}: out {} run {} lits {} total {}",
                        cmd_no,
                        out.len(),
                        run,
                        lits,
                        streams.literals.len()
                    );
                }
                out.extend_from_slice(&streams.literals[lits..lits + run]);
                lits += run;
            } else {
                let clen = cmd as usize - 0x80 + 4;
                if offs + 2 > streams.offsets.len() {
                    panic!("offset exhausted at command {cmd_no}");
                }
                let d =
                    u16::from_le_bytes([streams.offsets[offs], streams.offsets[offs + 1]]) as usize;
                offs += 2;
                if d == 0 || d > out.len() {
                    panic!("bad dist {d} at command {cmd_no} (out {})", out.len());
                }
                for _ in 0..clen {
                    let b = out[out.len() - d];
                    out.push(b);
                }
            }
        }
        assert_eq!(out, chunk);
    }

    #[test]
    fn mulshift_data_roundtrips_exactly() {
        // The optimizer's old raw-fallback fixture (i*K)>>8 has genuine
        // distance-256 structure; prove the matcher's copies reconstruct
        // byte-exactly through a manual decoder walk.
        let data: Vec<u8> = (0..65536u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 8) as u8)
            .collect();
        let streams = encode_sequence(&data);
        let mut lits = 0usize;
        let mut offs = 0usize;
        let mut out = Vec::with_capacity(data.len());
        for &cmd in &streams.commands {
            if cmd < 0x80 {
                let run = cmd as usize + 1;
                out.extend_from_slice(&streams.literals[lits..lits + run]);
                lits += run;
            } else {
                let clen = cmd as usize - 0x80 + 4;
                let d =
                    u16::from_le_bytes([streams.offsets[offs], streams.offsets[offs + 1]]) as usize;
                offs += 2;
                assert!(d > 0 && d <= out.len());
                for _ in 0..clen {
                    let b = out[out.len() - d];
                    out.push(b);
                }
            }
        }
        assert_eq!(out, data);
        // And the family genuinely wins on this data (the LZ structure is
        // real, not an accounting artifact).
        let cands = SequenceEncoder.encode(
            &data,
            &ctx_for(&data, &Limits::default(), &Policy::default()),
        );
        assert_eq!(cands.len(), 1);
        let cand = &cands[0];
        assert!(
            cand.cost.persisted_bytes() < data.len() as u64 / 10,
            "persisted {}",
            cand.cost.persisted_bytes()
        );
    }

    #[test]
    fn degenerate_streams_fall_back_to_raw_slots() {
        // Noise: no matches => literal runs of 128 => a command stream with
        // a single distinct value => the raw slot path.
        let input = noise(4096);
        let streams = encode_sequence(&input);
        assert!(streams.offsets.is_empty());
        let enc = encode_streams(&streams).unwrap();
        let parsed = parse_model_object(&enc.model_obj, 4096).unwrap();
        // commands stream: runs of 128 => all 0x7F => raw slot
        assert_eq!(parsed[0], StreamSlot::Raw);
        // offsets: no copies => empty slot
        assert_eq!(parsed[2], StreamSlot::Empty);
        // …and the full family correctly declines the candidate (gate).
        let cands = SequenceEncoder.encode(
            &input,
            &ctx_for(&input, &Limits::default(), &Policy::default()),
        );
        assert!(cands.is_empty());
    }

    #[test]
    fn rle_slot_layout() {
        // RLE: one literal byte (raw slot), highly skewed command/offset
        // streams (rANS slots).
        let input = vec![b'a'; 65536];
        let streams = encode_sequence(&input);
        assert_eq!(streams.literals, b"a");
        let enc = encode_streams(&streams).unwrap();
        let parsed = parse_model_object(&enc.model_obj, 4096).unwrap();
        assert!(matches!(parsed[0], StreamSlot::Rans(_)));
        assert_eq!(parsed[1], StreamSlot::Raw);
        assert!(matches!(parsed[2], StreamSlot::Rans(_)));
        // Round trip through a full store-style decode is covered by the
        // representation_roundtrip integration test; here prove the raw
        // streams reconstruct from the slots + enc object lengths.
        let back = reassemble_for_test(&enc, &parsed);
        assert_eq!(back.commands, streams.commands);
        assert_eq!(back.literals, streams.literals);
        assert_eq!(back.offsets, streams.offsets);
    }

    /// Test-only: decode the three streams from the enc object using the
    /// parsed slots (mirrors the materializer's per-stream decode order).
    fn reassemble_for_test(enc: &EncodedSequence, slots: &[StreamSlot; 3]) -> SequenceStreams {
        use crate::rans::metadata::decode_model;
        use crate::rans::residual::decode_stream;
        let seq_start = 0usize;
        let seq_end = enc.seq_len as usize;
        let lit_end = seq_end + enc.lit_len as usize;
        let commands: Vec<u8> = match &slots[0] {
            StreamSlot::Rans(m) => {
                let model = decode_model(m, 4096).unwrap();
                decode_stream(&model, &enc.enc_obj[seq_start..seq_end], enc.cmds as u64).unwrap()
            }
            StreamSlot::Raw => enc.enc_obj[seq_start..seq_end].to_vec(),
            StreamSlot::Empty => Vec::new(),
        };
        let copies = commands.iter().filter(|&&b| b >= 0x80).count();
        let off_out = 2 * copies;
        let literals: Vec<u8> = match &slots[1] {
            StreamSlot::Rans(m) => {
                let model = decode_model(m, 4096).unwrap();
                decode_stream(&model, &enc.enc_obj[seq_end..lit_end], enc.lit_out as u64).unwrap()
            }
            StreamSlot::Raw => enc.enc_obj[seq_end..lit_end].to_vec(),
            StreamSlot::Empty => Vec::new(),
        };
        let offsets: Vec<u8> = match &slots[2] {
            StreamSlot::Rans(m) => {
                let model = decode_model(m, 4096).unwrap();
                decode_stream(&model, &enc.enc_obj[lit_end..], off_out as u64).unwrap()
            }
            StreamSlot::Raw => enc.enc_obj[lit_end..].to_vec(),
            StreamSlot::Empty => Vec::new(),
        };
        SequenceStreams {
            commands,
            literals,
            offsets,
        }
    }
}
