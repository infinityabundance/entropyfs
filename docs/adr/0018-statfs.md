# ADR-0018: Conservative `statfs`; opt-in logical overcommit

**Status:** accepted · **Date:** 2026-08-25

## Context

EntropyFS's effective capacity is workload-dependent. Reporting an
"optimistic" capacity as guaranteed `df` capacity would violate the honesty
principle and break `ENOSPC` semantics for incompressible data.

## Decision

- `statfs()` reports capacity based primarily on **actual physical backing
  capacity**: `f_blocks = physical_capacity`, `f_bfree/f_bavail` account for
  used + reclaimable + GC reserve + pending-transaction worst case.
- Separate EntropyFS statistics expose the full picture: physical capacity,
  physical used, physical reclaimable, logical bytes stored, current
  effective ratio, reachable metadata, reachable residual data, reachable
  model data, snapshot-pinned bytes, GC reserve.
- Optional **logical overcommit/quota mode** is opt-in and clearly documents
  that incompressible writes can encounter ENOSPC despite logical headroom.
- Before accepting a transaction, EntropyFS preserves enough worst-case
  physical space for a safe commit.

## Consequences

- Applications see honest capacity; the effective ratio is an observation,
  not a promise.
- ENOSPC behavior is safe by construction (ADR-0009).
