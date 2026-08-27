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
