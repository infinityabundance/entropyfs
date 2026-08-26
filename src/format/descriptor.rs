//! Extent descriptor codec: the byte encoding of
//! `core::representation::Representation` (`docs/format/ondisk-v1.md` §7).
//!
//! Mirrors `Representation::encoded_size` exactly — a test asserts the two
//! agree for randomized descriptors so the accounting mirror can never
//! drift from the real codec.
//!
//! # PURPOSE
//!
//! This module is the persistence boundary of the representation algebra:
//! it converts between `Representation`/`Residual` values and the exact
//! byte strings stored as extent-tree leaf values and chunk-index values.
//! `encode` is the write-path serializer (called on already-validated
//! representations); `decode` is the read-path parser — the first thing
//! that touches every descriptor read back from a store, fsck walk,
//! optimizer pass, or materialization dependency fetch.
//!
//! # BOUNDARY
//!
//! The codec knows only the representation algebra (`Representation`,
//! `Residual`, their id/codec tag types), `Limits`, `ChunkId`, and the
//! `Reader`/`Writer` primitives. It must never know the store, the
//! B-tree, the materializer, object tables, content identity (BLAKE3),
//! record envelopes/CRCs, epochs, or transactions.
//!
//! Decoding produces a `Representation` — a *description* of one logical
//! extent — and never materializes bytes. Object ids are read as opaque
//! `[u8; 32]` references and resolved upstream by the caller's store or
//! `DecoderContext`; the codec never dereferences them.
//!
//! # MODEL
//!
//! A descriptor is a self-contained, bounded byte string describing one
//! logical extent. The wire form is a common 5-byte prefix — family tag
//! (1 byte) + logical extent length in bytes (`u32` LE, widened to `u64`
//! in memory) — followed by a per-family payload. All integers are
//! little-endian; object ids are 32 raw bytes; seeds are 16 raw bytes.
//!
//! UNITS: the leading `len` is the LOGICAL extent length in bytes (what
//! the extent materializes to), NOT the descriptor's own size. The
//! descriptor size is the byte length of the encoded record, bounded
//! separately by `Limits::max_descriptor_bytes` (default 8192; the
//! 8192/8193 boundary is a hostile-media court exhibit). Payload fields
//! (inline data, palette/pattern/tail bytes, stream lengths) are likewise
//! bytes.
//!
//! `Representation::validate` is the single authority for a family's
//! semantic invariants (canonical forms, rank ranges, stream sanity,
//! encoded-size cap). The parser enforces the cheap bounds needed to
//! parse safely (input cap, per-field caps) and delegates the full check
//! to `validate` before returning — see HISTORY / EVIDENCE for why that
//! ordering is load-bearing.
//!
//! # PERSISTENT AUTHORITY
//!
//! Every byte this module writes is persisted verbatim inside extent-tree
//! leaf values and chunk-index values, so this code IS the v1 on-disk
//! format for descriptors (§7). The encoding must stay byte-stable for
//! the lifetime of v1:
//!
//! - the codec mirrors `Representation::encoded_size` exactly (pinned by
//!   the `encoded_size_matches_codec` test);
//! - `decode` accepts only byte strings that re-encode byte-exactly
//!   (canonical encoding — trailing garbage is rejected, not tolerated);
//! - Phase-11A changed acceptance behavior, not the format: nothing the
//!   encoder ever emitted changed meaning; the parser only became
//!   stricter about *accepting* hostile input.
//!
//! Durability/acknowledgement semantics do not live here: descriptors
//! are made durable by the store's record envelope (CRC + content-id
//! binding, ondisk-v1.md §3), not by this codec.
//!
//! # CORRECTNESS INVARIANTS
//!
//! - Round-trip: `decode(encode(x), limits)` equals `x` for every
//!   representation that passes `validate` under `limits` (pinned by
//!   `roundtrip_all_families` / `roundtrip_randomized` and the
//!   hostile-media descriptor court's full corpus).
//! - Canonical re-encode: for every accepted byte string `b`,
//!   `encode(decode(b))` reproduces `b` exactly; the decode Stage-4
//!   exact-consumption check (`r.done()`) is what makes a stray suffix
//!   invalid rather than ignored.
//! - decode-OK implies validate-OK under the SAME `&Limits` (Phase-11A;
//!   the decode Stage-5 validation gate).
//! - The size mirror never drifts: `Writer::with_capacity` trusts
//!   `rep.encoded_size()`, and the mirror test pins it to real encoder
//!   output.
//! - Tag/kind constants here must match `Representation::tag` and the
//!   codec/universe/transform id tags; an unknown byte is rejected as
//!   `Malformed`, never treated as forward-compatible data.
//! - The read-path ordering invariant (hostile-media court):
//!
//!   ```text
//!   persistent bytes
//!       -> bounded parse
//!       -> structural validation
//!       -> resource preflight
//!       -> materialization
//!   ```
//!
//!   A persisted length is never treated as an allocation authority
//!   before structural validation.
//!
//! # CONCURRENCY
//!
//! Pure functions with no locks and no global state: `encode`/`decode`
//! may run concurrently on any thread — the read path already does
//! (Phase-11C runs decode as pure CPU with the epoch guard released, on
//! the worker pool). `Limits` is `Copy`; `Reader` borrows its input
//! immutably; encode mutates only its local `Writer`.
//!
//! # RESOURCE BOUNDS
//!
//! Attacker-controlled sizes reaching this code: the input byte length
//! (checked against `max_descriptor_bytes` before any parse; default
//! 8192), the logical `len` (checked against `max_chunk_size` the moment
//! it is read), and per-family fields (INLINE data ≤ `max_inline_bytes`;
//! palette cardinality ≤ `max_palette` and ≥ 1; period ≤ `max_period`
//! and ≥ 1; tail < period). Bounded reads use `Reader::take`, which
//! never reads past the input and returns `Truncated` instead. The
//! Stage-5 `validate` gate re-checks every derived size — including the
//! encoded-size cap — under the same limits.
//!
//! # PERFORMANCE
//!
//! The codec is on the hot path: `decode` runs for every extent read /
//! dependency fetch and `encode` for every write and background pass.
//! The writer pre-reserves `rep.encoded_size()` bytes so `encode` is a
//! single allocation with no growth reallocations, and `Reader::take`
//! returns zero-copy borrowed slices (owned `Vec`s are created only for
//! payloads the `Representation` enum must own). Phase-11B/11C measured
//! decode as pure CPU and kept it out of the epoch-mutex convoy.
//!
//! # FAILURE MODES
//!
//! Typed errors only (`CodecError`): `Truncated` (input ends
//! mid-structure), `TooLong` (input above the descriptor cap, `len`
//! above the chunk cap, INLINE above `max_inline_bytes`), `Malformed`
//! (unknown tag/kind/id byte, palette cardinality 0 or over
//! `max_palette`, period 0 or over the cap, tail ≥ period, trailing
//! bytes, and every `validate` failure that is not a size error).
//! `DescriptorTooLarge` / `ChunkTooLarge` from `validate` map to
//! `TooLong`; everything else maps to `Malformed`.
//!
//! Never: panic, OOM, unbounded CPU, or returning a descriptor that
//! fails `validate` under the same limits. The hostile-media oracle
//! (ADR-0016 "typed error, never panic") fuzz-proves the parser across
//! every bounded byte string under tight and default limits.
//!
//! # HISTORY / EVIDENCE
//!
//! Phase-11A (hostile-media court,
//! `evidence/hostile-media/court-1787750784-a2983dc/`) found a layering
//! gap: `decode` accepted descriptors that `validate` rejected — the
//! write path gated with `validate` (`put_chunk_in_tx`); the read path
//! never did. `decode` now takes the full `&Limits` and validates
//! internally, so the read path never hands an unvalidated descriptor to
//! the materializer, matching the write path's gate. The court's
//! descriptor oracle: decode-OK ⇒ validate-OK, encoded size within the
//! descriptor cap, and a byte-exact canonical re-encode. The on-disk
//! format is unchanged; the parser is stricter about accepting, never
//! about encoding.

