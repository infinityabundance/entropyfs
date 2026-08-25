# ADR-0003: Reuse `ryg-rans-rs` as the entropy backend

**Status:** accepted · **Date:** 2026-08-25

## Context

EntropyFS needs a production-quality rANS entropy coder. Writing our own is a
large correctness surface (bitstream contracts, renormalization arithmetic,
model normalization, malformed-input handling). `ryg-rans-rs` 0.5.1 is a
native-Rust, `no_std`, `forbid(unsafe_code)` forensic reconstruction of
Fabian Giesen's `ryg_rans`, sealed by 158 behavioural receipts, with division
and reciprocal paths, two-state interleaving, 64-bit rANS, word rANS, alias
method, SIMD decoders, malformed-input validation, and Kani proofs.

Source-guided audit findings (2026-08-25, crate `ryg-rans-rs` 0.5.1 +
`ryg-rans-rs-core` 0.5.1):

- Facade `ryg_rans_rs::byte` re-exports the full core: `RansByteState`,
  `RansByteEncSymbol::new(start, freq, scale_bits)` (validated, scale_bits
  1..=16, reciprocal fast path), `RansByteDecSymbol::new(start, freq)`,
  `BackwardByteWriter`, `ByteReader`, `rans_byte_enc_put_symbol`,
  `rans_byte_enc_flush`, `rans_byte_dec_init/get/advance_symbol`,
  `ByteInterleavedEncoder/Decoder` (two-state), R64 variants
  (`Rans64State`, `BackwardWord32Writer`, `Word32Reader`, `rans64_*`),
  word rANS scalar surface (`RansWordState`, `RansWordTables`,
  `rans_word_*`), alias method (`rans_byte_alias_normalize_freqs`,
  `rans_byte_alias_build_table`, `rans_byte_alias_*`), `EncodeError`,
  `DecodeError`, `ModelError`.
- `ryg_rans_rs::malformed` provides `validate_byte_compressed`,
  `validate_r64_compressed`, `validate_word_compressed`, `validate_freq_model`,
  `RenormGuard`, `has_dominant_symbol`, `is_single_symbol`.
- Model frequency **normalization** for byte rANS is not provided by the
  core (only the alias-method normalizer is); EntropyFS therefore owns
  deterministic histogram normalization in `src/rans/model.rs` and builds
  validated enc/dec symbols through the public constructors.
- The `simd` feature adds SSE4.1/AVX2/AVX-512 word decoders; the scalar
  path is the authority and is the default.

## Decision

Depend on `ryg-rans-rs = "=0.5.1"` (features `alloc` for convenience
helpers; SIMD deferred to Phase 6 after profiling). EntropyFS `src/rans/`
is a **thin adaptation layer**:

- canonical model construction (`rans/model.rs`);
- model identity and serialization for EntropyFS (`rans/metadata.rs`);
- candidate modes and residual encoding (`rans/residual.rs`);
- runtime backend selection with a scalar-authority path (`rans/dispatch.rs`).

We do **not** fork ryg_rans logic into EntropyFS. Every bit of a model's
frequencies and every encoded stream counts toward physical size.

## Consequences

- The rANS bitstream contract is pinned by upstream parity evidence rather
  than by our tests alone.
- Model bytes + encoded bytes must beat alternatives; model sharing and
  content-addressing of models is an optimization, not a correctness need.
- If the upstream crate later diverges, the adaptation layer confines the
  change to `src/rans/`.
