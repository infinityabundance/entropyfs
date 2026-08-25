# ADR-0005: Bounded, non-Turing-complete representation set v1

**Status:** accepted · **Date:** 2026-08-25

## Context

The core premise: `X = Materialize(D)` where `D` is a persisted descriptor
that is *not* necessarily the logical bytes. To keep decoding exact, bounded,
and auditable, the descriptor language must be closed and finite.

## Decision

Phase-1 representation set (stable numeric tags, see
`docs/format/ondisk-v1.md`):

| Tag | Representation |
|-----|----------------|
| 0x01 | `ZERO` — all-zero extent |
| 0x02 | `FILL` — repeated single byte |
| 0x03 | `RAW` — literal bytes (universal escape hatch) |
| 0x04 | `RANS` — rANS via `ryg-rans-rs` with a persisted model |
| 0x05 | `EXACT_REF` — exact sub-range reference to an existing chunk |
| 0x06 | `BASE_RESIDUAL` — base chunk + residual transform |
| 0x07 | `SPARSE` — combinatorial rank of marked positions + literals |
| 0x08 | `PALETTE` — small alphabet + multinomial rank |
| 0x09 | `PERIODIC` — period, pattern, count, tail |
| 0x0A | `ENTROPY_REF` — universe + seed + coordinate + transform + residual |
| 0x0B | `INLINE` — short literal bytes inside the descriptor |

Hard rules:

- **No executable programs.** The descriptor language is not Turing-complete;
  there is no loop construct other than bounded repetition derived from
  persisted lengths.
- Every representation declares: maximum output size, maximum encoded size,
  deterministic semantics, a deterministic operation budget, checked
  arithmetic, a bounded reference depth (initial hard maximum **4**,
  format-policy controlled), bounded memory, and range-readable
  materialization where possible.
- A malformed descriptor must never produce unbounded allocation, CPU,
  recursion, or expansion. All decode loops are length-bounded by persisted
  lengths checked against `Limits` *before* allocation.
- `ENTROPY_REF` v1 ships exactly one control universe, `UniformXofV1`
  (BLAKE3-based deterministic XOF), which is a **negative control**: random
  data must fall back to RAW. Brute-force seed search over astronomical seed
  spaces is prohibited. The universe specification is part of the format
  version.

## Consequences

- Encoding is a search over a fixed candidate set; decoding is a bounded
  interpreter. This asymmetry is deliberate and documented.
- Adding a representation later requires a format-feature gate (compat /
  ro_compat / incompat, see `docs/format/compatibility.md`).