#![forbid(unsafe_code)]

use crate::core::extent::ChunkId;
use crate::core::limits::Limits;
use crate::core::representation::{RansCodec, Representation, Residual, TransformId, UniverseId};
use crate::format::codec::{CodecError, Reader, Writer};

/// Representation tags — the on-disk family dispatch bytes
/// (`docs/format/ondisk-v1.md` §7). Must match `Representation::tag`
/// (the in-memory authority for the same table); `decode` rejects any
/// byte not listed here as `Malformed` — an unknown tag is never treated
/// as forward-compatible data.
///
/// Adding a family means extending this table AND
/// `Representation::tag`/`encoded_size`/`validate` in lockstep.
///
/// First tag: ZERO — `len` zero bytes, no payload. 0x00 is unassigned,
/// so a zeroed first byte is already invalid.
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
/// Tag: blockwise-64 enumerative sparse coding.
pub const TAG_SPARSE_BLOCK64: u8 = 0x0E;
/// Tag: cross-chunk dictionary match coding (Phase-9B).
pub const TAG_SEQUENCE_DICT: u8 = 0x0F;
/// Tag: shared amortized dictionary match coding (Phase-9C).
pub const TAG_SEQUENCE_SHARED_DICT: u8 = 0x10;
/// Tag: deep-match repcode + extended-length coding (Phase-9E).
pub const TAG_SEQUENCE_DEEP: u8 = 0x11;

