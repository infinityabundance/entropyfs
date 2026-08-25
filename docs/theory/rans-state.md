# rANS state: the EntropyFS adaptation of ryg-rans-rs

## 1. What rANS provides

EntropyFS uses the byte rANS surface of `ryg-rans-rs` 0.5.1 (see
`docs/research/upstream-audit.md`): single-state and two-state interleaved
encode/decode, validated symbol construction, malformed-stream guards.
The bitstream contract is the pinned upstream one (reverse-order encode,
backward byte writer, LSB-first renormalization, 4-byte flush). Decoding
never depends on CPU features: the scalar path is the authority; SIMD word
decoders are a Phase 6 performance option with identical stream semantics.

## 2. Models are persisted state

A per-chunk model is a 256-entry frequency table normalized to
`2^scale_bits` (default 14 → 16384). EntropyFS persists the model (or a
reference to a shared, content-addressed model object) and counts its bytes
in `model_bytes` (ADR-0010, §28). A model wins only if

```text
model_bytes + encoded_bytes + descriptor_bytes
```

beats every alternative (RAW, ZERO, configurational, references).

## 3. Deterministic normalization (`src/rans/model.rs`)

Given a histogram `h[256]` with total `T`:

1. If `T == 0` or single symbol dominates (≥ 99.9%), prefer
   `ZERO`/`FILL`/`PERIODIC` — the rANS candidate is dropped.
2. Compute normalized frequencies `f_i = round(2^s · h_i / T)` using
   exact rational rounding with a documented tie rule; then fix up so that
   `Σ f_i == 2^s` exactly:
   - distribute the residual `2^s − Σ f_i` (may be negative) to the symbols
     with the largest rounding error, deterministically ordered by symbol
     index (no randomness);
   - any `f_i` zeroed by rounding receives frequency stolen from the
     largest `f_j > 1` (zero-frequency theft), again deterministically.
3. Build `RansByteEncSymbol::new(start_i, f_i, s)` and
   `RansByteDecSymbol::new(start_i, f_i)` via the validated constructors.
4. `malformed::validate_freq_model` is applied to the persisted model on
   decode *before* any table is built.

The normalization is a pure function of the histogram; encode and decode
rebuild identical models from the persisted frequencies.

## 4. Model serialization (`src/rans/metadata.rs`)

The persisted model is the 256-entry frequency table, encoded by EntropyFS's
explicit codec (`src/format/codec.rs`): `scale_bits`, then a delta+RLE
packing of the `u16` frequencies with checked lengths. Model identity is
BLAKE3 of that encoding; identical models collapse to one content-addressed
object. No floating point anywhere in the model path.

## 5. Residual coding interplay

`BASE_RESIDUAL` residuals may themselves be rANS-coded (a residual with
structure is cheaper as a rANS stream than as literal edits). The residual
codec is selected by the same cost function; `residual_bytes` then splits
into `residual_literal` and `residual_encoded` in `explain` output.

## 6. Metadata entropy coding (§28)

Once the base format is sealed, rANS is used for metadata streams
(representation tags, extent deltas, object-ID prefix deltas, model IDs,
seed deltas, coordinate deltas, transform tags, residual descriptors) —
**except** the bootstrap metadata needed to locate the decoder itself,
which stays simple and independently recoverable. Bootstrap never depends
on a rANS model.

## 7. Backend dispatch (`src/rans/dispatch.rs`)

`rans/backend` enum: `ScalarSingle`, `ScalarInterleaved2` (Phase 1);
`WordScalar`, `Sse41`, `Avx2`, `Avx512` reserved. Selection is by
configuration + capability probing at mount; `ScalarInterleaved2` is the
authority. Cross-backend identity is guaranteed by the upstream bitstream
parity (ADR-0003), and dispatch tests verify byte-identical output between
backends on the same input.
