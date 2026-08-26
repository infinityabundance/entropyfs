//! Entropy universes: versioned deterministic materialization functions.
//!
//! `E = Universe(version, seed/state, coordinate, requested_range)`.
//! A decoder with the descriptor alone must regenerate the same bytes
//! forever: no network, no hidden corpus, no RNG, no CPU-dependent
//! floating point, no nondeterministic threading. The universe
//! specification is part of the format version
//! (`docs/theory/entropy-medium.md`).
//!
//! v1 ships exactly one control universe: [`UniformXofV1`] — the negative
//! control establishing that a random implicit dictionary cannot create
//! free compression once selector cost is included (ADR-0005 §9).
//!
//! # PURPOSE
//!
//! Two things: (1) the versioned, deterministic materialization functions
//! ("universes") that generate bytes from a small persisted state, and
//! (2) the ENTROPY_REF candidate family (tag `0x0A`) that proposes
//! `universe + seed + coordinate + transform + residual` descriptors.
//!
//! # BOUNDARY
//!
//! Knows only `blake3`, the format's [`UniverseId`], and the residual
//! derivation machinery. It never touches the store and never searches
//! seeds — brute-force seed search over astronomical seed spaces is
//! prohibited (ADR-0005).
//!
//! # MODEL
//!
//! `E = Universe(version, seed/state, coordinate, requested_range)`. For
//! [`UniformXofV1`]:
//!
//! ```text
//! stream = concat_i BLAKE3(domain ‖ universe_id ‖ seed ‖ coordinate ‖ i)
//! ```
//!
//! each block 32 bytes (one BLAKE3-256 output); `materialize_range`
//! returns `stream[start..end]` by skipping within the first block and
//! then emitting whole blocks. The candidate encodes the target as
//! `X = transform(E(seed, coordinate)) ⊕ residual` — in v1 exactly the
//! identity transform plus an XOR_SPARSE residual.
//!
//! # PERSISTENT AUTHORITY
//!
//! Yes: the universe specification is part of the format version
//! (`docs/theory/entropy-medium.md`), and the descriptor persists
//! `universe_id, seed (16 bytes), coordinate (8 bytes), transform,
//! residual` (`docs/format/ondisk-v1.md`, tag `0x0A`). The XOF output is
//! *generated*, not stored — the stored entropy is the descriptor
//! (seed 128 bits + coordinate + residual), which is exactly why the
//! family cannot create free compression (entropy-medium.md §3).
//!
//! # CORRECTNESS INVARIANTS
//!
//! - deterministic: the same `(seed, coordinate, range)` always yields the
//!   same bytes (pinned by the `deterministic` and `range_matches_full`
//!   tests);
//! - the seed is derived deterministically from the chunk's content id
//!   (first 16 bytes), so the decoder needs nothing beyond the descriptor
//!   — and no seed search is ever performed;
//! - every residual byte is accounted; a candidate whose residual is not
//!   small loses honestly to RAW (the negative control);
//! - materialization is byte-exact — enforced by the §32 candidate
//!   validation gate.
//!
//! # CONCURRENCY
//!
//! Stateless encoder and pure materializer; safe to call from any thread.
//!
//! # RESOURCE BOUNDS
//!
//! `n ≤ max_chunk_size`; retained residuals are XOR_SPARSE with
//! `edits ≤ max_fanout`; materialization is `O(ceil(len/32))` BLAKE3
//! calls — at most 8192 blocks for a 256 KiB chunk. The encoder generates
//! the full stream once (n bytes) on the write path.
//!
//! # PERFORMANCE
//!
//! Blockwise materialization gives range reads: a sub-range is produced
//! from `⌊start/32⌋` blocks with a first-block skip, without generating
//! the whole stream. For random input the family self-limits (the diff
//! count is ~n, exceeding the fanout cap or the cost) so the write path
//! does not waste time on winning-less candidates.
//!
//! # FAILURE MODES
//!
//! An inverted range is an asserted programmer contract, not persistent
//! input. Otherwise: residuals that fail validation or exceed the fanout
//! cap are skipped; for arbitrary input the family simply produces no
//! winning candidate and RAW wins. Nothing here panics on persistent
//! data.
//!
//! # HISTORY / EVIDENCE
//!
//! ADR-0005 (the negative control; the prohibited seed search);
//! `docs/theory/entropy-medium.md` §3 (the `UniformXofV1` accounting);
//! ondisk-v1.md tag `0x0A`.

