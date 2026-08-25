# ADR-0014: Caching is performance-only, never authoritative

**Status:** accepted · **Date:** 2026-08-25

## Context

Caches (materialized chunks, metadata nodes, models, descriptors) improve
latency but must never participate in correctness.

## Decision

- FUSE caching starts conservative: ordinary cached reads, safe
  write-through behavior, conservative attribute TTLs. No global direct-I/O
  (it disables useful page-cache behavior and complicates mmap).
- FUSE writeback cache is explicitly **off by default**; it is enabled only
  after partial writes, fsync, truncate, mmap, O_WRONLY partial-page
  behavior, and crash tests all pass.
- EntropyFS-internal caches (materialized chunks, metadata, models) have
  explicit memory budgets. Keyed by immutable logical content IDs where
  possible. Eviction affects performance only.
- Dropping every cache must leave the filesystem fully correct: caches are
  rebuilt from segments/roots on demand.
- FUSE passthrough is not required for correctness and may become an
  optional privileged optimization later for suitable RAW/materialized cases.

## Consequences

- Cache correctness tests drop caches at runtime and verify identical reads.
- No unbounded global hash maps; each cache is bounded and accounted in
  `status` output.
