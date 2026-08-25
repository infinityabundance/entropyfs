# Configurational storage: rank/unrank as a storage representation

The central experimental mathematics of EntropyFS: representing a logical
structure by its *coordinate inside a combinatorial state space* instead of
by its bytes.

## 1. The general scheme

A family `F(n, ...)` of admissible configurations of size `n` has
`|F(n, ...)|` members. A bijection

```text
rank:   F → [0, |F|)
unrank: [0, |F|) → F
```

lets us persist `(family_tag, n, params, rank)` instead of the bytes. The
saved bits are `n·8·k − ceil(log2 |F|)` for the pure-data part, minus the
cost of any literals and descriptors. Rank/unrank must satisfy

```text
unrank(rank(x)) == x          for every admissible x
rank(unrank(i)) == i          for every i in [0, |F|)
```

and both directions must be **deterministic and bounded** (checked u128
arithmetic; candidates whose state space overflows u128 are rejected, not
truncated).

## 2. Sparse configurations (`SPARSE`)

A chunk of `n` bytes with exactly `k` marked (non-zero) positions: persist
`(k, rank, literals)` where `rank ∈ [0, C(n, k))` identifies the position
subset via the combinatorial number system (Macaulay):

```text
rank(positions sorted descending) = Σ C(p_i, i)     (i = 1..k)
unrank: greedily recover p_k > p_{k-1} > ... > p_1
```

Cost: `ceil(log2 C(n,k)) + 8k` bytes vs `8k` for raw offsets + `n` for a
bitmap. `C(n, k)` is computed with checked `u128`; for `n = 65536`
(64 KiB chunk), this wins until `k` grows large, and the cost function
decides (it usually selects `RANS`/`RAW` beyond the crossover).

## 3. Palette / multinomial configurations (`PALETTE`)

A chunk using only `m ≤ 16` distinct symbols with multiplicities
`(c_1..c_m)`, `Σ c_i = n`: persist `(palette, counts, rank)` where

```text
|F| = n! / (c_1! · ... · c_m!)
rank = Σ over positions of (count of admissible continuations before the
       chosen symbol)
```

Checked `u128`; rejected on overflow. This is enumerative coding — the
inverse of rANS in spirit (exact coordinate vs. entropy stream) and often
cheaper for strongly skewed small alphabets.

## 4. Permutations (`PERMUTATION`, behind the v1 candidate gate)

Factoradic rank over `m ≤ 34` distinct elements (34! fits u128; 35! does
not). Applied **only** when the chunk is genuinely a permutation or
derangement-like rearrangement of a small alphabet — never blindly to
arbitrary bytes. v1 keeps this representation in the engine but the
candidate generator only proposes it for `m ≤ 34` with evidence of
permutation structure.

## 5. Periodic structures (`PERIODIC`)

`(period p ≤ 1024, pattern, count, tail_len)` with `len = p·count + tail`.
`FILL` is the `p = 1` special case. Exactness is trivial; the search finds
the smallest period that reproduces the chunk.

## 6. Why this is honest storage

The rank is a coordinate, not a secret dictionary. The decoder needs only
the persisted `(family, n, params, rank)` plus the format-versioned
definition of the family. The state-space size `|F|` is published with the
format, so `stored_entropy = ceil(log2 |F|)` is exactly knowable and is
reported. If `rank` doesn't fit u128, the family is simply not usable at
that size — there is no lossy fallback.

## 7. Where the wins actually come from (hypotheses H1, H2)

- **H1** (configurational gains): sparse/low-cardinality/structured blocks
  are represented more cheaply by coordinates than by bytes. Tested by
  ablation: engine with configurational candidates vs without, same
  corpus, exact accounting.
- **H2** (base+residual): versioned/gradually-changing data is represented
  as `base + sparse patch` — the patch being a `SPARSE`-style edit set over
  the base, which is itself a configurational form. The base chain is
  bounded (depth ≤ 4) and periodically flattened.

All rank/unrank code lives in `src/entropy/` (`rank.rs`, `sparse.rs`,
`palette.rs`, `permutation.rs`, `periodic.rs`), is `forbid(unsafe_code)`,
and carries proptest round-trip properties plus Kani harnesses at bounded
sizes.
