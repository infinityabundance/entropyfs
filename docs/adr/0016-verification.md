# ADR-0016: Verification posture — property tests, crash courts, fuzzing, Kani

**Status:** accepted · **Date:** 2026-08-25

## Context

A filesystem's failure modes are: wrong bytes, torn commits, unbounded
resource use, panics on corrupt input, and silent data loss. Each needs a
different verification tool.

## Decision

- **Property tests (proptest)** as the workhorse for: rank/unrank round
  trips (`unrank(rank(x)) == x`, `rank(unrank(i)) == i`), materialization
  round trips, residual inverse correctness, cost-accounting invariants,
  store recovery, extent-tree invariants.
- **Crash courts**: deterministic injection points
  (`AFTER_RECORD_APPEND`, `AFTER_SEGMENT_FDATASYNC`, `AFTER_ROOT_WRITE`,
  `AFTER_SUPERBLOCK_WRITE`, `BEFORE_SUPERBLOCK_FSYNC`,
  `AFTER_SUPERBLOCK_FSYNC`, `BEFORE_OLD_SEGMENT_DELETE`); for each fixture:
  construct known pre-state → begin operation → kill at injection point →
  restart → mount/read or fsck → verify state is an admissible pre/post
  transaction state, no unreachable authoritative metadata, logical hashes
  intact. Machine-readable receipts.
- **Fuzzing (cargo-fuzz)**: record decoder, superblock decoder, descriptor
  decoder, rank/unrank, materializer, residual application, rANS model
  parser, extent-tree mutation, directory mutation, fsck record walker,
  corrupted-segment recovery. Malformed persistent data must return typed
  errors, never panic.
- **Kani** where it provides real value at bounded sizes: checked extent
  arithmetic, sparse rank/unrank small cases, permutation rank/unrank at
  bounded sizes, descriptor output-length proofs, residual inverse
  correctness, transaction-generation selection, bounds calculations.
- **Miri** on the safe core where meaningful (the crate is
  `forbid(unsafe_code)` except the isolated platform module, which carries
  its own ledger).
- Standard suites: unit tests, integration tests in `src/tests/`,
  `pjdfstest`, Linux fstests generic/FUSE subset, `fsx`, `fsstress`,
  power-cut and ENOSPC tests — always in isolated images/VMs or scratch
  directories, never against the host filesystem.

## Consequences

- `fsck` is implemented alongside the format, not at the end.
- Every panic path in persistent-data parsing is a bug; fuzz targets encode
  that as a hard requirement.
