# ADR-0013: Concurrency model

**Status:** accepted · **Date:** 2026-08-25

## Context

FUSE request handling is inherently concurrent. The naive answer (one global
mutex around the whole filesystem) serializes everything; the elaborate
answer (lock-free engine) is unjustified before profiling.

## Decision

Initial model:

- **Concurrent readers**: materialization of immutable chunks and metadata
  reads take shared access only.
- **Per-inode mutation synchronization**: writes/truncates/setattr on the
  same inode serialize through a per-inode lock; different inodes proceed in
  parallel.
- **Narrow transaction/root-commit coordinator**: a single commit lock
  serializes root commits (append → sync → superblock flip), which are
  short critical sections.
- **Immutable shared data**: COW nodes (ADR-0007) are never mutated after
  publication; caches are internally synchronized.
- **Background optimizer**: reads a descriptor generation `G`, optimizes,
  and before committing verifies `generation == G`; a newer foreground write
  causes the result to be discarded. Generation/CAS checks are the
  synchronization mechanism; no optimizer overwrite of a newer write is
  possible.

Documented lock ordering (all acquisitions short, no nested user work under
locks):

1. commit coordinator (root commits only);
2. per-inode mutation lock;
3. cache shard locks;
4. per-segment append lock.

## Consequences

- Multi-threaded FUSE request handling is enabled only after concurrency
  correctness is proven by tests (concurrent rename/unlink/write, mmap,
  fsstress).
- FUSE writeback cache stays off until the conservative path is sealed
  (ADR-0014).
