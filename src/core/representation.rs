//! Representation descriptors and the residual algebra.
//!
//! The defining equation: `X = Materialize(D)` where `X` is the exact
//! logical byte sequence and `D` is the persisted representation descriptor.
//! This module defines `D` (in-memory form) and the exact, bounded,
//! non-Turing-complete descriptor language (ADR-0005).

#![forbid(unsafe_code)]

use crate::core::extent::ChunkId;

/// rANS codec variants supported in v1.
///
/// All codecs share the upstream bitstream contract
/// (`docs/theory/rans-state.md`); the scalar paths are the authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RansCodec {
    /// Single-state byte rANS.
    Single = 0,
    /// Two-state interleaved byte rANS.
    Interleaved2 = 1,
}

impl RansCodec {
    /// Decode the persisted codec tag.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Single),
            1 => Some(Self::Interleaved2),
            _ => None,
        }
    }

    /// Persisted tag.
    pub const fn tag(self) -> u8 {
        self as u8
    }
}

/// Entropy universe identifiers (registry is part of the format).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum UniverseId {
    /// Uniform XOF v1 — deterministic BLAKE3-based expander. This is the
    /// Phase-1 **negative control** universe (ADR-0005): it establishes
    /// that a random implicit dictionary does not create free compression
    /// once selector cost is included.
    UniformXofV1 = 0x01,
}

impl UniverseId {
    /// Decode a persisted universe id.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::UniformXofV1),
            _ => None,
        }
    }

    /// Persisted tag.
    pub const fn tag(self) -> u8 {
        self as u8
    }
}

/// Bounded deterministic reversible transform identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TransformId {
    /// Identity.
    Identity = 0x00,
}

impl TransformId {
    /// Decode a persisted transform id.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x00 => Some(Self::Identity),
            _ => None,
        }
    }

    /// Persisted tag.
    pub const fn tag(self) -> u8 {
        self as u8
    }
}

/// One edited position for [`Residual::XorSparse`]: byte at `pos` of the
/// target equals `base[pos] ^ val`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edit {
    /// Position within the residual (0-based).
    pub pos: u32,
    /// XOR difference value.
    pub val: u8,
}

/// One changed range for [`Residual::RangeReplace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeChange {
    /// Inclusive start of the replaced range.
    pub start: u32,
    /// Exclusive end of the replaced range.
    pub end: u32,
}

/// Exact residual forms for base+residual and entropy+residual
/// representations (`docs/adr/0005-representation-set.md`).
///
/// Semantics: for target `X`, base `B` (both of length `len`):
///
/// - `XorSparse`: `X[i] = B[i] ^ val` at edit positions; `X[i] = B[i]`
///   elsewhere.
/// - `RangeReplace`: `X[start..end] = literals` in order; elsewhere
///   `X[i] = B[i]`.
/// - `RansCoded`: the encoded stream decodes to `decoded_len` bytes `D`;
///   `X[i] = B[i] ^ D[i]`.
/// - `BaseSequence`: the output `X` is built by walking a command stream
///   — COPY(base_offset, len) copies from the base, LITERAL(run) appends
///   literal bytes. Shift-aware: inserted/deleted regions do not break it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Residual {
    /// Sparse XOR edit set.
    XorSparse {
        /// Length of the residual in bytes (== chunk length).
        len: u64,
        /// Sorted, non-overlapping edits (positions strictly increasing).
        edits: Vec<Edit>,
    },
    /// Non-overlapping replaced ranges.
    RangeReplace {
        /// Length of the residual in bytes.
        len: u64,
        /// Sorted, non-overlapping changes.
        changes: Vec<RangeChange>,
        /// Concatenated replacement literals (total == Σ(end−start)).
        literals: Vec<u8>,
    },
    /// rANS-coded XOR difference stream.
    RansCoded {
        /// Length of the residual in bytes (== chunk length).
        len: u64,
        /// Content id of the encoded stream object.
        enc_obj: ChunkId,
        /// Content id of the rANS model object.
        model: ChunkId,
        /// Model scale bits.
        scale_bits: u8,
        /// Codec used for the stream.
        codec: RansCodec,
        /// Decoded stream length.
        decoded_len: u64,
    },
    /// Shift-aware copy/literal delta against the base (Phase-8 §5).
    ///
    /// Command stream (one byte per command): `0x00..=0x7F` is a literal
    /// run of `b + 1` (1..=128) bytes from the literal stream;
    /// `0x80..=0xFF` is a copy of `b - 0x80 + 4` (4..=131) bytes from the
    /// base at a u32 LE base offset (next 4 bytes of the offset stream).
    /// The output is built by appending: literals verbatim, copies from
    /// `base[off..off+len]` (validated against the base length).
    BaseSequence {
        /// Length of the residual in bytes (== chunk length).
        len: u64,
        /// Content id of the encoded object (3 concatenated streams).
        enc_obj: ChunkId,
        /// Content id of the model object (3 slots, same codec as
        /// SEQUENCE_RANS).
        model: ChunkId,
        /// Model scale bits.
        scale_bits: u8,
        /// Codec used for the streams.
        codec: RansCodec,
        /// Encoded command-stream length.
        seq_len: u32,
        /// Encoded literal-stream length.
        lit_len: u32,
        /// Encoded offset-stream length.
        off_len: u32,
        /// Decoded command count.
        cmds: u32,
        /// Decoded literal byte count.
        lit_out: u32,
    },
}

