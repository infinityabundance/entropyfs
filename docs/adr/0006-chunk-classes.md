# ADR-0006: Multiple logical chunk classes, 64 KiB default

**Status:** accepted · **Date:** 2026-08-25

## Context

Chunk size trades compression context against random-access materialization
cost and FUSE read granularity. The permanent format must not be hardcoded to
one size.

## Decision

- The on-disk format permits chunk classes **4 KiB, 16 KiB, 64 KiB,
  256 KiB** (tagged; class membership is per-extent, and extents of different
  classes may coexist in one file).
- Phase 1 default: **64 KiB**, chosen for useful compression context while
  keeping bounded materialization cost and FUSE-friendly read sizes.
- The logical extent tree maps `offset → extent` where each extent carries
  its own class; chunk boundaries are never user-visible semantics.
- Content-defined chunking (FastCDC/Rabin) is explicitly deferred to a later
  phase, after correctness, for cold sequential data and cross-version
  deduplication. It is not a Phase 1 feature and must not change logical
  semantics when it arrives.

## Consequences

- Read amplification is bounded by `ceil(requested / chunk_class)` chunks;
  materialization never exceeds one chunk class plus residuals.
- DSFB write-behavior classification (tiny random mutation, stable
  sequential, append-only, drift, slew) can later select chunk class
  per-extent without format change.