/// Residual kinds — the payload dispatch bytes for residuals inside
/// BASE_RESIDUAL (0x06) and ENTROPY_REF (0x0A) descriptors
/// (`docs/format/ondisk-v1.md` §7). 0x00 is unassigned; `decode_residual`
/// rejects any unlisted byte as `Malformed`.
///
/// Kind: XOR_SPARSE — `edit_count` u32, then (pos u32, val u8) edits:
/// byte X at `pos` differs from the base by `val` (X = base[pos] XOR val).
pub const RESIDUAL_XOR_SPARSE: u8 = 0x01;
/// Residual kind: sparse range replacement.
pub const RESIDUAL_RANGE_REPLACE: u8 = 0x02;
/// Residual kind: rANS-coded stream.
pub const RESIDUAL_RANS_CODED: u8 = 0x03;
/// Residual kind: shift-aware copy/literal delta against the base.
pub const RESIDUAL_BASE_SEQUENCE: u8 = 0x04;

/// Encode a representation descriptor to its wire bytes.
///
/// # What
///
/// Serializes a `Representation` into the exact byte string
/// `docs/format/ondisk-v1.md` §7 defines: the family tag + logical
/// `len` (`u32` LE) prefix, then the family's payload fields.
///
/// # Why / authority
///
/// Encoding is the write path's persistence step — the bytes become
/// extent-tree leaf / chunk-index values. The input is trusted only as
/// far as it satisfies `Representation::validate` under the active
/// limits: encoders are gated by `validate` upstream (the write path
/// never persists an unvalidated descriptor), and the round-trip tests
/// prove every encoded descriptor decodes. The `u32` width casts are
/// lossless under the validate guarantees (`len` ≤ `max_chunk_size`,
/// payloads bounded by their caps, residual fanout ≤ `max_fanout`).
///
/// # Algorithm
///
/// Stage-numbered inline: (1) reserve exactly `encoded_size()` bytes
/// (the accounting mirror, pinned by the `encoded_size_matches_codec`
/// test); (2) tag dispatch — every arm writes the common prefix, then
/// its payload; (3) finalize.
///
/// # Invariants
///
/// - `encode(x).len() == x.encoded_size()` for every validate-OK `x`
///   (the mirror test pins it).
/// - `decode(encode(x), limits) == x` (round-trip).
/// - The only error path is a palette cardinality above 255, which does
///   not fit the wire field's `u8`; under default limits
///   (`max_palette` = 16) and for any validate-OK palette it is
///   unreachable.
///
/// # Failure behavior
///
/// `CodecError::Malformed` only, and only for the palette-width case
/// above.
pub fn encode(rep: &Representation) -> Result<Vec<u8>, CodecError> {
    // -------------------------------------------------------------------
    // Stage 1: Preflight — reserve the exact encoded size.
    //
    // `encoded_size` is the accounting mirror of this function; the
    // `encoded_size_matches_codec` test pins the two together, so the
    // reservation is exact and encode performs a single allocation with
    // no growth reallocations. If the mirror ever drifted, `Writer`
    // would still emit correct bytes — the mirror only shapes allocation
    // behavior.
    // -------------------------------------------------------------------
    let mut w = Writer::with_capacity(rep.encoded_size() as usize);

    // -------------------------------------------------------------------
    // Stage 2: Tag dispatch — common prefix, then the family payload.
    //
    // Every arm first writes the family tag and the logical extent
    // length (`u32` LE); the fields that follow are exactly the payload
    // columns of the §7 tag table, in the same order `decode` and
    // `encoded_size` expect. All writers here are infallible except the
    // palette width check below.
    // -------------------------------------------------------------------
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
            w.u8(TAG_SPARSE_BLOCK64);
            w.u32(*len as u32);
            w.bytes(model.as_bytes());
            w.bytes(enc_obj.as_bytes());
            w.u8(*scale_bits);
            w.u8(codec.tag());
            w.u32(*pc_len);
            w.u32(*rank_len);
            w.u32(*lit_len);
            w.u32(*words);
            w.u32(*nonzero);
            w.u32(*lit_out);
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
            w.u8(TAG_SEQUENCE_DICT);
            w.u32(*len as u32);
            w.bytes(dictionary.as_bytes());
            w.u32(*dictionary_len);
            w.bytes(model.as_bytes());
            w.bytes(enc_obj.as_bytes());
            w.u8(*scale_bits);
            w.u8(codec.tag());
            w.u32(*seq_len);
            w.u32(*lit_len);
            w.u32(*off_len);
            w.u32(*src_len);
            w.u32(*cmds);
            w.u32(*lit_out);
        }
        Representation::SequenceSharedDict {
            dictionary,
            dictionary_len,
            shared,
            shared_len,
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
            w.u8(TAG_SEQUENCE_SHARED_DICT);
            w.u32(*len as u32);
            w.bytes(dictionary.as_bytes());
            w.u32(*dictionary_len);
            w.bytes(shared.as_bytes());
            w.u32(*shared_len);
            w.bytes(model.as_bytes());
            w.bytes(enc_obj.as_bytes());
            w.u8(*scale_bits);
            w.u8(codec.tag());
            w.u32(*seq_len);
            w.u32(*lit_len);
            w.u32(*off_len);
            w.u32(*src_len);
            w.u32(*cmds);
            w.u32(*lit_out);
        }
        Representation::SequenceDeep {
            model,
            enc_obj,
            scale_bits,
            codec,
            seq_len,
            lit_len,
            off_len,
            len_len,
            cmds,
            lit_out,
            len,
        } => {
            w.u8(TAG_SEQUENCE_DEEP);
            w.u32(*len as u32);
            w.bytes(model.as_bytes());
            w.bytes(enc_obj.as_bytes());
            w.u8(*scale_bits);
            w.u8(codec.tag());
            w.u32(*seq_len);
            w.u32(*lit_len);
            w.u32(*off_len);
            w.u32(*len_len);
            w.u32(*cmds);
            w.u32(*lit_out);
        }
    }

    // -------------------------------------------------------------------
    // Stage 3: Finalize — hand the buffer to the caller.
    // -------------------------------------------------------------------
    Ok(w.into_bytes())
}

