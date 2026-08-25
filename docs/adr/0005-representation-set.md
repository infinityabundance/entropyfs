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
| 0x0C | `PERMUTATION` — factoradic rank over distinct symbols (len ≤ 34) |
| 0x0D | `SEQUENCE_RANS` — local-match (LZ77) commands/literals/offsets, each rANS-coded or raw |
| 0x0E | `SPARSE_BLOCK64` — blockwise-64 enumerative sparse coding (per-word popcount + C(64,k) rank, rANS/raw streams) |
| 0x0F | `SEQUENCE_DICT` — SEQUENCE_RANS + a fourth copy-source stream over an external ≤64 KiB same-file dictionary (Phase-9B) |
| 0x10 | `SEQUENCE_SHARED_DICT` — SEQUENCE_DICT + a shared cross-file dictionary selected by the background optimizer (Phase-9C) |
| 0x11 | `SEQUENCE_DEEP` — repcodes (REP0/REP1) + extended length codes + the deep background matcher (Phase-9E) |

`SEQUENCE_RANS` (0x0D) is the general-purpose compression floor added in
Phase 8: pure rANS is an entropy coder, not a match finder, so the family
adds a bounded hash-chain LZ77 matcher whose three streams are then
entropy-coded with `ryg-rans-rs`. It is what makes EntropyFS competitive
with zstd-class transparent compression on ordinary workloads while the
structural/configurational families provide the differentiated ceiling.
Copy semantics are byte-progressive (overlap allowed), so RLE and
arbitrarily long matches are representable by repeated copies at one
distance; every stream has a raw fallback so degenerate streams never
force the family to lose. All three streams plus their models are
persisted, content-addressed objects counted in the extent's byte total.

Residual kind `0x04 BASE_SEQUENCE` (inside `BASE_RESIDUAL`) is the
shift-aware copy/literal delta: the output is a command stream of
`COPY(base_offset, len)` and `LITERAL(run)` against the base, so
inserted/deleted regions cost only their own bytes instead of exploding a
positional XOR residual. It shares the three-stream rANS/raw codec with
`SEQUENCE_RANS` and accepts bases shorter or longer than the target.

`SPARSE_BLOCK64` (0x0E) extends the configurational ceiling past the
`u128` combination-rank limit of `SPARSE`: blockwise-64 enumerative
coding keeps every rank within a `u64` (`C(64,32)` < 2^63), so sparse
chunks with any marked-byte count are representable (Phase-8 §6).

`SEQUENCE_DICT` (0x0F, Phase-9B) and `SEQUENCE_SHARED_DICT` (0x10,
Phase-9C) extend the general-purpose floor with *cross-chunk* dictionary
context. The former uses the previous same-file chunk as an external
≤64 KiB dictionary beside local history; the latter adds a shared
cross-file dictionary (a per-directory anchor chunk chosen by the
background `shared_dict_pass` because it maximizes savings against
member incumbents — an existing terminal chunk, so its persisted state
is accounted where it is materialized and the group pays only reference
+ read cost). A fourth copy-source stream selects LOCAL (byte-
progressive backward distance), DICT, or SHARED (absolute offset) per
copy; both dictionaries are content-addressed chunk references, both are
depth-capped like base chains, and GC pins them through the reference
closure so reads stay byte-exact after the owning file is deleted.

The tree-court evidence gate that motivated 0x10 is sealed in
`campaign-1787679299-8d6e147`: on a real tree of separately-written
small files the previous-chunk dictionary gets almost no opportunity
(279/282 files are single-chunk), and the shared dictionary recovers a
measured part of the cross-file structure the packed stream exploits
(2.182× → 2.328× post-GC).

`SEQUENCE_DEEP` (0x11, Phase-9E) extends the general-purpose floor with a
richer bounded command language: recent-distance repcodes (REP0/REP1
copies carry no offset symbol) and extended length codes (one XCOPY/XLIT
plus a u16 extra instead of runs of 131-byte continuation commands), fed
by a deep background matcher (hash chains to depth 256, lazy parsing with
a minimum-gain threshold, rep-distance priority). It is background-only
(the foreground keeps the fast greedy matcher and its CPU budget),
terminal (depth 0), and shares the rANS/raw codec. The 9D anchor POOL
(up to four per directory, greedy marginal-savings selection) lets a
heterogeneous directory give each file its best dictionary; the sealed
tree court (9D+9E campaign) measures the pair: 2.214× → 2.380× post-GC
on the real tree, and the standalone deep floor 3.796× vs the fast floor
3.744× on the src pack.

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
