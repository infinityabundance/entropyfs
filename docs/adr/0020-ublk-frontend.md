# ADR-0020: Experimental ublk block-device frontend over the same engine

**Status:** accepted · **Date:** 2026-08-25

## Context

FUSE exposes the entropy store as a POSIX filesystem. A block-device view
would let experiments layer real filesystems (ext4/XFS) above the same
representation engine and test whether entropy representations help block
storage (VM images, container layers, database files). The Linux ublk
framework (kernel 6.0+, `CONFIG_BLK_DEV_UBLK`) allows userspace block
devices with an io_uring IO ring.

## Decision

- Implement ublk as **another internal EntropyFS frontend** (`src/ublk/`)
  over the **exact same storage engine** — never a separate crate, never a
  duplicated engine (ADR-0001).
- A device is a hidden regular file (`.<name>.ublk`) in the store root:
  the device *is* a file, so the block image participates in the normal
  namespace, snapshots, dedup, GC and forensic tooling (`explain`,
  `fsck`). Block I/O goes through the same materialization/guided-search
  paths as file I/O.
- Block semantics: 4096-byte logical blocks; `read`/`write` via the extent
  engine; `flush` = durability barrier (ADR-0008 Phase 6); `discard` =
  `punch_hole`. Device capacity is fixed at creation.
- The kernel binding uses the `libublk` crate (safe control-plane API).
  The target callbacks serve the IO ring from a `Mutex<BlockStore>` — the
  same single-writer concurrency model as the FUSE frontend.
- Running `entropyfs ublk run` requires root and the `ublk_drv` kernel
  module (CachyOS ships it). The `BlockStore` adapter itself needs no
  kernel support and is fully unit-tested; `entropyfs ublk bench`
  exercises it directly.

## Consequences

- The FUSE and block frontends are interchangeable views over one store;
  both are thin adapters, and the entropy engine is the single source of
  truth.
- ublk is explicitly experimental and optional: it is not required for
  EntropyFS and never blocks the FUSE path.
- The block frontend inherits every correctness property of the engine
  (crash courts, fsck, ENOSPC, deferred durability) because it shares the
  code path.
