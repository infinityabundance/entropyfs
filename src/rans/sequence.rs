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
//!
//! # SequenceDict (Phase-9B): cross-chunk dictionary context
//!
//! `SEQUENCE_DICT` extends the same command semantics with a fourth stream:
//! the *copy-source* stream, one byte per copy command, saying whether the
//! command's u16 value is a LOCAL backward distance into the already-
//! materialized output (`0x00`) or a DICT absolute offset into the ≤64 KiB
//! dictionary chunk (`0x01`). The model object holds four slots, the enc
//! object four concatenated streams:
//!
//! ```text
//! commands | literals | offsets | sources
//! ```
//!
//! The dictionary is a content-addressed chunk reference (the previous
//! same-file chunk, Phase-9B v1). The descriptor's `dictionary_len` bounds
//! DICT offsets (u16 → ≤ 65536) and the reference depth is accounted like
//! a base chain: the dictionary's own chain depth plus 1 must not exceed
//! `max_reference_depth`, so cross-chunk dictionary chains can never
//! defeat bounded random access. SequenceDict stays a *distinct* family
//! from `BaseSequence` (temporal deltas) and `SequenceRans` (local-only)
//! to preserve the attribution boundary.

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
/// Copy-source symbol: the u16 value is a backward distance in the
/// already-materialized output (byte-progressive copy).
pub const SRC_LOCAL: u8 = 0x00;
/// Copy-source symbol: the u16 value is an absolute offset into the
/// dictionary chunk.
pub const SRC_DICT: u8 = 0x01;
/// Copy-source symbol (Phase-9C): the u16 value is an absolute offset into
/// the shared cross-file dictionary chunk.
pub const SRC_SHARED: u8 = 0x02;
/// Maximum dictionary size: DICT offsets are u16 LE, so a dictionary can
/// be at most 64 KiB (offsets 0..=65535).
pub const MAX_DICT: usize = 65536;
/// Dict-match chain depth cap (deterministic, bounded search).
const DICT_CHAIN_DEPTH: usize = 8;
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

/// The fully encoded N-stream family (Phase-9B generalization): model
/// object, enc object, and one encoded length per stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedStreams {
    /// Model object payload (N slots).
    pub model_obj: Vec<u8>,
    /// Enc object payload (N concatenated streams).
    pub enc_obj: Vec<u8>,
    /// Encoded length of each stream, in stream order.
    pub lens: Vec<u32>,
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

/// The four raw streams of a SequenceDict parse (Phase-9B): the usual
/// commands/literals/offsets plus one copy-source byte per copy command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictStreams {
    /// One command byte per command (same semantics as SequenceRans).
    pub commands: Vec<u8>,
    /// Literal-run bytes in command order.
    pub literals: Vec<u8>,
    /// One u16 LE offset per copy command (LOCAL: distance; DICT: absolute
    /// dictionary offset).
    pub offsets: Vec<u8>,
    /// One source byte (`SRC_LOCAL`/`SRC_DICT`) per copy command.
    pub sources: Vec<u8>,
}