/// The representation descriptor set, v1 (ADR-0005).
///
/// Every variant's `len` is the exact materialized output length. All
/// arithmetic on these values is checked at parse and materialization time;
/// a malformed descriptor yields a typed error, never a panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Representation {
    /// All-zero extent.
    Zero {
        /// Materialized length in bytes.
        len: u64,
    },
    /// Single repeated byte.
    Fill {
        /// The repeated byte value.
        value: u8,
        /// Materialized length in bytes.
        len: u64,
    },
    /// Short literal bytes stored inside the descriptor.
    Inline {
        /// The literal bytes.
        data: Vec<u8>,
    },
    /// Literal bytes stored as an object.
    Raw {
        /// Content id of the literal-bytes object.
        obj: ChunkId,
        /// Materialized length in bytes.
        len: u64,
    },
    /// rANS-encoded stream with a persisted model.
    Rans {
        /// Content id of the model object.
        model: ChunkId,
        /// Content id of the encoded stream object.
        enc_obj: ChunkId,
        /// Model scale bits.
        scale_bits: u8,
        /// Codec used for the stream.
        codec: RansCodec,
        /// Materialized length.
        len: u64,
    },
    /// Exact sub-range reference into an existing logical chunk.
    ExactRef {
        /// Target chunk content id (its descriptor resolves via the store's
        /// chunk index).
        target: ChunkId,
        /// Offset into the target chunk.
        off: u64,
        /// Referenced length.
        len: u64,
    },
    /// Base chunk plus exact residual.
    BaseResidual {
        /// Base chunk content id.
        base: ChunkId,
        /// Materialized length of the base chunk (must be >= `len`).
        base_len: u64,
        /// Residual.
        residual: Residual,
        /// Materialized length.
        len: u64,
    },
    /// Combinatorial sparse configuration: `k` marked positions among `len`,
    /// position subset encoded as combination rank, values as literals.
    Sparse {
        /// Number of marked positions.
        k: u32,
        /// Combination rank in `[0, C(len, k))`.
        rank: u128,
        /// Literal value at each marked position (k bytes).
        literals: Vec<u8>,
        /// Materialized length.
        len: u64,
    },
    /// Low-cardinality palette configuration: `m ≤ 16` symbols with counts,
    /// multinomial rank over `n!/(∏c!)`.
    Palette {
        /// Palette symbols (distinct bytes).
        palette: Vec<u8>,
        /// Multiplicity of each palette symbol (sums to `len`).
        counts: Vec<u32>,
        /// Multinomial rank in `[0, n!/(∏c!))`.
        rank: u128,
        /// Materialized length.
        len: u64,
    },
    /// Periodic structure: pattern repeated `count` times plus tail.
    Periodic {
        /// Pattern length.
        period: u32,
        /// Pattern bytes.
        pattern: Vec<u8>,
        /// Number of full repetitions.
        count: u32,
        /// Tail bytes (length `tail_len`, `0 <= tail_len < period`).
        tail: Vec<u8>,
        /// Materialized length (= period*count + tail.len()).
        len: u64,
    },
    /// Permutation of `m ≤ 34` distinct bytes, encoded by factoradic rank
    /// over the sorted distinct symbols.
    Permutation {
        /// Factoradic rank in `[0, m!)`.
        rank: u128,
        /// The sorted distinct symbols (length == m == len).
        alphabet: Vec<u8>,
        /// Materialized length (== m, ≤ 34).
        len: u64,
    },
    /// Entropy universe reference: `X = T(E(U, S, P)) ⊕ R`.
    EntropyRef {
        /// Universe.
        universe: UniverseId,
        /// Seed/state.
        seed: [u8; 16],
        /// Coordinate.
        coordinate: u64,
        /// Transform.
        transform: TransformId,
        /// Exact residual (may be empty for exact matches).
        residual: Residual,
        /// Materialized length.
        len: u64,
    },
    /// Local match coding + entropy: LZ77-style COPY/LITERAL token streams
    /// entropy-coded with ryg-rans-rs (the general-purpose compression
    /// floor, Phase-8 directive §4).
    ///
    /// Three byte streams, each rANS-coded:
    ///
    /// - *commands*: one byte per command. `0x00..=0x7F` is a literal run
    ///   of length `b + 1` (1..=128); `0x80..=0xFF` is a copy of length
    ///   `b - 0x80 + 4` (4..=131) whose offset (u16 LE) follows in the
    ///   offset stream.
    /// - *literals*: the literal-run bytes, in command order.
    /// - *offsets*: one u16 LE offset per copy command.
    ///
    /// The `model` object holds the three models length-prefixed; the
    /// `enc_obj` holds the three encoded streams concatenated (lengths in
    /// the descriptor). Copy offsets are relative to the current output
    /// position (local history), so the decoder needs only the decoded
    /// output buffer.
    SequenceRans {
        /// Content id of the model object (3 length-prefixed models).
        model: ChunkId,
        /// Content id of the encoded object (3 concatenated streams).
        enc_obj: ChunkId,
        /// Model scale bits (shared by all three streams).
        scale_bits: u8,
        /// Codec used for the streams.
        codec: RansCodec,
        /// Encoded command-stream length (bytes).
        seq_len: u32,
        /// Encoded literal-stream length (bytes).
        lit_len: u32,
        /// Encoded offset-stream length (bytes).
        off_len: u32,
        /// Command count (= decoded command-stream length).
        cmds: u32,
        /// Decoded literal-stream length (total literal bytes).
        lit_out: u32,
        /// Materialized length.
        len: u64,
    },
    /// Blockwise-64 enumerative sparse coding: the chunk's nonzero-byte
    /// positions are coded as 64-bit subblocks — per 64-bit word, its
    /// popcount `k` and the subset rank among `C(64, k)` (which fits a
    /// u64 for every `k`), plus the literal byte values. The three streams
    /// (popcounts, ranks, literals) use the same rANS/raw codec as
    /// SEQUENCE_RANS. This removes the `u128` combination-rank cliff of
    /// SPARSE (which cannot represent `10 <= k <= n-10` for 64 KiB
    /// chunks) while staying bounded, SIMD/popcount-friendly, and
    /// random-accessible per word (Phase-8 directive §6; ADR-0005).
    SparseBlock64 {
        /// Content id of the model object (3 slots).
        model: ChunkId,
        /// Content id of the encoded object (3 concatenated streams).
        enc_obj: ChunkId,
        /// Model scale bits.
        scale_bits: u8,
        /// Codec used for the streams.
        codec: RansCodec,
        /// Encoded popcount-stream length.
        pc_len: u32,
        /// Encoded rank-stream length.
        rank_len: u32,
        /// Encoded literal-stream length.
        lit_len: u32,
        /// Number of 64-bit words (= ceil(len / 8)).
        words: u32,
        /// Number of nonzero words (= decoded rank-stream entries).
        nonzero: u32,
        /// Decoded literal byte count (= total marked bytes).
        lit_out: u32,
        /// Materialized length.
        len: u64,
    },
    /// Cross-chunk dictionary match coding (Phase-9B; ADR-0005): the
    /// SEQUENCE_RANS command semantics plus a fourth *copy-source* stream
    /// (one byte per copy: `SRC_LOCAL` = the u16 value is a backward
    /// distance in the already-materialized output, `SRC_DICT` = it is an
    /// absolute offset into the dictionary chunk). The dictionary is the
    /// previous same-file chunk (v1); it is a content-addressed chunk
    /// reference, so its own persisted state is accounted where it is
    /// materialized, and the reference depth (dictionary chain + 1) is
    /// capped by `max_reference_depth` so cross-chunk dictionary chains
    /// never defeat bounded random access.
    SequenceDict {
        /// Content id of the dictionary chunk.
        dictionary: ChunkId,
        /// Materialized length of the dictionary chunk (≤ 64 KiB; u16
        /// DICT offsets).
        dictionary_len: u32,
        /// Content id of the model object (4 length-prefixed slots).
        model: ChunkId,
        /// Content id of the encoded object (4 concatenated streams:
        /// commands, literals, offsets, copy sources).
        enc_obj: ChunkId,
        /// Model scale bits (shared by all four streams).
        scale_bits: u8,
        /// Codec used for the streams.
        codec: RansCodec,
        /// Encoded command-stream length (bytes).
        seq_len: u32,
        /// Encoded literal-stream length (bytes).
        lit_len: u32,
        /// Encoded offset-stream length (bytes).
        off_len: u32,
        /// Encoded copy-source-stream length (bytes).
        src_len: u32,
        /// Command count (= decoded command-stream length).
        cmds: u32,
        /// Decoded literal-stream length (total literal bytes).
        lit_out: u32,
        /// Materialized length.
        len: u64,
    },
    /// Shared amortized dictionary match coding (Phase-9C; ADR-0005): the
    /// SEQUENCE_RANS command semantics with a fourth *copy-source* stream
    /// whose per-copy byte selects among `SRC_LOCAL` (the u16 value is a
    /// backward distance in the already-materialized output), `SRC_DICT`
    /// (absolute offset into the previous same-file chunk, when present),
    /// and `SRC_SHARED` (absolute offset into a shared cross-file
    /// dictionary chunk). The shared dictionary is a content-addressed
    /// chunk chosen by the background optimizer to amortize structure
    /// common to a file family/directory; it is persisted state (its own
    /// object/chunk is accounted where it is materialized) and its
    /// reference depth is capped by `max_reference_depth` like every other
    /// reference family. `dictionary` may be ZERO (= no file dictionary;
    /// the shared dictionary is then the only external source).
    SequenceSharedDict {
        /// Content id of the previous same-file chunk (ZERO = absent).
        dictionary: ChunkId,
        /// Materialized length of the file dictionary (0 when absent).
        dictionary_len: u32,
        /// Content id of the shared cross-file dictionary chunk (never
        /// ZERO; ≤ 64 KiB so u16 offsets bound it).
        shared: ChunkId,
        /// Materialized length of the shared dictionary.
        shared_len: u32,
        /// Content id of the model object (4 length-prefixed slots).
        model: ChunkId,
        /// Content id of the encoded object (4 concatenated streams:
        /// commands, literals, offsets, copy sources).
        enc_obj: ChunkId,
        /// Model scale bits (shared by all four streams).
        scale_bits: u8,
        /// Codec used for the streams.
        codec: RansCodec,
        /// Encoded command-stream length (bytes).
        seq_len: u32,
        /// Encoded literal-stream length (bytes).
        lit_len: u32,
        /// Encoded offset-stream length (bytes).
        off_len: u32,
        /// Encoded copy-source-stream length (bytes).
        src_len: u32,
        /// Command count (= decoded command-stream length).
        cmds: u32,
        /// Decoded literal-stream length (total literal bytes).
        lit_out: u32,
        /// Materialized length.
        len: u64,
    },
}