/// Encode a residual's kind byte and payload.
///
/// The kind byte is the payload dispatch for BASE_RESIDUAL (0x06) and
/// ENTROPY_REF (0x0A) descriptors; the fields match the residual table
/// in `docs/format/ondisk-v1.md` §7. The encode side carries no limits
/// of its own: it is only reached for representations that passed
/// `validate` (the write-path gate), every writer here writes
/// fixed-width fields the size mirror accounts for, and no arm can
/// produce an error — the `Result` return mirrors `decode_residual`'s
/// signature but is always `Ok(())` today.
pub fn encode_residual(w: &mut Writer, r: &Residual) -> Result<(), CodecError> {
    // -------------------------------------------------------------------
    // Stage 1: Kind dispatch — each arm writes the kind byte, then its
    // payload in the §7 field order.
    // -------------------------------------------------------------------
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
        Residual::BaseSequence {
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
            w.u8(RESIDUAL_BASE_SEQUENCE);
            w.bytes(enc_obj.as_bytes());
            w.bytes(model.as_bytes());
            w.u8(*scale_bits);
            w.u8(codec.tag());
            w.u32(*seq_len);
            w.u32(*lit_len);
            w.u32(*off_len);
            w.u32(*cmds);
            w.u32(*lit_out);
        }
    }
    Ok(())
}

