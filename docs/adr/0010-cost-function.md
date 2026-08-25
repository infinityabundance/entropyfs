# ADR-0010: Explicit cost function and policy modes

**Status:** accepted · **Date:** 2026-08-25

## Context

Minimizing bytes blindly is wrong: a tiny descriptor that costs 100× more
CPU and I/O to materialize may be a bad trade for latency-sensitive reads.
Every representation must still be exact.

## Decision

Define the objective:

```text
J = persisted_bytes
  + λ_read  * estimated_read_cycles
  + λ_write * estimated_write_cycles
  + λ_io    * dependent_physical_reads
  + λ_depth * reference_depth
```

- `persisted_bytes` is the full accountable persisted state for the extent:
  descriptor + model + residual + seed/state + coordinate + integrity +
  attributable GC overhead (see `docs/theory/information-accounting.md`).
- `estimated_read_cycles` and `estimated_write_cycles` are deterministic
  cycle budgets computed per representation (fixed tables, not wall-clock
  measurements at selection time).
- `dependent_physical_reads` counts physical objects that must be fetched to
  materialize (1 for RAW/RANS object, 1 for each distinct base object, etc.).
- `reference_depth` penalizes deep base chains (initial hard cap 4).
- `λ` tables define policy modes: `capacity`, `balanced` (default),
  `latency`, `archive`, `ram`.

Rules:

- Persist the representation, not the transient cost estimate. The estimate
  is deterministic, so it can be recomputed later.
- A future pass may re-evaluate and rewrite representations without changing
  file contents, provided `hash(materialize(old)) == hash(materialize(new))`
  (background optimizer, Phase 4).
- The final selection is by this exact function. DSFB (ADR-0004) only orders
  the candidate search.

## Consequences

- `latency` mode will prefer RAW/interleaved-rANS over deep references even
  at higher physical cost; `capacity` will prefer the smallest descriptor.
- Ablation benchmarks report per-component bytes (descriptor/model/residual/
  seed/coordinate/integrity/GC) so the cost function is auditable
  (`docs/performance/methodology.md`).