#![forbid(unsafe_code)]

use crate::core::candidate::{Candidate, CandidateContext, Encoder};
use crate::core::cost::ByteSplit;
use crate::core::representation::UniverseId;
use crate::core::representation::{Representation, Residual};
use crate::entropy::residual::{derive_residuals, residual_data_bytes};

/// Domain separation prefix for the v1 XOF universe (format-part).
const XOF_DOMAIN: &[u8] = b"ENTROPYFS-XOF-V1\0";
/// XOF block size: one BLAKE3-256 output (32 bytes) per counter value.
const XOF_BLOCK: u64 = 32;

/// `UniformXofV1`: a deterministic BLAKE3-based expander.
///
/// ```text
/// stream = concat_i BLAKE3(domain ‖ universe_id ‖ seed ‖ coordinate ‖ i)
/// ```
/// for `i = 0, 1, 2, ...`, each block 32 bytes (one BLAKE3-256 output).
/// `materialize_range` returns `stream[start..end]`.
///
/// This universe is a **negative control**: for arbitrary/random input the
/// residual is ~full size and RAW wins; it wins only when the input is
/// exactly (or almost exactly) the generated stream — honest, exact, and
/// rare. Brute-force seed search is prohibited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UniformXofV1;

impl UniformXofV1 {
    /// Universe id.
    pub const ID: UniverseId = UniverseId::UniformXofV1;

    /// Materialize `range` bytes of the stream for `(seed, coordinate)`.
    ///
    /// The range is normalized, then filled blockwise: skip within the
    /// first block, then emit whole 32-byte XOF blocks until `len` bytes
    /// are produced. A sub-range therefore costs only the blocks it
    /// touches, not the whole stream.
    pub fn materialize_range(
        seed: [u8; 16],
        coordinate: u64,
        range: std::ops::Range<u64>,
    ) -> Vec<u8> {
        // -------------------------------------------------------------------
        // Stage 1: normalize the range (asserted programmer contract —
        // inverted ranges are call bugs, not persistent input).
        // -------------------------------------------------------------------
        assert!(range.start <= range.end, "inverted range");
        let len = range.end - range.start;
        // -------------------------------------------------------------------
        // Stage 2: blockwise fill — skip `skip` bytes in the first block,
        // then stream whole blocks until `len` bytes are emitted.
        // -------------------------------------------------------------------
        let mut out = Vec::with_capacity(len as usize);
        let first_block = range.start / XOF_BLOCK;
        let skip = (range.start % XOF_BLOCK) as usize;
        let mut block = first_block;
        let mut offset = skip;
        while out.len() < len as usize {
            let b = Self::block(seed, coordinate, block);
            let take = XOF_BLOCK as usize - offset;
            let want = (len as usize - out.len()).min(take);
            out.extend_from_slice(&b[offset..offset + want]);
            offset = 0;
            block += 1;
        }
        out
    }

    /// Compute one XOF block (a single BLAKE3-256 output).
    fn block(seed: [u8; 16], coordinate: u64, index: u64) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(XOF_DOMAIN);
        h.update(&[UniverseId::UniformXofV1.tag()]);
        h.update(&seed);
        h.update(&coordinate.to_le_bytes());
        h.update(&index.to_le_bytes());
        *h.finalize().as_bytes()
    }

    /// Generate the full stream of `len` bytes (helper for tests).
    pub fn generate(seed: [u8; 16], coordinate: u64, len: u64) -> Vec<u8> {
        Self::materialize_range(seed, coordinate, 0..len)
    }
}

/// The entropy-reference candidate family: the v1 **negative control**.
///
/// Proposes `ENTROPY_REF { universe: UniformXofV1, seed, coordinate: 0 }`
/// with an exact XOR residual. The seed is derived deterministically from
/// the chunk's content id (no seed search — brute-force over astronomical
/// seed spaces is prohibited, ADR-0005). For arbitrary/random input the
/// residual is ~full size and RAW wins; the candidate wins only when the
/// input is (almost) the generated stream. Every byte of the residual is
/// accounted, so no "free compression" is possible
/// (`docs/theory/entropy-medium.md` §3).
#[derive(Debug, Default)]
pub struct UniverseEncoder;