/// The raw LZ77 streams for `input` against both the local history and the
/// external dictionary (greedy, bounded hash-chain matcher).
///
/// At every position the longer of the local match and the dictionary
/// match wins; equal lengths deterministically prefer the LOCAL match
/// (identical stream cost, cheaper decoder state). Deterministic and
/// bounded: both chain walks are depth-capped, offsets/distances are u16,
/// every loop is length-bounded by `input.len()`. Returns `None` for an
/// empty input or an unusable dictionary.
pub fn encode_sequence_dict(input: &[u8], dict: &[u8]) -> Option<DictStreams> {
    let n = input.len();
    if n == 0 || dict.is_empty() || dict.len() > MAX_DICT {
        return None;
    }
    let mut commands = Vec::new();
    let mut literals = Vec::new();
    let mut offsets = Vec::new();
    let mut sources = Vec::new();
    let hsize = 1usize << 16;
    // Local hash chains over the input (as consumed).
    let mut head = vec![u32::MAX; hsize];
    let mut chain = vec![u32::MAX; n];
    // Dictionary hash chains over the whole dictionary (built once; the
    // dictionary is immutable for the duration of the parse).
    let mut d_head = vec![u32::MAX; hsize];
    let mut d_chain = vec![u32::MAX; dict.len()];
    let dict_limit = dict.len().saturating_sub(MIN_MATCH - 1);
    for (p, slot) in d_chain.iter_mut().enumerate().take(dict_limit) {
        let h = hash_at(dict, p);
        *slot = d_head[h];
        d_head[h] = p as u32;
    }
    let mut pos = 0usize;
    while pos < n {
        if pos + MIN_MATCH <= n {
            if let Some((dist, len, source)) =
                best_match(input, pos, dict, &head, &chain, &d_head, &d_chain)
            {
                // Same copy-clipping contract as SequenceRans: a tail
                // remainder of 1..=3 bytes would decode as a 128-byte
                // literal run — clip it so the remainder lands in the
                // literal path (byte-exactness preserved).
                let mut len = len;
                let rem = len % MAX_COPY;
                if rem > 0 && rem < MIN_MATCH {
                    len -= rem;
                }
                let mut remaining = len;
                // A LOCAL copy is byte-progressive (continuation commands
                // repeat the same distance over the growing output); a
                // DICT copy reads a contiguous dict range, so each
                // continuation command must carry the ADVANCED absolute
                // offset (dict[off + i*131 ..]) — the decoder reads every
                // command's u16 independently.
                let mut cur_off = dist;
                while remaining > 0 {
                    let take = remaining.min(MAX_COPY);
                    debug_assert!((MIN_MATCH..=MAX_COPY).contains(&take));
                    commands.push((0x80 + take - MIN_MATCH) as u8);
                    offsets.extend_from_slice(&(cur_off as u16).to_le_bytes());
                    sources.push(source);
                    if source == SRC_DICT {
                        cur_off = cur_off.saturating_add(take);
                    }
                    remaining -= take;
                }
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
            let has_match = pos + MIN_MATCH <= n
                && best_match(input, pos, dict, &head, &chain, &d_head, &d_chain).is_some();
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
    }
    Some(DictStreams {
        commands,
        literals,
        offsets,
        sources,
    })
}

/// The raw LZ77 streams for `input` against the local history, the
/// previous same-file chunk (`file_dict`, may be empty = absent), and a
/// shared cross-file dictionary (`shared`, required; Phase-9C).
///
/// Three match sources, one copy-source symbol per copy: `SRC_LOCAL`
/// (backward distance), `SRC_DICT` (absolute file-dictionary offset),
/// `SRC_SHARED` (absolute shared-dictionary offset). At every position the
/// longest match wins; equal lengths deterministically prefer LOCAL, then
/// DICT, then SHARED (identical stream cost, cheapest decoder state).
/// Deterministic and bounded: all chain walks are depth-capped, offsets are
/// u16, every loop is length-bounded by `input.len()`. Returns `None` for
/// an empty input or an unusable shared dictionary.
pub fn encode_sequence_shared(
    input: &[u8],
    file_dict: &[u8],
    shared: &[u8],
) -> Option<DictStreams> {
    let n = input.len();
    if n == 0 || shared.is_empty() || shared.len() > MAX_DICT || file_dict.len() > MAX_DICT {
        return None;
    }
    let mut commands = Vec::new();
    let mut literals = Vec::new();
    let mut offsets = Vec::new();
    let mut sources = Vec::new();
    let hsize = 1usize << 16;
    // Local hash chains over the input (as consumed).
    let mut head = vec![u32::MAX; hsize];
    let mut chain = vec![u32::MAX; n];
    // File-dictionary hash chains (built once when present).
    let mut f_head = vec![u32::MAX; hsize];
    let mut f_chain = vec![u32::MAX; file_dict.len()];
    let f_limit = file_dict.len().saturating_sub(MIN_MATCH - 1);
    for (p, slot) in f_chain.iter_mut().enumerate().take(f_limit) {
        let h = hash_at(file_dict, p);
        *slot = f_head[h];
        f_head[h] = p as u32;
    }
    // Shared-dictionary hash chains (built once; immutable for the parse).
    let mut s_head = vec![u32::MAX; hsize];
    let mut s_chain = vec![u32::MAX; shared.len()];
    let s_limit = shared.len().saturating_sub(MIN_MATCH - 1);
    for (p, slot) in s_chain.iter_mut().enumerate().take(s_limit) {
        let h = hash_at(shared, p);
        *slot = s_head[h];
        s_head[h] = p as u32;
    }
    let mut pos = 0usize;
    while pos < n {
        if pos + MIN_MATCH <= n {
            if let Some((dist, len, source)) = best_match_shared(
                input, pos, file_dict, shared, &head, &chain, &f_head, &f_chain, &s_head, &s_chain,
            ) {
                // Same copy-clipping contract as SequenceRans: a tail
                // remainder of 1..=3 bytes would decode as a 128-byte
                // literal run — clip it so the remainder lands in the
                // literal path (byte-exactness preserved).
                let mut len = len;
                let rem = len % MAX_COPY;
                if rem > 0 && rem < MIN_MATCH {
                    len -= rem;
                }
                let mut remaining = len;
                // A LOCAL copy is byte-progressive (continuation commands
                // repeat the same distance over the growing output); a
                // DICT/SHARED copy reads a contiguous dict range, so each
                // continuation command must carry the ADVANCED absolute
                // offset (dict[off + i*131 ..]) — the decoder reads every
                // command's u16 independently.
                let mut cur_off = dist;
                while remaining > 0 {
                    let take = remaining.min(MAX_COPY);
                    debug_assert!((MIN_MATCH..=MAX_COPY).contains(&take));
                    commands.push((0x80 + take - MIN_MATCH) as u8);
                    offsets.extend_from_slice(&(cur_off as u16).to_le_bytes());
                    sources.push(source);
                    if source == SRC_DICT || source == SRC_SHARED {
                        cur_off = cur_off.saturating_add(take);
                    }
                    remaining -= take;
                }
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
            let has_match = pos + MIN_MATCH <= n
                && best_match_shared(
                    input, pos, file_dict, shared, &head, &chain, &f_head, &f_chain, &s_head,
                    &s_chain,
                )
                .is_some();
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
    }
    Some(DictStreams {
        commands,
        literals,
        offsets,
        sources,
    })
}

/// The longest of the local, file-dictionary, and shared-dictionary matches
/// at `pos` (deterministic: equal lengths prefer LOCAL, then DICT).
#[allow(clippy::too_many_arguments)]
fn best_match_shared(
    input: &[u8],
    pos: usize,
    file_dict: &[u8],
    shared: &[u8],
    head: &[u32],
    chain: &[u32],
    f_head: &[u32],
    f_chain: &[u32],
    s_head: &[u32],
    s_chain: &[u32],
) -> Option<(usize, usize, u8)> {
    let local = find_match(input, pos, head, chain);
    let f = if file_dict.is_empty() {
        None
    } else {
        find_dict_match(input, pos, file_dict, f_head, f_chain)
    };
    let s = find_dict_match(input, pos, shared, s_head, s_chain);
    let mut best: Option<(usize, usize, u8)> = local.map(|(d, l)| (d, l, SRC_LOCAL));
    if let Some((od, ol)) = f {
        let better = match best {
            Some((_, bl, _)) => ol > bl,
            None => true,
        };
        if better {
            best = Some((od, ol, SRC_DICT));
        }
    }
    if let Some((od, ol)) = s {
        let better = match best {
            Some((_, bl, _)) => ol > bl,
            None => true,
        };
        if better {
            best = Some((od, ol, SRC_SHARED));
        }
    }
    best
}

/// The longer of the local match and the dictionary match at `pos`
/// (deterministic: equal lengths prefer LOCAL).
fn best_match(
    input: &[u8],
    pos: usize,
    dict: &[u8],
    head: &[u32],
    chain: &[u32],
    d_head: &[u32],
    d_chain: &[u32],
) -> Option<(usize, usize, u8)> {
    let local = find_match(input, pos, head, chain);
    let dm = find_dict_match(input, pos, dict, d_head, d_chain);
    match (local, dm) {
        (Some((ld, ll)), Some((dd, dl))) => {
            if dl > ll {
                Some((dd, dl, SRC_DICT))
            } else {
                Some((ld, ll, SRC_LOCAL))
            }
        }
        (Some(m), None) => Some((m.0, m.1, SRC_LOCAL)),
        (None, Some(m)) => Some((m.0, m.1, SRC_DICT)),
        (None, None) => None,
    }
}

/// Find the longest dictionary match at `pos`, capped by `DICT_CHAIN_DEPTH`
/// chain walks. Returns `(offset, len)` with `len >= MIN_MATCH`. The match
/// cannot extend past the dictionary end or the input end.
fn find_dict_match(
    input: &[u8],
    pos: usize,
    dict: &[u8],
    d_head: &[u32],
    d_chain: &[u32],
) -> Option<(usize, usize)> {
    let n = input.len();
    let h = hash_at(input, pos);
    let max_len = n - pos;
    let mut c = d_head[h];
    let mut best_len = 0usize;
    let mut best_off = 0usize;
    let mut depth = 0usize;
    while c != u32::MAX && depth < DICT_CHAIN_DEPTH {
        let cpos = c as usize;
        let avail = dict.len() - cpos;
        let limit = max_len.min(avail);
        let mut l = 0usize;
        while l < limit && dict[cpos + l] == input[pos + l] {
            l += 1;
        }
        if l >= MIN_MATCH && l > best_len {
            best_len = l;
            best_off = cpos;
            if l == max_len {
                break; // matched to the input end: nothing can be longer
            }
        }
        c = d_chain[cpos];
        depth += 1;
    }
    if best_len >= MIN_MATCH {
        Some((best_off, best_len))
    } else {
        None
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
    let enc = encode_streams_n(&[
        streams.commands.clone(),
        streams.literals.clone(),
        streams.offsets.clone(),
    ])?;
    Some(EncodedSequence {
        model_obj: enc.model_obj,
        enc_obj: enc.enc_obj,
        seq_len: enc.lens[0],
        lit_len: enc.lens[1],
        off_len: enc.lens[2],
        cmds: streams.commands.len() as u32,
        lit_out: streams.literals.len() as u32,
    })
}

/// Encode N raw streams (Phase-9B generalization; N in 1..=4): per-stream
/// histogram, degenerate streams stored raw, rANS where it wins. Returns
/// `None` when any stream cannot be represented.
pub fn encode_streams_n(streams: &[Vec<u8>]) -> Option<EncodedStreams> {
    if streams.is_empty() {
        return None;
    }
    let mut model_obj = Vec::with_capacity(streams.len() * 3 + streams.len() * 512);
    let mut enc_obj = Vec::new();
    let mut lens = Vec::with_capacity(streams.len());
    for stream in streams {
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
        lens.push(payload.len() as u32);
        enc_obj.extend_from_slice(&payload);
    }
    Some(EncodedStreams {
        model_obj,
        enc_obj,
        lens,
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

/// Parse an N-slot model object (Phase-9B generalization; N in 1..=4).
/// `max_bytes` bounds the input; the per-slot model payloads are
/// additionally capped by the format's per-model bound.
pub fn parse_model_object_slots(
    bytes: &[u8],
    max_bytes: u64,
    slots: usize,
) -> Result<Vec<StreamSlot>, SequenceError> {
    if slots == 0 || slots > 4 {
        return Err(SequenceError::Malformed);
    }
    if bytes.len() as u64 > max_bytes {
        return Err(SequenceError::TooLarge {
            len: bytes.len() as u64,
            max: max_bytes,
        });
    }
    let mut out = Vec::with_capacity(slots);
    let mut pos = 0usize;
    for _ in 0..slots {
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
        let slot = match kind {
            SLOT_RANS => {
                let b = bytes.get(pos..pos + len).ok_or(SequenceError::Truncated)?;
                StreamSlot::Rans(b.to_vec())
            }
            SLOT_RAW => {
                if len != 0 {
                    return Err(SequenceError::Malformed);
                }
                StreamSlot::Raw
            }
            SLOT_EMPTY => {
                if len != 0 {
                    return Err(SequenceError::Malformed);
                }
                StreamSlot::Empty
            }
            other => return Err(SequenceError::UnknownKind(other)),
        };
        out.push(slot);
        pos += len;
    }
    if pos != bytes.len() {
        return Err(SequenceError::TrailingBytes);
    }
    Ok(out)
}

/// Parse the three-slot model object. `max_bytes` bounds the input; the
/// per-slot model payloads are additionally capped by the format's
/// per-model bound.
pub fn parse_model_object(bytes: &[u8], max_bytes: u64) -> Result<[StreamSlot; 3], SequenceError> {
    let v = parse_model_object_slots(bytes, max_bytes, 3)?;
    Ok([v[0].clone(), v[1].clone(), v[2].clone()])
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

/// Decode N streams (N in 3..=4: commands, literals, offsets, [sources])
/// from the model and enc objects, validating every length. Shared by the
/// SEQUENCE_RANS / BASE_SEQUENCE materialize paths, the SPARSE_BLOCK64
/// arm, and the SEQUENCE_DICT path.
///
/// Stream roles by index: 0 = commands (decodes to `commands_expected`),
/// 1 = literals (decodes to `literals_expected`), 2 = offsets (decodes to
/// `copies × off_per_copy`), 3 = copy sources (decodes to `copies`).
/// `copies_override` pins the copy count (SPARSE_BLOCK64's nonzero-word
/// count); `None` derives it from the decoded command stream (the
/// SequenceRans/BaseSequence/SequenceDict convention).
#[allow(clippy::too_many_arguments)] // the arguments are the format's own field set
pub fn decode_streams_n(
    ctx: &dyn crate::core::materialize::DecoderContext,
    limits: &crate::core::limits::Limits,
    refs: StreamRefs,
    encoded_lens: &[u32],
    commands_expected: u64,
    literals_expected: u64,
    copies_override: Option<u64>,
    off_per_copy: u32,
) -> Result<Vec<Vec<u8>>, crate::core::materialize::MaterializeError> {
    use crate::core::materialize::MaterializeError;
    let n = encoded_lens.len();
    if !(3..=4).contains(&n) {
        return Err(MaterializeError::InvalidDescriptor(
            "stream count must be 3 or 4".into(),
        ));
    }
    let StreamRefs {
        model,
        enc_obj,
        scale_bits,
        codec,
    } = refs;
    // Stream lengths must compose exactly to the enc object.
    let enc_total: u64 = encoded_lens.iter().map(|&l| l as u64).sum();
    if enc_total > limits.max_alloc_bytes {
        return Err(MaterializeError::AllocTooLarge {
            requested: enc_total,
            max: limits.max_alloc_bytes,
        });
    }
    let model_bytes = ctx.fetch_object(&model)?;
    let slots = parse_model_object_slots(
        &model_bytes,
        max_model_object_bytes_n(limits.max_model_bytes, n),
        n,
    )
    .map_err(|e| MaterializeError::Sequence(e.to_string()))?;
    let enc = ctx.fetch_object(&enc_obj)?;
    if enc.len() as u64 != enc_total {
        return Err(MaterializeError::InvalidDescriptor(
            "enc object length mismatch".into(),
        ));
    }
    let mut slices: Vec<&[u8]> = Vec::with_capacity(n);
    let mut p = 0usize;
    for &l in encoded_lens {
        slices.push(&enc[p..p + l as usize]);
        p += l as usize;
    }

    // Stream 0: commands (decoded first; the copy count derives from it
    // unless the caller pinned it).
    let commands: Vec<u8> = match &slots[0] {
        StreamSlot::Rans(m) => {
            ctx.decode_rans(m, slices[0], scale_bits, codec, commands_expected)?
        }
        StreamSlot::Raw => {
            if slices[0].len() as u64 != commands_expected {
                return Err(MaterializeError::InvalidDescriptor(
                    "raw command stream length mismatch".into(),
                ));
            }
            slices[0].to_vec()
        }
        StreamSlot::Empty => {
            return Err(MaterializeError::InvalidDescriptor(
                "empty command stream".into(),
            ));
        }
    };
    if commands.len() as u64 != commands_expected {
        return Err(MaterializeError::InvalidDescriptor(
            "command stream decoded length mismatch".into(),
        ));
    }
    let copies = match copies_override {
        Some(c) => c,
        None => commands.iter().filter(|&&b| b >= 0x80).count() as u64,
    };
    let off_out = copies.checked_mul(off_per_copy as u64).ok_or_else(|| {
        MaterializeError::InvalidDescriptor("offset stream length overflow".into())
    })?;
    if off_out > limits.max_alloc_bytes {
        return Err(MaterializeError::AllocTooLarge {
            requested: off_out,
            max: limits.max_alloc_bytes,
        });
    }

    // Stream 1: literals.
    let literals: Vec<u8> = match &slots[1] {
        StreamSlot::Rans(m) => {
            ctx.decode_rans(m, slices[1], scale_bits, codec, literals_expected)?
        }
        StreamSlot::Raw => {
            if slices[1].len() as u64 != literals_expected {
                return Err(MaterializeError::InvalidDescriptor(
                    "raw literal stream length mismatch".into(),
                ));
            }
            slices[1].to_vec()
        }
        StreamSlot::Empty => {
            if literals_expected != 0 || slices[1].len() as u64 != 0 {
                return Err(MaterializeError::InvalidDescriptor(
                    "non-empty literal stream without a model".into(),
                ));
            }
            Vec::new()
        }
    };
    if literals.len() as u64 != literals_expected {
        return Err(MaterializeError::InvalidDescriptor(
            "literal stream decoded length mismatch".into(),
        ));
    }

    // Stream 2: offsets.
    let offsets: Vec<u8> = match &slots[2] {
        StreamSlot::Rans(m) => ctx.decode_rans(m, slices[2], scale_bits, codec, off_out)?,
        StreamSlot::Raw => {
            if slices[2].len() as u64 != off_out {
                return Err(MaterializeError::InvalidDescriptor(
                    "raw offset stream length mismatch".into(),
                ));
            }
            slices[2].to_vec()
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

    let mut out = vec![commands, literals, offsets];
    if n == 4 {
        // Stream 3: copy sources (one byte per copy command).
        let sources: Vec<u8> = match &slots[3] {
            StreamSlot::Rans(m) => ctx.decode_rans(m, slices[3], scale_bits, codec, copies)?,
            StreamSlot::Raw => {
                if slices[3].len() as u64 != copies {
                    return Err(MaterializeError::InvalidDescriptor(
                        "raw source stream length mismatch".into(),
                    ));
                }
                slices[3].to_vec()
            }
            StreamSlot::Empty => {
                if copies != 0 {
                    return Err(MaterializeError::InvalidDescriptor(
                        "non-empty source stream without a model".into(),
                    ));
                }
                Vec::new()
            }
        };
        if sources.len() as u64 != copies {
            return Err(MaterializeError::InvalidDescriptor(
                "source stream decoded length mismatch".into(),
            ));
        }
        out.push(sources);
    }
    Ok(out)
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
    let v = decode_streams_n(
        ctx,
        limits,
        refs,
        &[lens.seq_len, lens.lit_len, lens.off_len],
        lens.cmds as u64,
        lens.lit_out as u64,
        units.map(|u| u as u64),
        off_per_copy,
    )?;
    Ok(DecodedStreams {
        commands: v[0].clone(),
        literals: v[1].clone(),
        offsets: v[2].clone(),
    })
}

/// The descriptor stream-length fields of the SEQUENCE_DICT family
/// (four streams: commands, literals, offsets, copy sources).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FourStreams {
    /// Encoded command-stream length.
    pub seq_len: u32,
    /// Encoded literal-stream length.
    pub lit_len: u32,
    /// Encoded offset-stream length.
    pub off_len: u32,
    /// Encoded copy-source-stream length.
    pub src_len: u32,
    /// Decoded command count.
    pub cmds: u32,
    /// Decoded literal byte count.
    pub lit_out: u32,
}

/// Decode the four streams (commands, literals, offsets, copy sources)
/// for the SEQUENCE_DICT family. The copy count derives from the command
/// stream; each copy consumes one u16 offset and one source byte.
pub fn decode_four_streams(
    ctx: &dyn crate::core::materialize::DecoderContext,
    limits: &crate::core::limits::Limits,
    refs: StreamRefs,
    lens: FourStreams,
) -> Result<DictStreams, crate::core::materialize::MaterializeError> {
    let v = decode_streams_n(
        ctx,
        limits,
        refs,
        &[lens.seq_len, lens.lit_len, lens.off_len, lens.src_len],
        lens.cmds as u64,
        lens.lit_out as u64,
        None,
        2,
    )?;
    Ok(DictStreams {
        commands: v[0].clone(),
        literals: v[1].clone(),
        offsets: v[2].clone(),
        sources: v[3].clone(),
    })
}

/// Max model-object size for N per-stream models plus the slot headers,
/// bounded against the format's per-model cap.
pub const fn max_model_object_bytes_n(per_model: u64, slots: usize) -> u64 {
    per_model.saturating_mul(slots as u64).saturating_add(64)
}

/// Max model-object size for one chunk: three per-stream models plus the
/// slot headers, bounded against the format's per-model cap.
pub const fn max_model_object_bytes(per_model: u64) -> u64 {
    max_model_object_bytes_n(per_model, 3)
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

/// The SequenceDict candidate family (Phase-9B): local-history + external
/// dictionary match coding over the same command semantics, with a fourth
/// copy-source stream. The dictionary is the previous same-file chunk
/// (v1); the reference is explicit, costed, and depth-capped.
#[derive(Debug, Clone)]
pub struct SequenceDictEncoder {
    /// Content id of the dictionary chunk (must resolve in the chunk
    /// index at decode time).
    pub dictionary: crate::core::extent::ChunkId,
    /// Materialized dictionary bytes (≤ 64 KiB).
    pub dict_bytes: Vec<u8>,
    /// Reference depth the dictionary chunk already contributes (its own
    /// chain depth). The candidate's depth is `dict_depth + 1`; the
    /// encoder refuses candidates that would exceed the decode cap.
    pub dict_depth: u8,
}

impl Encoder for SequenceDictEncoder {
    fn name(&self) -> &'static str {
        "SEQUENCE_DICT"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        if input.is_empty() || input.len() as u64 > ctx.limits.max_chunk_size {
            return Vec::new();
        }
        // Depth cap: a dictionary chain must never defeat bounded random
        // access (Phase-9B constraint; §51). The decode-time cap would
        // catch it, but refusing at encode time avoids wasted validation.
        if self.dict_depth.saturating_add(1) > ctx.limits.max_reference_depth {
            return Vec::new();
        }
        // LZ overhead (four models + four streams + dictionary reference)
        // cannot win on tiny inputs; skip the CPU.
        if input.len() < 128 {
            return Vec::new();
        }
        if self.dict_bytes.is_empty() || self.dict_bytes.len() > MAX_DICT {
            return Vec::new();
        }
        let streams = match encode_sequence_dict(input, &self.dict_bytes) {
            Some(s) => s,
            None => return Vec::new(),
        };
        // When the dictionary contributed nothing (no DICT copies), the
        // parse is strictly a SequenceRans parse with an extra 32-byte
        // reference and a fourth stream: skip it so the local-only family
        // wins on cost without the wasted descriptor.
        if !streams.sources.contains(&SRC_DICT) {
            return Vec::new();
        }
        let cmds = streams.commands.len() as u32;
        let lit_out = streams.literals.len() as u32;
        let enc = match encode_streams_n(&[
            streams.commands,
            streams.literals,
            streams.offsets,
            streams.sources,
        ]) {
            Some(e) => e,
            None => return Vec::new(),
        };
        let model_obj = ObjectRecord::model(enc.model_obj);
        let enc_obj = ObjectRecord::data(enc.enc_obj);
        let rep = Representation::SequenceDict {
            dictionary: self.dictionary,
            dictionary_len: self.dict_bytes.len() as u32,
            model: model_obj.id,
            enc_obj: enc_obj.id,
            scale_bits: SCALE_BITS,
            codec: CODEC,
            seq_len: enc.lens[0],
            lit_len: enc.lens[1],
            off_len: enc.lens[2],
            src_len: enc.lens[3],
            cmds,
            lit_out,
            len: input.len() as u64,
        };
        // Honest gate: descriptor + model object + enc object (the
        // dictionary chunk itself is counted as a reference, and its own
        // persisted state is accounted wherever it is materialized) must
        // beat the raw bytes, else RAW/SequenceRans wins on cost.
        let total = rep
            .encoded_size()
            .saturating_add(model_obj.payload.len() as u64)
            .saturating_add(enc_obj.payload.len() as u64);
        if total >= input.len() as u64 {
            return Vec::new();
        }
        let split = ByteSplit {
            // dictionary + model + enc content ids.
            reference: 96,
            ..Default::default()
        };
        let mut cost = crate::core::candidate::account_objects(
            crate::core::cost::estimate(&rep, &split, model_obj.payload.len() as u64),
            &[enc_obj.clone(), model_obj.clone()],
        );
        // The candidate's reference depth includes the dictionary chunk's
        // own chain depth (§15: λ_depth penalizes deep chains; the
        // decode-time cap is enforced in `materialize`).
        cost.depth = cost.depth.saturating_add(self.dict_depth);
        vec![Candidate {
            representation: rep,
            objects: vec![enc_obj, model_obj],
            cost,
            content_id: ctx.content_id,
        }]
    }
}

/// The SequenceSharedDict candidate family (Phase-9C): local-history +
/// optional previous-file-chunk dictionary + a shared cross-file dictionary
/// in one stream, with a fourth copy-source stream whose per-copy byte
/// selects LOCAL / DICT / SHARED. The shared dictionary is chosen by the
/// background optimizer to amortize structure common to a file family; it
/// is a content-addressed chunk reference, so its own persisted state is
/// accounted where it is materialized, and the reference depth
/// (max(file-dict depth, shared depth) + 1) is capped by
/// `max_reference_depth`.
#[derive(Debug, Clone)]
pub struct SequenceSharedDictEncoder {
    /// Content id of the previous same-file chunk (ZERO = absent).
    pub dictionary: crate::core::extent::ChunkId,
    /// Materialized file-dictionary bytes (empty = absent).
    pub dict_bytes: Vec<u8>,
    /// Reference depth of the file dictionary chunk.
    pub dict_depth: u8,
    /// Content id of the shared dictionary chunk.
    pub shared: crate::core::extent::ChunkId,
    /// Materialized shared dictionary bytes (≤ 64 KiB).
    pub shared_bytes: Vec<u8>,
    /// Reference depth of the shared dictionary chunk.
    pub shared_depth: u8,
}

impl Encoder for SequenceSharedDictEncoder {
    fn name(&self) -> &'static str {
        "SEQUENCE_SHARED_DICT"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        if input.is_empty() || input.len() as u64 > ctx.limits.max_chunk_size {
            return Vec::new();
        }
        // Depth cap: a dictionary chain must never defeat bounded random
        // access (Phase-9C constraint, §51). The decode-time cap would
        // catch it, but refusing at encode time avoids wasted validation.
        let dict_depth = self.dict_depth.max(self.shared_depth);
        if dict_depth.saturating_add(1) > ctx.limits.max_reference_depth {
            return Vec::new();
        }
        // LZ overhead (four models + four streams + two references) cannot
        // win on tiny inputs; skip the CPU.
        if input.len() < 128 {
            return Vec::new();
        }
        if self.shared_bytes.is_empty()
            || self.shared_bytes.len() > MAX_DICT
            || self.dict_bytes.len() > MAX_DICT
        {
            return Vec::new();
        }
        let streams = match encode_sequence_shared(input, &self.dict_bytes, &self.shared_bytes) {
            Some(s) => s,
            None => return Vec::new(),
        };
        // When the shared dictionary contributed nothing (no SHARED
        // copies), the parse is at best a SequenceDict/SequenceRans parse
        // with an extra 32-byte reference and fourth-stream entropy: skip
        // it so the cheaper family wins on cost without the wasted
        // descriptor.
        if !streams.sources.contains(&SRC_SHARED) {
            return Vec::new();
        }
        let cmds = streams.commands.len() as u32;
        let lit_out = streams.literals.len() as u32;
        let enc = match encode_streams_n(&[
            streams.commands,
            streams.literals,
            streams.offsets,
            streams.sources,
        ]) {
            Some(e) => e,
            None => return Vec::new(),
        };
        let model_obj = ObjectRecord::model(enc.model_obj);
        let enc_obj = ObjectRecord::data(enc.enc_obj);
        let rep = Representation::SequenceSharedDict {
            dictionary: self.dictionary,
            dictionary_len: self.dict_bytes.len() as u32,
            shared: self.shared,
            shared_len: self.shared_bytes.len() as u32,
            model: model_obj.id,
            enc_obj: enc_obj.id,
            scale_bits: SCALE_BITS,
            codec: CODEC,
            seq_len: enc.lens[0],
            lit_len: enc.lens[1],
            off_len: enc.lens[2],
            src_len: enc.lens[3],
            cmds,
            lit_out,
            len: input.len() as u64,
        };
        // Honest gate: descriptor + model object + enc object (the
        // dictionary chunks themselves are references; their persisted
        // state is accounted where they are materialized) must beat the
        // raw bytes, else RAW/SequenceRans wins on cost.
        let total = rep
            .encoded_size()
            .saturating_add(model_obj.payload.len() as u64)
            .saturating_add(enc_obj.payload.len() as u64);
        if total >= input.len() as u64 {
            return Vec::new();
        }
        let split = ByteSplit {
            // file dict + shared + model + enc content ids.
            reference: 128,
            ..Default::default()
        };
        let mut cost = crate::core::candidate::account_objects(
            crate::core::cost::estimate(&rep, &split, model_obj.payload.len() as u64),
            &[enc_obj.clone(), model_obj.clone()],
        );
        // The candidate's reference depth includes the deeper of the two
        // dictionary chunks' own chain depths (§15).
        cost.depth = cost.depth.saturating_add(dict_depth);
        vec![Candidate {
            representation: rep,
            objects: vec![enc_obj, model_obj],
            cost,
            content_id: ctx.content_id,
        }]
    }
}

/// The SequenceDict candidate family (Phase-9B): local-history + external
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

    /// A dictionary chunk with strong repeated structure (the versioned
    /// corpus's chunk-0 class: period-7 pattern). Exactly 64 KiB so it is
    /// addressable by u16 DICT offsets.
    fn dict_chunk() -> Vec<u8> {
        let mut out = Vec::with_capacity(65536);
        let pattern: Vec<u8> = (0..7u32).map(|i| (i * 37 % 251) as u8).collect();
        while out.len() < 65536 {
            let take = (65536 - out.len()).min(pattern.len());
            out.extend_from_slice(&pattern[..take]);
        }
        assert_eq!(out.len(), MAX_DICT);
        out
    }

    #[test]
    fn dict_parse_uses_dictionary_and_roundtrips_exactly() {
        // Input = the dictionary pattern with a light edit: most of the
        // input is DICT-copyable, so the parse must use DICT sources and
        // the manual walk must reproduce the input byte-exactly.
        let dict = dict_chunk();
        let mut input = dict.clone();
        input[100] ^= 0x5A;
        input[65535] ^= 0x01;
        let streams = encode_sequence_dict(&input, &dict).unwrap();
        assert!(
            streams.sources.contains(&SRC_DICT),
            "expected DICT copies on a dictionary-correlated input"
        );
        // Manual decoder walk (mirrors the materialize path).
        let mut lits = 0usize;
        let mut offs = 0usize;
        let mut srcs = 0usize;
        let mut out = Vec::with_capacity(input.len());
        for (i, &cmd) in streams.commands.iter().enumerate() {
            if cmd < 0x80 {
                let run = cmd as usize + 1;
                assert!(lits + run <= streams.literals.len(), "lit overflow at {i}");
                out.extend_from_slice(&streams.literals[lits..lits + run]);
                lits += run;
            } else {
                let clen = cmd as usize - 0x80 + 4;
                assert!(srcs < streams.sources.len(), "src exhausted at {i}");
                let source = streams.sources[srcs];
                srcs += 1;
                assert!(offs + 2 <= streams.offsets.len(), "off exhausted at {i}");
                let v =
                    u16::from_le_bytes([streams.offsets[offs], streams.offsets[offs + 1]]) as usize;
                offs += 2;
                match source {
                    SRC_LOCAL => {
                        assert!(v > 0 && v <= out.len(), "bad dist {v} at {i}");
                        for _ in 0..clen {
                            let b = out[out.len() - v];
                            out.push(b);
                        }
                    }
                    SRC_DICT => {
                        assert!(v + clen <= dict.len(), "dict copy out of bounds at {i}");
                        out.extend_from_slice(&dict[v..v + clen]);
                    }
                    other => panic!("unknown source {other} at {i}"),
                }
            }
        }
        assert_eq!(srcs, streams.sources.len());
        assert_eq!(out, input);
    }

    #[test]
    fn dict_long_match_continuation_advances_offset() {
        // Regression (Phase-9B): a DICT match longer than MAX_COPY is
        // split into continuation commands. A LOCAL continuation repeats
        // the same backward distance (byte-progressive); a DICT
        // continuation must ADVANCE the absolute offset, or the decoder
        // re-reads the same dict bytes.
        //
        // Dict layout: a 25536-byte sequence S, then S again (40 KiB in
        // the second copy). The hash bucket for S[0..4] has positions 0
        // and 25536; the chain head is 25536 (higher), with 40 KiB of
        // availability — a long contiguous DICT match for an input equal
        // to dict[25536..65536].
        let seq: Vec<u8> = (0..25536u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 16) as u8)
            .collect();
        let mut dict = seq.clone();
        dict.extend_from_slice(&seq);
        // Pad with the start of seq so the second 40 KiB run is exactly
        // seq ++ seq[..14464] (the same bytes as dict[..40000]).
        dict.extend_from_slice(&seq[..65536 - 2 * seq.len()]);
        assert_eq!(dict.len(), MAX_DICT);
        assert_eq!(&dict[25536..], &dict[..65536 - 25536]);
        let input: Vec<u8> = dict[25536..].to_vec(); // 40000 bytes
        let streams = encode_sequence_dict(&input, &dict).unwrap();
        assert!(streams.sources.contains(&SRC_DICT));
        // Manual decoder walk: DICT copies must compose to the input even
        // across continuation commands (offset advancement).
        let mut lits = 0usize;
        let mut offs = 0usize;
        let mut srcs = 0usize;
        let mut out = Vec::with_capacity(input.len());
        let mut dict_copies = 0usize;
        for &cmd in &streams.commands {
            if cmd < 0x80 {
                let run = cmd as usize + 1;
                out.extend_from_slice(&streams.literals[lits..lits + run]);
                lits += run;
            } else {
                let clen = cmd as usize - 0x80 + 4;
                let source = streams.sources[srcs];
                srcs += 1;
                let v =
                    u16::from_le_bytes([streams.offsets[offs], streams.offsets[offs + 1]]) as usize;
                offs += 2;
                match source {
                    SRC_LOCAL => {
                        for _ in 0..clen {
                            let b = out[out.len() - v];
                            out.push(b);
                        }
                    }
                    SRC_DICT => {
                        out.extend_from_slice(&dict[v..v + clen]);
                        dict_copies += 1;
                    }
                    other => panic!("unknown source {other}"),
                }
            }
        }
        assert!(
            dict_copies >= 2,
            "expected a continuation chain (got {dict_copies} DICT copies)"
        );
        assert_eq!(out, input, "long DICT continuation must advance offsets");
    }

    #[test]
    fn dict_encoder_wins_and_validates() {
        let limits = Limits::default();
        let policy = Policy::default();
        let dict = dict_chunk();
        let mut input = dict.clone();
        for i in (0..65536).step_by(17) {
            input[i] ^= 0x03;
        }
        let dict_id = crate::core::extent::ChunkId::of(&dict);
        let enc = SequenceDictEncoder {
            dictionary: dict_id,
            dict_bytes: dict.clone(),
            dict_depth: 0,
        };
        let ctx = ctx_for(&input, &limits, &policy);
        let cands = enc.encode(&input, &ctx);
        assert_eq!(cands.len(), 1);
        let cand = &cands[0];
        assert!(matches!(
            cand.representation,
            Representation::SequenceDict { .. }
        ));
        // Depth must be 1 (dict_depth 0 + the reference itself).
        assert_eq!(cand.cost.depth, 1);
        let mut resolver = MemResolver::from_map(
            cand.objects
                .iter()
                .map(|o| (o.id, o.payload.clone()))
                .collect(),
        );
        // The dictionary chunk must resolve at decode: register it as a
        // RAW chunk whose payload object is the dict bytes.
        resolver.put_chunk(
            dict_id,
            Representation::Raw {
                obj: dict_id,
                len: dict.len() as u64,
            },
        );
        resolver.put_object(dict_id, dict);
        validate_candidate(cand, &input, &resolver, &limits).unwrap();
        // The dictionary reference must dominate the win (the family is
        // meaningfully cheaper than the raw bytes).
        assert!(
            cand.cost.persisted_bytes() < input.len() as u64 / 4,
            "persisted {} >= raw/4",
            cand.cost.persisted_bytes()
        );
    }

    #[test]
    fn dict_skips_unrelated_dictionary() {
        // A dictionary sharing no byte values with the input (all 0xFF vs
        // text) can never produce a DICT match: no DICT copies appear and
        // the family declines (a SequenceRans-only parse would win there).
        let limits = Limits::default();
        let policy = Policy::default();
        let dict = vec![0xFFu8; 65536];
        let mut input = text_chunk();
        input.resize(65536, b' ');
        let enc = SequenceDictEncoder {
            dictionary: crate::core::extent::ChunkId::of(&dict),
            dict_bytes: dict,
            dict_depth: 0,
        };
        let cands = enc.encode(&input, &ctx_for(&input, &limits, &policy));
        assert!(cands.is_empty());
    }

    #[test]
    fn dict_depth_cap_refuses_candidate() {
        // A dictionary whose chain already hits the depth cap must produce
        // no candidate: the reference would defeat bounded random access.
        let limits = Limits::default();
        let policy = Policy::default();
        let dict = dict_chunk();
        let input = text_chunk();
        let enc = SequenceDictEncoder {
            dictionary: crate::core::extent::ChunkId::of(&dict),
            dict_bytes: dict,
            dict_depth: limits.max_reference_depth, // already at the cap
        };
        let cands = enc.encode(&input, &ctx_for(&input, &limits, &policy));
        assert!(cands.is_empty());
    }

    #[test]
    fn dict_urandom_has_no_fake_density() {
        // H6-style negative control: urandom input against a DIFFERENT
        // urandom dictionary (every input byte XOR-decorrelated from the
        // dict so no 4-byte window can match) must not produce a
        // candidate — a random implicit dictionary creates no free
        // compression.
        let limits = Limits::default();
        let policy = Policy::default();
        let dict = noise(65536);
        let mut input = noise(65536);
        for b in &mut input {
            *b ^= 0xAA;
        }
        let enc = SequenceDictEncoder {
            dictionary: crate::core::extent::ChunkId::of(&dict),
            dict_bytes: dict,
            dict_depth: 0,
        };
        assert!(
            enc.encode(&input, &ctx_for(&input, &limits, &policy))
                .is_empty()
        );
    }

    #[test]
    fn dict_dictionary_must_be_bounded() {
        // A dictionary larger than 64 KiB cannot be addressed by u16
        // offsets: the encoder must refuse it.
        let limits = Limits::default();
        let policy = Policy::default();
        let dict = vec![0u8; MAX_DICT + 1];
        let enc = SequenceDictEncoder {
            dictionary: crate::core::extent::ChunkId::of(&dict),
            dict_bytes: dict,
            dict_depth: 0,
        };
        let input = text_chunk();
        assert!(
            enc.encode(&input, &ctx_for(&input, &limits, &policy))
                .is_empty()
        );
    }

    // -------------------------------------------------------------------
    // Phase-9C: SequenceSharedDict (local + optional file dict + shared
    // dict, SRC_SHARED copy source).
    // -------------------------------------------------------------------

    #[test]
    fn shared_parse_uses_shared_dictionary_and_roundtrips_exactly() {
        // Input = the SHARED dictionary pattern with a light edit: most of
        // the input is SHARED-copyable, so the parse must use SRC_SHARED
        // and the manual walk must reproduce the input byte-exactly.
        let shared = dict_chunk();
        let mut input = shared.clone();
        input[100] ^= 0x5A;
        input[65535] ^= 0x01;
        let streams = encode_sequence_shared(&input, &[], &shared).unwrap();
        assert!(
            streams.sources.contains(&SRC_SHARED),
            "expected SHARED copies on a shared-dict-correlated input"
        );
        // Manual decoder walk (mirrors the materialize path).
        let mut lits = 0usize;
        let mut offs = 0usize;
        let mut srcs = 0usize;
        let mut out = Vec::with_capacity(input.len());
        for (i, &cmd) in streams.commands.iter().enumerate() {
            if cmd < 0x80 {
                let run = cmd as usize + 1;
                out.extend_from_slice(&streams.literals[lits..lits + run]);
                lits += run;
            } else {
                let clen = cmd as usize - 0x80 + 4;
                let source = streams.sources[srcs];
                srcs += 1;
                let v =
                    u16::from_le_bytes([streams.offsets[offs], streams.offsets[offs + 1]]) as usize;
                offs += 2;
                match source {
                    SRC_LOCAL => {
                        assert!(v > 0 && v <= out.len(), "bad dist {v} at {i}");
                        for _ in 0..clen {
                            let b = out[out.len() - v];
                            out.push(b);
                        }
                    }
                    SRC_SHARED => {
                        assert!(v + clen <= shared.len(), "shared copy out of bounds at {i}");
                        out.extend_from_slice(&shared[v..v + clen]);
                    }
                    other => panic!("unknown source {other} at {i}"),
                }
            }
        }
        assert_eq!(out, input);
    }

    #[test]
    fn shared_long_match_continuation_advances_offset() {
        // Regression (Phase-9C, same class as the Phase-9B DICT bug): a
        // SHARED match longer than MAX_COPY is split into continuation
        // commands whose absolute offsets must ADVANCE, or the decoder
        // re-reads the same shared-dict bytes.
        let seq: Vec<u8> = (0..25536u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 16) as u8)
            .collect();
        let mut shared = seq.clone();
        shared.extend_from_slice(&seq);
        shared.extend_from_slice(&seq[..65536 - 2 * seq.len()]);
        assert_eq!(shared.len(), MAX_DICT);
        let input: Vec<u8> = shared[25536..].to_vec(); // 40000 bytes
        let streams = encode_sequence_shared(&input, &[], &shared).unwrap();
        assert!(streams.sources.contains(&SRC_SHARED));
        let mut lits = 0usize;
        let mut offs = 0usize;
        let mut srcs = 0usize;
        let mut out = Vec::with_capacity(input.len());
        let mut shared_copies = 0usize;
        for &cmd in &streams.commands {
            if cmd < 0x80 {
                let run = cmd as usize + 1;
                out.extend_from_slice(&streams.literals[lits..lits + run]);
                lits += run;
            } else {
                let clen = cmd as usize - 0x80 + 4;
                let source = streams.sources[srcs];
                srcs += 1;
                let v =
                    u16::from_le_bytes([streams.offsets[offs], streams.offsets[offs + 1]]) as usize;
                offs += 2;
                match source {
                    SRC_LOCAL => {
                        for _ in 0..clen {
                            let b = out[out.len() - v];
                            out.push(b);
                        }
                    }
                    SRC_SHARED => {
                        out.extend_from_slice(&shared[v..v + clen]);
                        shared_copies += 1;
                    }
                    other => panic!("unknown source {other}"),
                }
            }
        }
        assert!(
            shared_copies >= 2,
            "expected a continuation chain (got {shared_copies} SHARED copies)"
        );
        assert_eq!(out, input, "long SHARED continuation must advance offsets");
    }

    #[test]
    fn shared_three_way_parse_uses_all_sources() {
        // Input with BOTH local repetition and shared-dictionary structure:
        // the 3-way parse must produce LOCAL and SHARED copies together.
        let shared = dict_chunk();
        // input = shared[:20000] ++ 500-byte repeated pattern ++ shared[20000..]
        let mut input = Vec::new();
        input.extend_from_slice(&shared[..20000]);
        let pattern: Vec<u8> = (0..500u32).map(|i| i as u8).collect();
        for _ in 0..20 {
            input.extend_from_slice(&pattern);
        }
        input.extend_from_slice(&shared[20000..]);
        let streams = encode_sequence_shared(&input, &[], &shared).unwrap();
        assert!(streams.sources.contains(&SRC_SHARED));
        assert!(streams.sources.contains(&SRC_LOCAL));
        // Manual walk reproduces the input exactly (the interesting case
        // where both copy sources are live).
        let mut lits = 0usize;
        let mut offs = 0usize;
        let mut srcs = 0usize;
        let mut out = Vec::with_capacity(input.len());
        for &cmd in &streams.commands {
            if cmd < 0x80 {
                let run = cmd as usize + 1;
                out.extend_from_slice(&streams.literals[lits..lits + run]);
                lits += run;
            } else {
                let clen = cmd as usize - 0x80 + 4;
                let source = streams.sources[srcs];
                srcs += 1;
                let v =
                    u16::from_le_bytes([streams.offsets[offs], streams.offsets[offs + 1]]) as usize;
                offs += 2;
                match source {
                    SRC_LOCAL => {
                        assert!(v > 0 && v <= out.len());
                        for _ in 0..clen {
                            let b = out[out.len() - v];
                            out.push(b);
                        }
                    }
                    SRC_SHARED => {
                        assert!(v + clen <= shared.len());
                        out.extend_from_slice(&shared[v..v + clen]);
                    }
                    other => panic!("unknown source {other}"),
                }
            }
        }
        assert_eq!(out, input);
    }

    #[test]
    fn shared_encoder_wins_and_validates() {
        let limits = Limits::default();
        let policy = Policy::default();
        let shared = dict_chunk();
        let mut input = shared.clone();
        for i in (0..65536).step_by(17) {
            input[i] ^= 0x03;
        }
        let shared_id = crate::core::extent::ChunkId::of(&shared);
        let enc = SequenceSharedDictEncoder {
            dictionary: crate::core::extent::ChunkId::ZERO,
            dict_bytes: Vec::new(),
            dict_depth: 0,
            shared: shared_id,
            shared_bytes: shared.clone(),
            shared_depth: 0,
        };
        let ctx = ctx_for(&input, &limits, &policy);
        let cands = enc.encode(&input, &ctx);
        assert_eq!(cands.len(), 1);
        let cand = &cands[0];
        assert!(matches!(
            cand.representation,
            Representation::SequenceSharedDict { .. }
        ));
        assert_eq!(cand.cost.depth, 1);
        let mut resolver = MemResolver::from_map(
            cand.objects
                .iter()
                .map(|o| (o.id, o.payload.clone()))
                .collect(),
        );
        resolver.put_chunk(
            shared_id,
            Representation::Raw {
                obj: shared_id,
                len: shared.len() as u64,
            },
        );
        resolver.put_object(shared_id, shared);
        validate_candidate(cand, &input, &resolver, &limits).unwrap();
        assert!(
            cand.cost.persisted_bytes() < input.len() as u64 / 4,
            "persisted {} >= raw/4",
            cand.cost.persisted_bytes()
        );
    }

    #[test]
    fn shared_skips_unrelated_dictionary() {
        let limits = Limits::default();
        let policy = Policy::default();
        let shared = vec![0xFFu8; 65536];
        let mut input = text_chunk();
        input.resize(65536, b' ');
        let enc = SequenceSharedDictEncoder {
            dictionary: crate::core::extent::ChunkId::ZERO,
            dict_bytes: Vec::new(),
            dict_depth: 0,
            shared: crate::core::extent::ChunkId::of(&shared),
            shared_bytes: shared,
            shared_depth: 0,
        };
        let cands = enc.encode(&input, &ctx_for(&input, &limits, &policy));
        assert!(cands.is_empty());
    }

    #[test]
    fn shared_depth_cap_refuses_candidate() {
        let limits = Limits::default();
        let policy = Policy::default();
        let shared = dict_chunk();
        let input = text_chunk();
        let enc = SequenceSharedDictEncoder {
            dictionary: crate::core::extent::ChunkId::ZERO,
            dict_bytes: Vec::new(),
            dict_depth: 0,
            shared: crate::core::extent::ChunkId::of(&shared),
            shared_bytes: shared,
            shared_depth: limits.max_reference_depth, // already at the cap
        };
        let cands = enc.encode(&input, &ctx_for(&input, &limits, &policy));
        assert!(cands.is_empty());
    }

    #[test]
    fn shared_urandom_has_no_fake_density() {
        // Negative control: urandom input against a DIFFERENT urandom
        // shared dictionary must not produce a candidate — no free
        // compression from an unrelated dictionary.
        let limits = Limits::default();
        let policy = Policy::default();
        let shared = noise(65536);
        let mut input = noise(65536);
        for b in &mut input {
            *b ^= 0xAA;
        }
        let enc = SequenceSharedDictEncoder {
            dictionary: crate::core::extent::ChunkId::ZERO,
            dict_bytes: Vec::new(),
            dict_depth: 0,
            shared: crate::core::extent::ChunkId::of(&shared),
            shared_bytes: shared,
            shared_depth: 0,
        };
        assert!(
            enc.encode(&input, &ctx_for(&input, &limits, &policy))
                .is_empty()
        );
    }

    #[test]
    fn shared_dictionary_must_be_bounded() {
        let limits = Limits::default();
        let policy = Policy::default();
        let shared = vec![0u8; MAX_DICT + 1];
        let enc = SequenceSharedDictEncoder {
            dictionary: crate::core::extent::ChunkId::ZERO,
            dict_bytes: Vec::new(),
            dict_depth: 0,
            shared: crate::core::extent::ChunkId::of(&shared),
            shared_bytes: shared,
            shared_depth: 0,
        };
        let input = text_chunk();
        assert!(
            enc.encode(&input, &ctx_for(&input, &limits, &policy))
                .is_empty()
        );
    }
}