impl Representation {
    /// The exact materialized output length of this descriptor.
    pub const fn len(&self) -> u64 {
        match self {
            Representation::Zero { len }
            | Representation::Fill { len, .. }
            | Representation::Raw { len, .. }
            | Representation::Rans { len, .. }
            | Representation::ExactRef { len, .. }
            | Representation::BaseResidual { len, .. }
            | Representation::Sparse { len, .. }
            | Representation::Palette { len, .. }
            | Representation::Periodic { len, .. }
            | Representation::EntropyRef { len, .. }
            | Representation::Permutation { len, .. }
            | Representation::SequenceRans { len, .. }
            | Representation::SparseBlock64 { len, .. }
            | Representation::SequenceDict { len, .. }
            | Representation::SequenceSharedDict { len, .. } => *len,
            Representation::Inline { data } => data.len() as u64,
        }
    }

    /// True for zero-length output (only legal for len 0 representations).
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The persistence tag (mirrors `format/descriptor.rs`).
    pub const fn tag(&self) -> u8 {
        match self {
            Representation::Zero { .. } => 0x01,
            Representation::Fill { .. } => 0x02,
            Representation::Raw { .. } => 0x03,
            Representation::Rans { .. } => 0x04,
            Representation::ExactRef { .. } => 0x05,
            Representation::BaseResidual { .. } => 0x06,
            Representation::Sparse { .. } => 0x07,
            Representation::Palette { .. } => 0x08,
            Representation::Periodic { .. } => 0x09,
            Representation::EntropyRef { .. } => 0x0A,
            Representation::Inline { .. } => 0x0B,
            Representation::Permutation { .. } => 0x0C,
            Representation::SequenceRans { .. } => 0x0D,
            Representation::SparseBlock64 { .. } => 0x0E,
            Representation::SequenceDict { .. } => 0x0F,
            Representation::SequenceSharedDict { .. } => 0x10,
        }
    }

