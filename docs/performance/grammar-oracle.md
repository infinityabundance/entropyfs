# Phase-12D-0: the grammar-addressed entropy OFFLINE oracle

Sealed: `evidence/performance/grammar-oracle-*/`.
Oracle: `src/tests/grammar_oracle.rs`.

## The deliverable

The 12D brief's first step, executed offline — no format change:

> **Train grammar candidates on a real tree, encode all members, FULLY
> account grammar + state + residual + descriptor bytes, and compare
> against the incumbents. If it loses, stop.**

## The model under test

```text
X = Render(G, Θ) ⊕ R
  G = a bounded template grammar (Literal skeleton + Slot positions)
  Θ = per-member production state (the slot values, stored raw)
  R = the exact residual (structurally 0 for this induction — every
      member byte is a slot or a literal)
```

The grammar object (the shared skeleton) is stored ONCE; each member
stores its slot values + a tiny descriptor (grammar id + slot lengths).
The induction is bounded: longest common prefix/suffix become the
leading/trailing literals, the middle is split on the longest common
internal substring (capped at 8 slots, 256-byte literals), and periodic
segments are `Repeat`-compressed. State is raw (the conservative
accounting; the brief's rank/residual tightening is 12D-1 refinement).

## The corpora

- **generated-config** (200 members × 64 KiB): a NON-PERIODIC shared
  skeleton (deterministic irregular bytes, identical across members —
  incompressible by RANS, not periodic) plus per-member fields. The
  grammar-friendly class, sized so the store's per-file metadata is
  amortized and the comparison basis is the CONTENT.
- **diverse** (200 members × 64 KiB of mixed source/noise/zeros/prose):
  no shared skeleton — the honest negative control.

## Sealed numbers (release)

| generated-config | bytes | ratio |
| --- | ---: | ---: |
| logical | 12 015 600 | — |
| **grammar (fully accounted)** | **66 059** | **181.9×** |
| EntropyFS foreground | 5 899 905 | 2.04× |
| EntropyFS settled (+dict) | 465 068 | 25.8× |
| zstd -19 whole pack | 29 731 | 404.1× |

Diverse control: grammar 1.00× (≈ RAW, as expected), EntropyFS 2.45× —
the grammar loses there ✓ (the concept is not magic).

## The verdict: STOP (per the brief's gate), with the direction recorded

The fully-accounted grammar **beats EntropyFS's best settled machinery by
7.0×** on the grammar-friendly corpus — the template-grammar concept is
directionally real, and the negative control behaves correctly. But it
does **not** beat every incumbent: **zstd-whole (29.7 KB) is 2.2×
smaller** than the grammar (66.1 KB), because the grammar stores its
irregular shared skeleton LITERALLY while zstd entropy-codes it.

The identified refinement is the brief's own "persisted entropy": **the
grammar object is itself data and must be entropy-coded** (rANS over the
skeleton, exactly like the members' state). The raw-skeleton accounting
is the conservative bound; a grammar whose skeleton is rANS-coded would
close the zstd gap — but the brief's gate applies to the fully-accounted
conservative case, so **12D-1 (the format-bit investigation) is not
justified on this evidence**. The oracle stays in the suite as the
offline measurement surface for any future grammar round.

---

# Phase-12D-1: the entropy-coded grammar skeleton (round two)

Sealed: `evidence/performance/grammar-ec-oracle-1787857795-806432e/`.
Oracle: `src/tests/grammar_oracle.rs::grammar_ec_oracle`. Driver:
`tools/court-grammar-ec.sh`.

## The refinement under test

The 12D-0 verdict's own identified direction, executed: the grammar
object is itself a byte string, so in the real
`Representation::Grammar { grammar: ChunkId, .. }` design it is stored
as a normal content-addressed CHUNK — put through the store's
representation search and charged its smallest valid candidate's
persisted bytes (descriptor + model + objects + integrity, the store's
own accounting authority). `grammar_chunk_cost` runs byte-rANS,
sequence-rANS, the four configurational families, and RAW over the
skeleton payload (the exact bytes 12D-0's `grammar_bytes` accounted,
`Repeat`-compressed segments included) with exact-cost selection.

```text
grammar_ec_total = chunk_cost(skeleton) + Σ(state + descriptor)
```

Nothing hidden; the only change vs 12D-0 is the literal skeleton
replaced by its entropy-coded form. State remains raw (conservative).

## Sealed numbers (release, n = 200)

| generated-config | bytes | ratio |
| --- | ---: | ---: |
| logical | 12 015 600 | — |
| grammar raw skeleton (12D-0) | 66 059 | 181.9× |
| **grammar entropy-coded (12D-1)** | **35 156** | **341.8×** |
| EntropyFS settled (+dict) | 465 068 | 25.8× |
| zstd -19 whole pack | 29 731 | 404.1× |

Skeleton decomposition: 60 059 B literal → **29 156 B via SEQ_RANS
(3.88 bits/byte)** + 6 000 B state/descriptors. The entropy-coding
refinement is REAL: the skeleton is not incompressible literal text —
the sequence matcher's context modeling captures the LCG-text structure
at 3.88 bits/byte, cutting the grammar's total from 66 059 B to
35 156 B (−47%) and closing the zstd gap from **2.2× to 1.18×**.

Diverse control: skeleton 0 B (no shared skeleton — the induction
produces all-slot members), EC total 13 109 241 B vs EntropyFS
5 350 054 B — the grammar loses there ✓.

## The verdict: STOP (the gate is the gate), gap now 1.18×

The entropy-coded grammar beats EntropyFS settled **13.2×** and the
12D-0 raw-skeleton grammar **1.9×**, but does **not** beat every
incumbent: **zstd-whole (29 731 B) remains 1.2× smaller**. The format
-bit investigation is NOT justified on this evidence.

Where the remaining 1.18× lives (decomposition, recorded honestly):

1. **Context-modeling quality** — the sequence matcher's order-1-style
   modeling reaches 3.88 bits/byte on the skeleton; zstd's order-2+
   modeling reaches ~3.7. Closing this needs an order-2+ contextual
   coder — the 12C/12D "contextual entropy models" direction, a new
   coder, not an oracle tweak.
2. **State encoding** — the per-member fields are stored raw (~6 000 B
   total, 17% of the grammar's cost); rank-coding them would save
   ~2–3 KB, still not enough alone.

Combined, both refinements would land near the boundary — which is
precisely why the brief's discipline stops here: the gate applies to
the fully-accounted case actually measured, and neither refinement is
justified without its own evidence round. The 12D line's recorded
result: the template-grammar concept is within 18% of whole-pack zstd
WHILE providing per-member random access (an architectural property the
pack lacks), but the format bit is not earned on this evidence.