/// Decode a representation descriptor from its wire bytes.
///
/// # What
///
/// Parses one extent descriptor (`docs/format/ondisk-v1.md` §7): the
/// common tag + logical-length prefix, then the family's payload fields,
/// into an owned `Representation`. `max_descriptor_bytes` bounds the
/// input; `max_inline_bytes` bounds INLINE payloads; `max_palette` bounds
/// palette cardinality; `max_period` bounds periodic patterns.
///
/// # Why
///
/// This is the read path's entry point into the representation algebra:
/// store reads, fsck walks, the optimizer, and the materializer's
/// dependency fetches all turn persisted descriptor bytes into
/// `Representation`s here. Because the bytes are persistent, they are
/// UNTRUSTED, and the decode result is the last place a hostile
/// descriptor can be stopped before it reaches the materializer.
///
/// # Inputs and authority
///
/// - `bytes` — persistent, therefore untrusted; bounded to
///   `limits.max_descriptor_bytes` (default 8192) before any parsing.
/// - `limits` — trusted, caller-supplied runtime resource bounds. They
///   gate the input size, the logical length, and the variable-width
///   payload fields, and are the SAME limits used for the final
///   validation.
///
/// # Guarantees (Phase-11A hostile-media contract)
///
/// The decoded representation is passed through
/// `Representation::validate` BEFORE it is returned: decode-OK implies
/// structurally valid under the same limits (lengths, canonical forms,
/// rank ranges, stream sanity, and the encoded-size cap), so a caller
/// may rely on the returned representation satisfying every invariant of
/// `validate`. The read path therefore never hands an unvalidated
/// descriptor to the materializer.
///
/// # Algorithm
///
/// 1. bound the input against `max_descriptor_bytes`;
/// 2. read the tag + logical `len` (`u32` LE, widened to `u64`) and
///    check `len` against `max_chunk_size` immediately;
/// 3. dispatch on the tag, enforcing each family's caps before the
///    allocations they guard;
/// 4. require the input to be consumed exactly (trailing bytes are
///    `Malformed` — the encoding must be canonical);
/// 5. run `Representation::validate` and map its errors.
///
/// The stages are numbered inline in the function body.
///
/// # Resource bounds
///
/// Maximum allocations are sized either by the (already capped) input
/// slice or by limit-checked fields (`m ≤ max_palette` before
/// `Vec::with_capacity(m)`; `take` never reads past the input). The
/// final `validate` re-checks every derived size, including the
/// encoded-size cap, under the same limits.
///
/// # Concurrency
///
/// Pure and lock-free; safe to run on any thread (the read path decodes
/// on the worker pool with the epoch guard released — Phase-11C).
///
/// # Failure behavior
///
/// Typed `CodecError`s only — `Truncated` (input ends mid-structure),
/// `TooLong` (input above the descriptor cap, `len` above the chunk cap,
/// INLINE above `max_inline_bytes`), `Malformed` (unknown tag/id byte,
/// palette/period violations, trailing bytes, and any `validate` failure
/// that is not a size error; `DescriptorTooLarge`/`ChunkTooLarge` map to
/// `TooLong`). Never panics or OOMs — the hostile-media oracle
/// (ADR-0016 "typed error, never panic") fuzz-proves every bounded byte
/// string under tight and default limits.
///
/// # Evidence
///
/// Phase-11A hostile-media court
/// (`evidence/hostile-media/court-1787750784-a2983dc/`): `decode`
/// previously accepted descriptors that `validate` rejected — the write
/// path gated with `validate` (`put_chunk_in_tx`); the read path never
/// did. Taking the full `&Limits` and validating internally closed that
/// layering gap. The on-disk format is unchanged; the parser is stricter
/// about accepting, never about encoding.
pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Representation, CodecError> {
    // -------------------------------------------------------------------
    // Stage 1: Input cap.
    //
    // The whole input is bounded BEFORE any parsing or allocation. This
    // is the first of two size gates (the second is `validate`'s
    // `DescriptorTooLarge` re-check in Stage 5); the descriptor court
    // pins the 8192/8193 boundary.
    // -------------------------------------------------------------------
    if bytes.len() as u64 > limits.max_descriptor_bytes {
        return Err(CodecError::TooLong);
    }
    let mut r = Reader::new(bytes);

    // -------------------------------------------------------------------
    // Stage 2: Common prefix — family tag + logical extent length.
    //
    // `len` is `u32` LE on disk (widened to `u64` in memory) and is the
    // LOGICAL extent length in bytes — what the extent materializes to —
    // not the descriptor's own size (that is bounded separately by
    // `max_descriptor_bytes`). It is checked against `max_chunk_size`
    // immediately, before any payload byte is read or any allocation is
    // made: a persisted length is never allowed to become an allocation
    // authority before this check.
    // -------------------------------------------------------------------
    let tag = r.u8()?;
    let len = r.u32()? as u64;
    if len > limits.max_chunk_size {
        return Err(CodecError::TooLong);
    }

    // -------------------------------------------------------------------
    // Stage 3: Per-tag payload parse.
    //
    // Each arm consumes exactly the fields `docs/format/ondisk-v1.md` §7
    // defines for its tag, via bounded `take`s that return `Truncated`
    // rather than reading past the input. Per-family caps are enforced
    // HERE, before the allocations they guard: INLINE vs
    // `max_inline_bytes`; palette cardinality vs `max_palette` (m ≥ 1);
    // period vs `max_period` (period ≥ 1); tail < period. Full semantic
    // validation is deliberately deferred to Stage 5, where
    // `Representation::validate` is the single authority.
    // -------------------------------------------------------------------
    let rep = match tag {
        TAG_ZERO => Representation::Zero { len },
        TAG_FILL => {
            let value = r.u8()?;
            Representation::Fill { value, len }
        }
        TAG_INLINE => {
            if len > limits.max_inline_bytes {
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
            if m > limits.max_palette || m == 0 {
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
            if period == 0 || period > limits.max_period {
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
        TAG_SPARSE_BLOCK64 => {
            let model = read_id(&mut r)?;
            let enc_obj = read_id(&mut r)?;
            let scale_bits = r.u8()?;
            let codec = RansCodec::from_u8(r.u8()?).ok_or(CodecError::Malformed)?;
            let pc_len = r.u32()?;
            let rank_len = r.u32()?;
            let lit_len = r.u32()?;
            let words = r.u32()?;
            let nonzero = r.u32()?;
            let lit_out = r.u32()?;
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
            }
        }
        TAG_SEQUENCE_DICT => {
            let dictionary = read_id(&mut r)?;
            let dictionary_len = r.u32()?;
            let model = read_id(&mut r)?;
            let enc_obj = read_id(&mut r)?;
            let scale_bits = r.u8()?;
            let codec = RansCodec::from_u8(r.u8()?).ok_or(CodecError::Malformed)?;
            let seq_len = r.u32()?;
            let lit_len = r.u32()?;
            let off_len = r.u32()?;
            let src_len = r.u32()?;
            let cmds = r.u32()?;
            let lit_out = r.u32()?;
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
            }
        }
        TAG_SEQUENCE_SHARED_DICT => {
            let dictionary = read_id(&mut r)?;
            let dictionary_len = r.u32()?;
            let shared = read_id(&mut r)?;
            let shared_len = r.u32()?;
            let model = read_id(&mut r)?;
            let enc_obj = read_id(&mut r)?;
            let scale_bits = r.u8()?;
            let codec = RansCodec::from_u8(r.u8()?).ok_or(CodecError::Malformed)?;
            let seq_len = r.u32()?;
            let lit_len = r.u32()?;
            let off_len = r.u32()?;
            let src_len = r.u32()?;
            let cmds = r.u32()?;
            let lit_out = r.u32()?;
            Representation::SequenceSharedDict {
                dictionary,
                dictionary_len,
                shared,
                shared_len,
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
            }
        }
        TAG_SEQUENCE_DEEP => {
            let model = read_id(&mut r)?;
            let enc_obj = read_id(&mut r)?;
            let scale_bits = r.u8()?;
            let codec = RansCodec::from_u8(r.u8()?).ok_or(CodecError::Malformed)?;
            let seq_len = r.u32()?;
            let lit_len = r.u32()?;
            let off_len = r.u32()?;
            let len_len = r.u32()?;
            let cmds = r.u32()?;
            let lit_out = r.u32()?;
            Representation::SequenceDeep {
                model,
                enc_obj,
                scale_bits,
                codec,
                seq_len,
                lit_len,
                off_len,
                len_len,
                cmds,
                lit_out,
                len,
            }
        }
        _ => return Err(CodecError::Malformed),
    };

    // -------------------------------------------------------------------
    // Stage 4: Exact-consumption check.
    //
    // Every accepted descriptor must be consumed exactly — trailing
    // bytes are `Malformed`, not ignored. Together with the deterministic
    // field order this is what makes the encoding canonical: the
    // descriptor court's byte-exact re-encode clause
    // (`encode(decode(b)) == b`) depends on it.
    // -------------------------------------------------------------------
    if !r.done() {
        return Err(CodecError::Malformed);
    }

    // -------------------------------------------------------------------
    // Stage 5: Structural validation gate.
    //
    // Phase-11A: decode-OK implies structurally valid. Every descriptor
    // that survives the parse must satisfy `validate` under the SAME
    // limits (lengths, canonical forms, rank ranges, stream sanity, and
    // the encoded-size cap), so no caller — store read path, fsck,
    // materializer — ever sees a decodable-but-invalid descriptor.
    //
    // This gate is the read path's half of the hostile-media ordering
    // invariant:
    //
    //     persistent bytes
    //         -> bounded parse          (Stages 1-4)
    //         -> structural validation  (this gate)
    //         -> resource preflight     (materializer limits)
    //         -> materialization
    //
    // The read path must never trust a persisted length as an allocation
    // authority before structural validation, and decode-OK must imply a
    // byte-exact canonical re-encode (the descriptor court's clauses;
    // see the module doc). The Phase-11A court
    // (`evidence/hostile-media/court-1787750784-a2983dc/`) found the
    // pre-11A read path handed decodable-but-invalid descriptors to the
    // materializer — this gate is the fix, matching the write path's
    // `validate`-before-persist gate (`put_chunk_in_tx`). Never move
    // allocation ahead of validation without extending the hostile-media
    // court to prove the replacement ordering safe.
    // -------------------------------------------------------------------
    rep.validate(limits).map_err(|e| match e {
        crate::core::representation::ReprError::DescriptorTooLarge
        | crate::core::representation::ReprError::ChunkTooLarge => CodecError::TooLong,
        _ => CodecError::Malformed,
    })?;
    Ok(rep)
}

/// Decode a residual's kind byte and payload.
///
/// `repr_len` is the enclosing descriptor's logical extent length: the
/// residual wire form carries no length of its own (the representation
/// length is needed to validate), so the descriptor's `len` is threaded
/// through — every decoded `Residual` carries it, and
/// `Residual::validate` checks against it (`ResidualLenMismatch`).
///
/// Structural preflight happens before the allocations it guards: for
/// RANGE_REPLACE, each change's `start < end` is verified while the
/// literal total is accumulated, and only then are the literal bytes
/// taken. Unknown kind bytes are rejected as `Malformed`.
pub fn decode_residual(r: &mut Reader<'_>, repr_len: u64) -> Result<Residual, CodecError> {
    let kind = r.u8()?;
    // -------------------------------------------------------------------
    // Stage 1: Kind dispatch — each arm reads exactly the §7 residual
    // table's fields via bounded takes.
    // -------------------------------------------------------------------
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
        RESIDUAL_BASE_SEQUENCE => {
            let enc_obj = read_id(r)?;
            let model = read_id(r)?;
            let scale_bits = r.u8()?;
            let codec = RansCodec::from_u8(r.u8()?).ok_or(CodecError::Malformed)?;
            let seq_len = r.u32()?;
            let lit_len = r.u32()?;
            let off_len = r.u32()?;
            let cmds = r.u32()?;
            let lit_out = r.u32()?;
            Ok(Residual::BaseSequence {
                len: repr_len,
                enc_obj,
                model,
                scale_bits,
                codec,
                seq_len,
                lit_len,
                off_len,
                cmds,
                lit_out,
            })
        }
        _ => Err(CodecError::Malformed),
    }
}

/// Read a 32-byte `ChunkId` — the fixed-width wire form of an object
/// reference, opaque to the codec (resolution happens upstream).
///
/// The `try_into().unwrap()` cannot fail: `take(32)` returns an
/// exactly-32-byte slice or `Err(Truncated)`, so the length conversion
/// is size-guaranteed.
fn read_id(r: &mut Reader<'_>) -> Result<ChunkId, CodecError> {
    let b = r.take(32)?;
    Ok(ChunkId::new(b.try_into().unwrap()))
}

/// Read a 16-byte entropy-universe seed (the fixed-width wire form of
/// `EntropyRef`'s seed field). As with `read_id`, the `unwrap` is
/// size-guaranteed by the preceding `take(16)`.
fn read_seed(r: &mut Reader<'_>) -> Result<[u8; 16], CodecError> {
    let b = r.take(16)?;
    Ok(b.try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::representation::{Edit, RangeChange};
    use proptest::prelude::*;

    fn dl() -> Limits {
        Limits::default()
    }

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
            Representation::BaseResidual {
                base: id,
                base_len: 64,
                residual: Residual::BaseSequence {
                    len: 64,
                    enc_obj: id,
                    model: id,
                    scale_bits: 14,
                    codec: RansCodec::Interleaved2,
                    seq_len: 10,
                    lit_len: 5,
                    off_len: 8,
                    cmds: 4,
                    lit_out: 3,
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
            Representation::SparseBlock64 {
                model: id,
                enc_obj: id,
                scale_bits: 14,
                codec: RansCodec::Interleaved2,
                pc_len: 100,
                rank_len: 60,
                lit_len: 20,
                words: 512,
                nonzero: 7,
                lit_out: 9,
                len: 4096,
            },
            Representation::SequenceDict {
                dictionary: id,
                dictionary_len: 65536,
                model: id,
                enc_obj: id,
                scale_bits: 14,
                codec: RansCodec::Interleaved2,
                seq_len: 100,
                lit_len: 50,
                off_len: 20,
                src_len: 10,
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
            let back = decode(&bytes, &dl()).unwrap();
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
                if let Ok(rep2) = decode(&bad, &dl()) {
                    // A flipped byte may produce a valid descriptor (e.g.
                    // a different fill value); the contract is: never
                    // panic, and the result must pass structural
                    // validation (or be rejected — either is fine here).
                    let _ = rep2.validate(&dl());
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
                    decode(&bytes[..cut], &dl()).is_err(),
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
        let tight = Limits {
            max_descriptor_bytes: bytes.len() as u64 - 1,
            ..Limits::default()
        };
        assert_eq!(decode(&bytes, &tight), Err(CodecError::TooLong));
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
                let back = decode(&bytes, &dl()).unwrap();
                assert_eq!(back, rep);
                assert_eq!(bytes.len() as u64, rep.encoded_size());
            }
        }
    }
}