    /// A human-readable family name (for `explain`/`inspect`).
    pub const fn family(&self) -> &'static str {
        match self {
            Representation::Zero { .. } => "ZERO",
            Representation::Fill { .. } => "FILL",
            Representation::Raw { .. } => "RAW",
            Representation::Rans { .. } => "RANS",
            Representation::ExactRef { .. } => "EXACT_REF",
            Representation::BaseResidual { .. } => "BASE_RESIDUAL",
            Representation::Sparse { .. } => "SPARSE",
            Representation::Palette { .. } => "PALETTE",
            Representation::Periodic { .. } => "PERIODIC",
            Representation::EntropyRef { .. } => "ENTROPY_REF",
            Representation::Inline { .. } => "INLINE",
            Representation::Permutation { .. } => "PERMUTATION",
            Representation::SequenceRans { .. } => "SEQUENCE_RANS",
            Representation::SparseBlock64 { .. } => "SPARSE_BLOCK64",
            Representation::SequenceDict { .. } => "SEQUENCE_DICT",
            Representation::SequenceSharedDict { .. } => "SEQUENCE_SHARED_DICT",
        }
    }

    /// Exact encoded descriptor size in bytes, mirroring
    /// `format::descriptor` sizing rules.
    ///
    /// A test in `src/tests/` asserts this equals the real encoder output
    /// length for random descriptors, keeping the mirror in sync.
    pub fn encoded_size(&self) -> u64 {
        // common prefix: tag (1) + len (4)
        let base = 5u64;
        let payload: u64 = match self {
            Representation::Zero { .. } => 0,
            Representation::Fill { .. } => 1,
            Representation::Inline { data } => data.len() as u64,
            Representation::Raw { .. } => 32,
            Representation::Rans { .. } => 32 + 32 + 1 + 1,
            Representation::ExactRef { .. } => 32 + 4,
            Representation::BaseResidual { residual, .. } => 32 + 4 + residual.encoded_size(),
            Representation::Sparse { literals, .. } => 4 + 16 + literals.len() as u64,
            Representation::Palette {
                palette, counts, ..
            } => 1 + palette.len() as u64 + 4 * counts.len() as u64 + 16,
            Representation::Periodic {
                period,
                pattern: _,
                tail,
                ..
            } => 4 + *period as u64 + 4 + 4 + tail.len() as u64,
            Representation::EntropyRef { residual, .. } => 1 + 16 + 8 + 1 + residual.encoded_size(),
            Representation::Permutation { alphabet, .. } => 16 + alphabet.len() as u64,
            Representation::SequenceRans { .. } => 32 + 32 + 1 + 1 + 4 + 4 + 4 + 4 + 4,
            Representation::SparseBlock64 { .. } => 32 + 32 + 1 + 1 + 4 + 4 + 4 + 4 + 4 + 4,
            // dictionary id + dictionary_len + model + enc + scale + codec
            // + seq/lit/off/src/cmds/lit_out.
            Representation::SequenceDict { .. } => 32 + 4 + 32 + 32 + 1 + 1 + 4 + 4 + 4 + 4 + 4 + 4,
            // file dict id + file dict len + shared id + shared len + model
            // + enc + scale + codec + seq/lit/off/src/cmds/lit_out.
            Representation::SequenceSharedDict { .. } => {
                32 + 4 + 32 + 4 + 32 + 32 + 1 + 1 + 4 + 4 + 4 + 4 + 4 + 4
            }
        };
        base + payload
    }

    /// Validate structural invariants that do not require external
    /// resolution: lengths, palette consistency, periodic arithmetic,
    /// inline size, reference sanity, and the encoded descriptor size
    /// (a descriptor that exceeds `max_descriptor_bytes` could win on raw
    /// byte cost yet be undecodable — every persisted descriptor must
    /// decode).
    pub fn validate(&self, limits: &crate::core::limits::Limits) -> Result<(), ReprError> {
        if self.encoded_size() > limits.max_descriptor_bytes {
            return Err(ReprError::DescriptorTooLarge);
        }
        match self {
            Representation::Zero { len } => {
                check_len(*len, limits)?;
            }
            Representation::Fill { len, .. } => {
                check_len(*len, limits)?;
            }
            Representation::Inline { data } => {
                if data.len() as u64 > limits.max_inline_bytes {
                    return Err(ReprError::InlineTooLarge);
                }
            }
            Representation::Raw { obj, len } => {
                check_len(*len, limits)?;
                if obj.is_zero() {
                    return Err(ReprError::ZeroObjectId);
                }
            }
            Representation::Rans {
                model,
                enc_obj,
                scale_bits,
                len,
                ..
            } => {
                check_len(*len, limits)?;
                if model.is_zero() || enc_obj.is_zero() {
                    return Err(ReprError::ZeroObjectId);
                }
                if !(1..=16).contains(scale_bits) {
                    return Err(ReprError::BadScaleBits);
                }
            }
            Representation::ExactRef { target, off, len } => {
                check_len(*len, limits)?;
                if target.is_zero() {
                    return Err(ReprError::ZeroObjectId);
                }
                if off.checked_add(*len).is_none() {
                    return Err(ReprError::Overflow);
                }
            }
            Representation::BaseResidual {
                base,
                base_len,
                residual,
                len,
            } => {
                check_len(*len, limits)?;
                if base.is_zero() {
                    return Err(ReprError::ZeroObjectId);
                }
                // Copy/literal deltas (BaseSequence) may reference a base
                // shorter or longer than the target (insertions/deletions);
                // positional residuals require base >= target.
                if !matches!(residual, Residual::BaseSequence { .. }) && *base_len < *len {
                    return Err(ReprError::BaseTooShort);
                }
                residual.validate(*len, limits)?;
            }
            Representation::Sparse {
                k,
                rank,
                literals,
                len,
            } => {
                check_len(*len, limits)?;
                let k64 = *k as u64;
                if k64 > *len {
                    return Err(ReprError::SparseKTooLarge);
                }
                if literals.len() as u64 != k64 {
                    return Err(ReprError::SparseLiteralCount);
                }
                // rank must be < C(len, k)
                match crate::entropy::rank::comb(*len as u128, k64 as u128) {
                    Some(total) if *rank < total => {}
                    Some(_) => return Err(ReprError::SparseRankOutOfRange),
                    None => return Err(ReprError::CombOverflow),
                }
            }
            Representation::Palette {
                palette,
                counts,
                rank,
                len,
            } => {
                check_len(*len, limits)?;
                if palette.is_empty() || palette.len() > limits.max_palette {
                    return Err(ReprError::BadPalette);
                }
                if counts.len() != palette.len() {
                    return Err(ReprError::BadPalette);
                }
                let mut total: u64 = 0;
                for &c in counts.iter() {
                    total = total.checked_add(c as u64).ok_or(ReprError::Overflow)?;
                }
                if total != *len {
                    return Err(ReprError::PaletteCountsMismatch);
                }
                // Every symbol must have a nonzero count (canonical form).
                if counts.contains(&0) {
                    return Err(ReprError::BadPalette);
                }
                match crate::entropy::rank::multinomial(*len, counts) {
                    Some(total_states) if *rank < total_states => {}
                    Some(_) => return Err(ReprError::PaletteRankOutOfRange),
                    None => return Err(ReprError::CombOverflow),
                }
            }
            Representation::Periodic {
                period,
                pattern,
                count,
                tail,
                len,
            } => {
                check_len(*len, limits)?;
                if *period == 0 || *period as u64 > limits.max_period as u64 {
                    return Err(ReprError::BadPeriod);
                }
                if pattern.len() as u64 != *period as u64 {
                    return Err(ReprError::BadPeriod);
                }
                if tail.len() as u64 >= *period as u64 {
                    return Err(ReprError::BadTail);
                }
                let expected = (*period as u64)
                    .checked_mul(*count as u64)
                    .and_then(|v| v.checked_add(tail.len() as u64))
                    .ok_or(ReprError::Overflow)?;
                if expected != *len {
                    return Err(ReprError::PeriodicLenMismatch);
                }
            }
            Representation::EntropyRef {
                universe,
                seed: _,
                coordinate: _,
                transform,
                residual,
                len,
            } => {
                check_len(*len, limits)?;
                // Unknown universe/transform ids are typed errors (registry
                // part of the format, ADR compatibility rules).
                if *universe == crate::core::representation::UniverseId::UniformXofV1 {
                    // known
                } else {
                    return Err(ReprError::UnknownUniverse);
                }
                if *transform != crate::core::representation::TransformId::Identity {
                    return Err(ReprError::UnknownTransform);
                }
                residual.validate(*len, limits)?;
            }
            Representation::Permutation {
                rank,
                alphabet,
                len,
            } => {
                check_len(*len, limits)?;
                let m = *len;
                if m == 0 || m > 34 {
                    return Err(ReprError::PermutationSize);
                }
                if alphabet.len() as u64 != m {
                    return Err(ReprError::BadPermutationAlphabet);
                }
                // alphabet must be strictly increasing (canonical form).
                for w in alphabet.windows(2) {
                    if w[0] >= w[1] {
                        return Err(ReprError::BadPermutationAlphabet);
                    }
                }
                let total =
                    crate::entropy::rank::factorial(m as u128).ok_or(ReprError::CombOverflow)?;
                if *rank >= total {
                    return Err(ReprError::PermutationRankOutOfRange);
                }
            }
            Representation::SequenceRans {
                model,
                enc_obj,
                scale_bits,
                seq_len,
                lit_len,
                off_len,
                cmds,
                lit_out,
                len,
                ..
            } => {
                check_len(*len, limits)?;
                if model.is_zero() || enc_obj.is_zero() {
                    return Err(ReprError::ZeroObjectId);
                }
                if !(1..=16).contains(scale_bits) {
                    return Err(ReprError::BadScaleBits);
                }
                // Stream-length sanity: every byte is bounded by the chunk
                // class (streams cannot exceed the materialized size plus
                // a generous constant), and a copy needs at least one
                // command. `lit_out` must be <= len (literals are a subset
                // of the output).
                let max_stream = limits.max_chunk_size.saturating_add(64);
                for s in [*seq_len, *lit_len, *off_len] {
                    if s as u64 > max_stream {
                        return Err(ReprError::SequenceStreamTooLarge);
                    }
                }
                if (*lit_out as u64) > *len {
                    return Err(ReprError::SequenceLitOutMismatch);
                }
                if *cmds == 0 && *len > 0 {
                    return Err(ReprError::SequenceNoCommands);
                }
                // Every command writes at least one byte, so the command
                // count cannot exceed the output length (bounds the decode
                // allocation for the command stream).
                if (*cmds as u64) > *len {
                    return Err(ReprError::SequenceCmdsMismatch);
                }
            }
            Representation::SparseBlock64 {
                model,
                enc_obj,
                scale_bits,
                pc_len,
                rank_len,
                lit_len,
                words,
                nonzero,
                lit_out,
                len,
                ..
            } => {
                check_len(*len, limits)?;
                if model.is_zero() || enc_obj.is_zero() {
                    return Err(ReprError::ZeroObjectId);
                }
                if !(1..=16).contains(scale_bits) {
                    return Err(ReprError::BadScaleBits);
                }
                let max_stream = limits.max_chunk_size.saturating_add(64);
                for s in [*pc_len, *rank_len, *lit_len] {
                    if s as u64 > max_stream {
                        return Err(ReprError::SequenceStreamTooLarge);
                    }
                }
                // Word count must cover the output: words*8 >= len.
                if (*words as u64).saturating_mul(8) < *len {
                    return Err(ReprError::SparseBlockWords);
                }
                if (*nonzero as u64) > *words as u64 {
                    return Err(ReprError::SparseBlockWords);
                }
                if (*lit_out as u64) > *len {
                    return Err(ReprError::SequenceLitOutMismatch);
                }
                // Every marked byte carries a literal; the rank stream is
                // 8 bytes per nonzero word. Each nonzero word has >= 1
                // marked byte, so nonzero <= lit_out.
                if (*nonzero as u64) > *lit_out as u64 {
                    return Err(ReprError::SparseBlockLiteralCount);
                }
            }
            Representation::SequenceDict {
                dictionary,
                dictionary_len,
                model,
                enc_obj,
                scale_bits,
                seq_len,
                lit_len,
                off_len,
                src_len,
                cmds,
                lit_out,
                len,
                ..
            } => {
                check_len(*len, limits)?;
                if dictionary.is_zero() || model.is_zero() || enc_obj.is_zero() {
                    return Err(ReprError::ZeroObjectId);
                }
                if !(1..=16).contains(scale_bits) {
                    return Err(ReprError::BadScaleBits);
                }
                // DICT offsets are u16: the dictionary must be non-empty
                // and at most 64 KiB, and it is a logical chunk, so it
                // cannot exceed the chunk-class bound either.
                if *dictionary_len == 0
                    || *dictionary_len as u64 > crate::rans::sequence::MAX_DICT as u64
                    || *dictionary_len as u64 > limits.max_chunk_size
                {
                    return Err(ReprError::BadDictionary);
                }
                let max_stream = limits.max_chunk_size.saturating_add(64);
                for s in [*seq_len, *lit_len, *off_len, *src_len] {
                    if s as u64 > max_stream {
                        return Err(ReprError::SequenceStreamTooLarge);
                    }
                }
                if (*lit_out as u64) > *len {
                    return Err(ReprError::SequenceLitOutMismatch);
                }
                if *cmds == 0 && *len > 0 {
                    return Err(ReprError::SequenceNoCommands);
                }
                // Every command writes at least one byte, so the command
                // count cannot exceed the output length.
                if (*cmds as u64) > *len {
                    return Err(ReprError::SequenceCmdsMismatch);
                }
            }
            Representation::SequenceSharedDict {
                dictionary,
                dictionary_len,
                shared,
                shared_len,
                model,
                enc_obj,
                scale_bits,
                seq_len,
                lit_len,
                off_len,
                src_len,
                cmds,
                lit_out,
                len,
                ..
            } => {
                check_len(*len, limits)?;
                // The shared dictionary is mandatory; the file dictionary
                // is optional (ZERO id + zero length = absent).
                if shared.is_zero() || model.is_zero() || enc_obj.is_zero() {
                    return Err(ReprError::ZeroObjectId);
                }
                if !(1..=16).contains(scale_bits) {
                    return Err(ReprError::BadScaleBits);
                }
                if *shared_len == 0
                    || *shared_len as u64 > crate::rans::sequence::MAX_DICT as u64
                    || *shared_len as u64 > limits.max_chunk_size
                {
                    return Err(ReprError::BadDictionary);
                }
                // The optional file dictionary must be self-consistent.
                let file_absent = dictionary.is_zero() && *dictionary_len == 0;
                let file_present = !dictionary.is_zero()
                    && *dictionary_len > 0
                    && *dictionary_len as u64 <= crate::rans::sequence::MAX_DICT as u64
                    && *dictionary_len as u64 <= limits.max_chunk_size;
                if !file_absent && !file_present {
                    return Err(ReprError::BadDictionary);
                }
                let max_stream = limits.max_chunk_size.saturating_add(64);
                for s in [*seq_len, *lit_len, *off_len, *src_len] {
                    if s as u64 > max_stream {
                        return Err(ReprError::SequenceStreamTooLarge);
                    }
                }
                if (*lit_out as u64) > *len {
                    return Err(ReprError::SequenceLitOutMismatch);
                }
                if *cmds == 0 && *len > 0 {
                    return Err(ReprError::SequenceNoCommands);
                }
                if (*cmds as u64) > *len {
                    return Err(ReprError::SequenceCmdsMismatch);
                }
            }
        }
        Ok(())
    }
}

