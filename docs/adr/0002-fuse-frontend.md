# ADR-0002: FUSE synchronous `fuser` frontend first; ublk later

**Status:** accepted · **Date:** 2026-08-25

## Context

EntropyFS must become a daily-use Linux filesystem on CachyOS/Arch x86-64.
The first production-capable frontend could be: a kernel module, an
out-of-tree FS, FUSE, or ublk.

Lessons from prior art (see `docs/research/prior-art.md`): kernel modules
multiply crash surface and packaging burden; FUSE has mature caching,
writeback options, and a stable ABI; ublk is attractive for block-device
experiments but its userspace-kernel protocol is newer and the VFS still
sits above it.

## Decision

Implement the first production-capable frontend with **FUSE via the stable
synchronous `fuser` crate (0.18.0, pure Rust, no libfuse dependency)**.

Reasons:
- userspace failures cannot panic the kernel;
- iteration and forensic testing (crash courts, VM tests) are dramatically
  safer;
- a normal CachyOS installation can mount it (`/dev/fuse`, `fusermount3`
  present) with no kernel patch and no out-of-tree module;
- stable ABI; the experimental FUSE-over-io_uring ABI is **not** a foundation.

`ublk` is a future alternative frontend, implemented as an internal
`src/ublk/` module over the same storage engine — never as a separate crate
and never required for EntropyFS.

Internal backing-store io_uring is deferred until profiling justifies it.

## Consequences

- The FUSE layer is an adapter: it converts FUSE operations into storage
  transactions and contains no entropy algorithms.
- FUSE semantics (writeback cache, mmap, direct I/O) are gated by the
  conservative caching strategy in ADR-0014.
- The `fuse` module may be replaced by a `ublk` module without touching the
  store or the representation engine.
