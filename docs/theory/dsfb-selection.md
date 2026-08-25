# DSFB selection in EntropyFS

How the drift–slew fusion bootstrap observer steers candidate search without
ever touching correctness.

## 1. Channels = candidate predictor families

| Channel | Predictor | Evidence source |
|---------|-----------|-----------------|
| P0 | previous version of the same logical chunk | historical extent record (same inode, same offset) |
| P1 | adjacent chunk | left/right neighbor extent in the file |
| P2 | exact/shared content | content index hit (dedup) |
| P3 | previous chunk in the same file | the extent immediately before in write order |
| P4 | file-family structural base | shared base object for the file family |
| P5 | entropy/configuration universe | `UniformXofV1` + rank families |
| P6 | conventional rANS | per-chunk model |
| P7 | raw | always available (escape hatch) |

## 2. Measurement model

For a target chunk `X`, each channel produces a candidate base or family;
we evaluate it exactly and derive a bounded scalar evidence value:

```text
y_k = clamp01(1 − log2(1 + residual_cost_k) / log2(1 + raw_cost))
```

- `residual_cost_k` is the exact persisted bytes the candidate would need
  (residual + descriptor + references).
- `y_k ≈ 1` ⇒ the predictor is nearly free; `y_k ≈ 0` ⇒ the predictor is
  as expensive as raw.

The `DsfbObserver` (8 channels) maintains per-channel EMA residuals and
trust weights, and a state `(φ, ω, α)`:

- `φ` — current representation-quality regime;
- `ω` — drift: slow change of residual structure over time;
- `α` — slew: acceleration of that change (regime breaks).

## 3. Drift vs slew semantics (storage meaning)

**Drift** (small `|ω|`, small `|α|`, low residual EMA): the basis remains
useful and the residual evolves slowly.

- keep the basis (P0/P4);
- update small residuals;
- increase trust in the winning channel;
- narrow search: cheap residual candidates first.

**Slew** (large `|α|` or residual-EMA jump): the structure changed abruptly.

- stop forcing the old basis;
- reduce trust in the previously-winning channels;
- broaden candidate search (all families, deeper rank/unrank);
- establish a new baseline (re-seed P0/P4 from the new chunk).

## 4. Authority separation (absolute)

- **DSFB decides**: the *order* in which expensive candidates are evaluated,
  the *budget* (how many candidates to try, how deep the search), and
  whether background re-optimization is promising.
- **Exact cost decides**: the winning representation (ADR-0010).
- **Validation decides**: whether a candidate may be committed at all
  (`materialize(candidate) == X`, ADR-0011/§32).
- DSFB state is never persisted in the authoritative graph; a filesystem
  image decodes identically with all DSFB state deleted.

If DSFB predicts poorly, the filesystem wastes CPU — never data.

## 5. H3 hypothesis

> DSFB drift/slew classification reduces search cost or improves
> representation selection across evolving files.

Tested by ablation (§43): DSFB-ranked search vs exhaustive same candidate
set vs simple heuristic ranking, measuring candidates evaluated, CPU, and
final cost. A DSFB improvement is legitimate only if it reaches the same or
better representations while searching fewer candidates, using less CPU, or
adapting better over time — and it is never credited with deduplication or
rANS savings.

## 6. Implementation

`src/dsfb/`: `observer.rs` (wraps the published `dsfb` crate), `features.rs`
(evidence extraction), `drift.rs` / `slew.rs` (classifiers),
`trust.rs` (trust bookkeeping), `selection.rs` (ordered candidate plan +
budget). The optimizer (`src/optimizer/`) consumes `Selection` and performs
the exact-cost evaluation; `core` never imports `dsfb`.

## 7. Phase 4 wiring (implemented)

The guided search (`src/optimizer/search.rs`) is the only place that turns a
target chunk into a committed representation:

1. **P2 exact dedup** — always first in the write path (§12), verified by
   materializing the existing chunk and comparing exact bytes. The
   background optimizer never dedups: a rewrite of the same extent cannot
   dedup profitably (the aliased chunk-index entry must stay for
   decodability, so the apparent savings are vacuous).
2. **Cheap structural families + rANS + RAW** — always evaluated in the
   foreground (§16).
3. **P0/P1/P3/P4 bases + P5 universe** — evaluated in DSFB trust order,
   bounded by the plan budget. Foreground only tries P0 (in hand) plus at
   most one high-trust extra base; the background pass evaluates the full
   plan.

Correctness invariants enforced by the search:

- **Validation** (§32): every candidate is materialized and compared
  byte-exact against the target before it may win; a candidate's own new
  objects are visible to the validator.
- **Reference cycles are impossible**: a base whose chain transitively
  references the target chunk's own content id is rejected
  (`rebase::chain_contains`) — otherwise two chunks could reference each
  other and become undecodable.
- **The chunk index never self-aliases**: `put_chunk_in_tx` refuses to
  store `EXACT_REF{target: cid}` for `cid` (a self-referencing descriptor
  loops at decode).
- **Shallow chains**: `base_chunk_at` reports true chain depth; the
  encoder skips bases at the depth cap, and `write_region` performs
  rebase-on-write — when the previous version is itself a deep chain it is
  flattened to depth 0 in the same transaction (drift workloads stay
  shallow instead of collapsing to RAW).

Background densification (§16, H4): `src/optimizer/background.rs` runs a
resumable, bounded pass (`PassCursor`) over file extents with the full
plan, plus `rebase::flatten_if_deep` for chains at the depth threshold.
Every rewrite is byte-validated and CAS-checked (§25: the extent must
still hold the descriptor we read). The mount daemon spawns an idle-only
worker (`--no-background-optimize` disables it) that runs a bounded slice
when the store has been silent for 3 s.