fn check_len(len: u64, limits: &crate::core::limits::Limits) -> Result<(), ReprError> {
    if len > limits.max_chunk_size {
        return Err(ReprError::ChunkTooLarge);
    }
    Ok(())
}

impl Residual {
    /// Length of the residual in bytes.
    pub const fn len(&self) -> u64 {
        match self {
            Residual::XorSparse { len, .. }
            | Residual::RangeReplace { len, .. }
            | Residual::RansCoded { len, .. }
            | Residual::BaseSequence { len, .. } => *len,
        }
    }

    /// Whether the residual covers zero bytes.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Encoded size in bytes, mirroring `format::descriptor` sizing.
    pub fn encoded_size(&self) -> u64 {
        match self {
            Residual::XorSparse { edits, .. } => 1 + 4 + 5 * edits.len() as u64,
            Residual::RangeReplace {
                changes, literals, ..
            } => 1 + 4 + 8 * changes.len() as u64 + literals.len() as u64,
            Residual::RansCoded { .. } => 1 + 32 + 32 + 1 + 1 + 4,
            Residual::BaseSequence { .. } => 1 + 32 + 32 + 1 + 1 + 4 + 4 + 4 + 4 + 4,
        }
    }

    /// Validate structural invariants against the representation length.
    pub fn validate(
        &self,
        repr_len: u64,
        limits: &crate::core::limits::Limits,
    ) -> Result<(), ReprError> {
        if self.len() != repr_len {
            return Err(ReprError::ResidualLenMismatch);
        }
        match self {
            Residual::XorSparse { edits, .. } => {
                if edits.len() as u64 > limits.max_fanout as u64 {
                    return Err(ReprError::FanoutTooLarge);
                }
                let mut prev: Option<u32> = None;
                for e in edits {
                    if e.pos as u64 >= repr_len {
                        return Err(ReprError::EditOutOfRange);
                    }
                    if let Some(p) = prev {
                        if e.pos <= p {
                            return Err(ReprError::EditsNotSorted);
                        }
                    }
                    prev = Some(e.pos);
                }
            }
            Residual::RangeReplace {
                changes, literals, ..
            } => {
                if changes.len() as u64 > limits.max_fanout as u64 {
                    return Err(ReprError::FanoutTooLarge);
                }
                let mut expected_lits: u64 = 0;
                let mut prev: Option<u32> = None;
                for c in changes {
                    if c.start >= c.end || c.end as u64 > repr_len {
                        return Err(ReprError::RangeOutOfRange);
                    }
                    if let Some(p) = prev {
                        if c.start <= p {
                            return Err(ReprError::RangesOverlap);
                        }
                    }
                    prev = Some(c.end);
                    expected_lits = expected_lits
                        .checked_add((c.end - c.start) as u64)
                        .ok_or(ReprError::Overflow)?;
                }
                if literals.len() as u64 != expected_lits {
                    return Err(ReprError::LiteralCountMismatch);
                }
            }
            Residual::RansCoded {
                enc_obj,
                model,
                scale_bits,
                decoded_len,
                ..
            } => {
                if enc_obj.is_zero() || model.is_zero() {
                    return Err(ReprError::ZeroObjectId);
                }
                if !(1..=16).contains(scale_bits) {
                    return Err(ReprError::BadScaleBits);
                }
                if *decoded_len != repr_len {
                    return Err(ReprError::ResidualLenMismatch);
                }
            }
            Residual::BaseSequence {
                enc_obj,
                model,
                scale_bits,
                seq_len,
                lit_len,
                off_len,
                cmds,
                lit_out,
                ..
            } => {
                if enc_obj.is_zero() || model.is_zero() {
                    return Err(ReprError::ZeroObjectId);
                }
                if !(1..=16).contains(scale_bits) {
                    return Err(ReprError::BadScaleBits);
                }
                // Stream-length sanity: bounded by the chunk class.
                let max_stream = limits.max_chunk_size.saturating_add(64);
                for s in [*seq_len, *lit_len, *off_len] {
                    if s as u64 > max_stream {
                        return Err(ReprError::SequenceStreamTooLarge);
                    }
                }
                // Literals are a subset of the output; every command
                // writes at least one byte, so the command count cannot
                // exceed the output length.
                if (*lit_out as u64) > repr_len {
                    return Err(ReprError::SequenceLitOutMismatch);
                }
                if *cmds == 0 && repr_len > 0 {
                    return Err(ReprError::SequenceNoCommands);
                }
                if (*cmds as u64) > repr_len {
                    return Err(ReprError::SequenceCmdsMismatch);
                }
            }
        }
        Ok(())
    }
}