impl Encoder for UniverseEncoder {
    fn name(&self) -> &'static str {
        "ENTROPY_REF"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        // -------------------------------------------------------------------
        // Stage 1: bounds gate.
        // -------------------------------------------------------------------
        let n = input.len() as u64;
        if n == 0 || n > ctx.limits.max_chunk_size {
            return Vec::new();
        }
        // -------------------------------------------------------------------
        // Stage 2: deterministic seed derivation from the content id (first
        // 16 bytes) — stored in the descriptor, so the decoder needs
        // nothing else; no seed search (ADR-0005).
        // -------------------------------------------------------------------
        let cid = ctx.content_id;
        let seed: [u8; 16] = cid.as_bytes()[..16].try_into().expect("32 > 16");
        let coordinate: u64 = 0;
        // -------------------------------------------------------------------
        // Stage 3: generate the stream and derive every residual kind
        // against it.
        // -------------------------------------------------------------------
        let generated = UniformXofV1::materialize_range(seed, coordinate, 0..n);
        let mut residuals = derive_residuals(input, &generated, ctx.limits.max_fanout);
        // -------------------------------------------------------------------
        // Stage 4: residual filter — only XOR_SPARSE edits within the
        // fanout cap are admissible for entropy-ref v1 (RANGE_REPLACE /
        // RANS_CODED / BASE_SEQUENCE are not used here). For random input
        // the diff count is ~n, so the family self-limits and RAW wins.
        // -------------------------------------------------------------------
        // Keep only residuals that are actually small: for random input the
        // diff count is ~n which exceeds the fanout cap or the cost, so the
        // family self-limits. Keep the empty (exact match) residual too.
        residuals.retain(|r| match r {
            Residual::XorSparse { edits, .. } => edits.len() as u64 <= ctx.limits.max_fanout as u64,
            Residual::RangeReplace { .. } => false, // v1: entropy-ref uses XOR only
            Residual::RansCoded { .. } => false,
            Residual::BaseSequence { .. } => false, // not valid for entropy ref
        });
        // -------------------------------------------------------------------
        // Stage 5: per-residual descriptor construction, validation, and
        // honest accounting (residual bytes + 16-byte seed + 8-byte
        // coordinate). The cost function decides from here.
        // -------------------------------------------------------------------
        let mut out = Vec::new();
        for residual in residuals {
            let rep = Representation::EntropyRef {
                universe: UniverseId::UniformXofV1,
                seed,
                coordinate,
                transform: crate::core::representation::TransformId::Identity,
                residual: residual.clone(),
                len: n,
            };
            if rep.validate(ctx.limits).is_err() {
                continue;
            }
            let split = ByteSplit {
                residual: residual_data_bytes(&residual),
                seed_state: 16 + 8,
                ..Default::default()
            };
            let cost = crate::core::cost::estimate(&rep, &split, 0);
            out.push(Candidate {
                representation: rep,
                objects: Vec::new(),
                cost,
                content_id: ctx.content_id,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let seed = [7u8; 16];
        let a = UniformXofV1::generate(seed, 42, 100_000);
        let b = UniformXofV1::generate(seed, 42, 100_000);
        assert_eq!(a, b);
    }

    #[test]
    fn range_matches_full() {
        let seed = [3u8; 16];
        let full = UniformXofV1::generate(seed, 9, 100_000);
        // a sub-range must equal the same slice of the full stream
        let sub = UniformXofV1::materialize_range(seed, 9, 10_000..20_000);
        assert_eq!(&full[10_000..20_000], &sub[..]);
        // cross-block boundary: start at a non-multiple of 8192
        let cross = UniformXofV1::materialize_range(seed, 9, 8_000..10_000);
        assert_eq!(&full[8_000..10_000], &cross[..]);
    }

    #[test]
    fn differs_across_coordinates_and_seeds() {
        let s1 = [1u8; 16];
        let s2 = [2u8; 16];
        let a = UniformXofV1::generate(s1, 0, 256);
        let b = UniformXofV1::generate(s1, 1, 256);
        let c = UniformXofV1::generate(s2, 0, 256);
        assert_ne!(a, b);
        assert_ne!(a, c);
    }
}
