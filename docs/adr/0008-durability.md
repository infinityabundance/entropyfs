# ADR-0008: Dual superblocks, append-only segments, generation commit

**Status:** accepted · **Date:** 2026-08-25

## Context

Crash consistency requires that recovery observe the complete previous
transaction or the complete new transaction, never a hybrid root.

## Decision

- Backing layout: `store/superblock` (two slots A/B at fixed offsets),
  `store/segments/0000000000000000.seg ...`, `store/lock`.
- Append-only segments hold immutable records (tag, version, flags, header
  length, stored length, materialized length, content ID, header CRC,
  payload CRC, payload). Segment files are large (initial default 128 MiB,
  benchmarked) and sealed when full.
- Two independently checksummed superblock slots, each recording: magic,
  format version, compat/ro_compat/incompat feature bits, UUID, generation,
  root object ID, current segment sequence, creation parameters, integrity
  hash. Commit **by generation**.
- Commit durability ordering:
  1. append all new immutable records to the current segment;
  2. `fdatasync` the affected segment;
  3. ensure newly created segment directory entries are durable
     (`fsync` of the `segments` dir when a new segment file is created);
  4. construct the new root object (already appended in step 1);
  5. write the **inactive** superblock slot with generation N+1;
  6. `fsync` the superblock file;
  7. only then report durable completion to the caller.
- Mount: read both slots, validate both, choose the highest *valid* committed
  generation; reject unsupported incompat features. Never overwrite the only
  known-good root.

## Consequences

- Recovery may see: old root, or new root — never a hybrid, because the new
  root object is only durable *after* all its referenced records are durable,
  and the superblock is only flipped after that.
- Records appended but unreferenced by any root are garbage by definition;
  GC (ADR-0009) reclaims them. This is the log-structured trade: correctness
  is trivial, space needs GC.
- Every durability boundary is a crash-court injection point
  (`docs/recovery/crash-consistency.md`).