/// Typed representation validation errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReprError {
    /// Logical length exceeds the format maximum.
    ChunkTooLarge,
    /// Zero content id where an object reference is required.
    ZeroObjectId,
    /// Scale bits outside 1..=16.
    BadScaleBits,
    /// Arithmetic overflow in a length/rank computation.
    Overflow,
    /// Base chunk shorter than the representation length.
    BaseTooShort,
    /// Residual length differs from representation length.
    ResidualLenMismatch,
    /// Edit position out of range.
    EditOutOfRange,
    /// Edits not strictly increasing.
    EditsNotSorted,
    /// Range out of range or degenerate.
    RangeOutOfRange,
    /// Ranges overlap or are not sorted.
    RangesOverlap,
    /// Literal byte count mismatch.
    LiteralCountMismatch,
    /// Too many edits/changes for the format limits.
    FanoutTooLarge,
    /// Sparse k exceeds length.
    SparseKTooLarge,
    /// Sparse literal count does not match k.
    SparseLiteralCount,
    /// Sparse rank out of range.
    SparseRankOutOfRange,
    /// Combination arithmetic overflowed u128 (candidate not representable).
    CombOverflow,
    /// Palette is empty, too large, or has zero-count symbols.
    BadPalette,
    /// Palette counts do not sum to the representation length.
    PaletteCountsMismatch,
    /// Palette rank out of range.
    PaletteRankOutOfRange,
    /// Invalid period or pattern length.
    BadPeriod,
    /// Tail length not < period.
    BadTail,
    /// Periodic arithmetic does not match declared length.
    PeriodicLenMismatch,
    /// INLINE exceeds the format limit.
    InlineTooLarge,
    /// Unknown universe id (registry is format-part).
    UnknownUniverse,
    /// Unknown transform id.
    UnknownTransform,
    /// Encoded descriptor exceeds the format limit.
    DescriptorTooLarge,
    /// Permutation length must be in 1..=34.
    PermutationSize,
    /// Permutation rank out of range.
    PermutationRankOutOfRange,
    /// Permutation alphabet must be strictly increasing with length == m.
    BadPermutationAlphabet,
    /// SEQUENCE_RANS encoded stream exceeds the format limit.
    SequenceStreamTooLarge,
    /// SEQUENCE_RANS literal output exceeds the materialized length.
    SequenceLitOutMismatch,
    /// SEQUENCE_RANS with no commands for a non-empty extent.
    SequenceNoCommands,
    /// SEQUENCE_RANS command count exceeds the materialized length.
    SequenceCmdsMismatch,
    /// SPARSE_BLOCK64 word count does not cover the output or exceeds the
    /// nonzero count.
    SparseBlockWords,
    /// SPARSE_BLOCK64 literal count is inconsistent with the marked bytes.
    SparseBlockLiteralCount,
    /// SEQUENCE_DICT dictionary length is zero, exceeds 64 KiB (u16 DICT
    /// offsets), or exceeds the chunk-class bound.
    BadDictionary,
}

