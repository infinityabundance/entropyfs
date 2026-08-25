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

## Phase 6 amendment: deferred durability

The original protocol ran the full durability barrier (steps 2–6, two
`fsync`s) on **every** write transaction — correct, but ~300 µs per write
(the FUSE 4 KiB write path measured 35 MB/s). Phase 6 splits commit into
**logical commit** and **durability barrier**:

- `Tx::commit_deferred` (the FUSE write path): append records, **flush** the
  segment to the file's page cache, write the inactive superblock slot to the
  page cache, publish the in-memory root. No `fsync`.
  - A **process crash** (daemon kill) preserves every acknowledged write:
    the data and the slot are in the OS page cache (POSIX requires writes to
    survive process termination).
  - A **power loss** may lose everything since the last barrier — POSIX-legal
    (only `fsync`'d data is power-durable).
- `Store::durability_barrier` (the FUSE `fsync` handler, and the final step
  of `Tx::commit`): `fdatasync` the segment, sync the segments dir,
  re-write the superblock slot, `fsync` the superblock.
- Recovery hardening: `Store::open` validates the chosen slot's root; if it
  is missing/undecodable (power loss destroyed an un-fsynced root record),
  it falls back to the **newest valid ROOT record found in the segments** —
  a complete earlier transaction. This also covers the worst case where
  both slots reference lost roots. fsck uses the same fallback.
- `Tx::commit` (full durability) remains for CLI/batch paths and crash-court
  tests; the crash points now fire inside `durability_barrier`.
- Measured effect (CachyOS 7.2, Ryzen 7 9800X3D, FUSE): 4 KiB writes
  35 → 47 MB/s and 1 MiB writes 601 → 721 MB/s with the search fast path;
  `git clone` seconds instead of minutes; a full `cargo build --release` of
  rust-bindgen (400+ crates) 4m14s → 1m13s.
