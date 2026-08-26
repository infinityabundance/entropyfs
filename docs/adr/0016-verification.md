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
- **Fuzzing (hostile-media court, in-package)**: the Phase-11A hostile-
  media court (`src/tests/hostile_media/`, `docs/security/hostile-media-
  court.md`) is the malformed-input suite: descriptor-decode fuzzing
  (every bounded byte string through `format::descriptor::decode`;
  decode-OK implies structural validation OK and a byte-exact canonical
  re-encode), bounded materialization-graph fuzzing (a fuzz-defined
  descriptor table + object table + entry descriptor materialized
  through an in-memory hostile resolver; bounded-valid or typed-reject),
  and the whole-store mutator (physical corruption with broken CRC vs
  semantic adversarial mutation with recomputed CRC, driving
  open/fsck/materialize over tiny stores). The driver is proptest — a
  deliberate in-package harness rather than a `fuzz/` Cargo package
  (ADR-0001: one package; no architectural drift). Malformed persistent
  data must return typed errors, never panic.
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