impl std::fmt::Display for ReprError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ReprError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::limits::Limits;

    fn l() -> Limits {
        Limits::default()
    }

    #[test]
    fn zero_valid() {
        let r = Representation::Zero { len: 65536 };
        assert_eq!(r.len(), 65536);
        assert_eq!(r.tag(), 0x01);
        r.validate(&l()).unwrap();
    }

    #[test]
    fn zero_too_large_rejected() {
        let r = Representation::Zero { len: 1 << 40 };
        assert_eq!(r.validate(&l()), Err(ReprError::ChunkTooLarge));
    }

    #[test]
    fn periodic_validation() {
        // period 4, pattern "abcd", count 3, tail "xy" => len 14
        let r = Representation::Periodic {
            period: 4,
            pattern: b"abcd".to_vec(),
            count: 3,
            tail: b"xy".to_vec(),
            len: 14,
        };
        r.validate(&l()).unwrap();

        // wrong len
        let bad = Representation::Periodic {
            period: 4,
            pattern: b"abcd".to_vec(),
            count: 3,
            tail: b"xy".to_vec(),
            len: 15,
        };
        assert_eq!(bad.validate(&l()), Err(ReprError::PeriodicLenMismatch));
    }

    #[test]
    fn sparse_validation() {
        // n = 8, k = 3: C(8,3) = 56
        let r = Representation::Sparse {
            k: 3,
            rank: 55,
            literals: vec![1, 2, 3],
            len: 8,
        };
        r.validate(&l()).unwrap();
        let bad = Representation::Sparse {
            k: 3,
            rank: 56,
            literals: vec![1, 2, 3],
            len: 8,
        };
        assert_eq!(bad.validate(&l()), Err(ReprError::SparseRankOutOfRange));
    }

    #[test]
    fn residual_edits_sorted() {
        let res = Residual::XorSparse {
            len: 8,
            edits: vec![Edit { pos: 5, val: 1 }, Edit { pos: 3, val: 2 }],
        };
        assert_eq!(res.validate(8, &l()), Err(ReprError::EditsNotSorted));
    }
}
