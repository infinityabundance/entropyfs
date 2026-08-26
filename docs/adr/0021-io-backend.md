# ADR-0021: Storage transport behind an `IoBackend`; io_uring as the performance path

Status: accepted.

## Context

Phase 10's persistence architecture — recoverable mutation epochs, bulk
canonicalization, one checkpoint root — was sealed through 10E/10E1, but the
physical transport still issues every durability and read syscall
synchronously (`File::write_all`, `File::sync_data`, `pread`). The 10E fd
cache made reads offset-based and lock-free, but the syscall-per-operation
shape remains: a materialization that needs a model, an encoded stream, a
dictionary and B-tree nodes performs those fetches as individual synchronous
`pread`s, and each commit's durability sequence is `seek→write_all` +
`sync_data` + dir `fsync` + slot write + superblock `fsync`.

io_uring (kernel ≥ 5.1) lets a process submit a batch of I/O operations to
the kernel in one `io_uring_enter`, and collect their completions without a
syscall per operation. That is the right transport for both sides of the
persistence shape:

- **Writes**: a record batch (the whole mutation-log append or the GC copy
  pass) is one submission; its durability barrier (segment `fdatasync`, dir
  sync, superblock slot write + `fsync`) is a small sequence of ordered ops.
- **Reads**: once a materialization's dependencies are known — model A,
  stream A, dictionary, tree node, stream B — they are one submission queue
  whose completions feed parallel decode.

## Decision

Place an internal transport abstraction below `Store` / transactions / epoch
checkpoint:

```text
Store / transactions / epoch checkpoint
                 │
                 ▼
              IoBackend
             /         \
        SyncIo           UringIo
     reference path    performance path
```

- `SyncIo` is the pre-10F synchronous engine, preserved byte-for-byte as the
  **crash-consistency oracle**. It is the default.
- `UringIo` implements the same record format and the **exact same durability
  ordering** with the syscalls issued through an io_uring ring (the
  `io-uring` crate, already a transitive dependency of `libublk` — no new
  dependency tree). It is opt-in (`--io-backend uring`).

The `IoBackend` trait exposes the primitive operations the store already
performs, with identical semantics per call:

- segment lifecycle: `open_segment`, `write_at` (pwrite), `truncate_segment`,
  `fdatasync_segment`, `sync_segment_file`, `sync_segments_dir`,
  `delete_segment`
- payload reads: `read_payload` and `read_many` (one submission for
  `UringIo`)
- superblock: `write_superblock_slot`, `fsync_superblock`

Every backend call completes its durability work before returning — the
store's orchestration (and its crash-court injection points between calls) is
unchanged, so the recovery contract (ADR-0008) holds for both backends by
construction.

## Crash-court parity is the acceptance test

A `UringIo` implementation is correct only if, at **every** crash-court
injection point, the store directory is byte-identical to the `SyncIo`
state (segment files and superblock), and recovery produces the same
admissible state. The court harness runs the full crash matrix on both
backends and diffs the directory bytes; any divergence is a bug in the
uring implementation, not a relaxation of the oracle.

## read_many

`read_many` is the architectural payoff. The read path collects the object
dependencies of a materialization (extent descriptors, entropy models,
encoded streams, dictionaries — enumerated statically from the descriptors,
plus B-tree node fetches during the extent scan) and fetches them in one
submission; decode then runs in parallel over the prefetched objects. The
single-read path (`read_payload`) remains for tree descents that cannot know
their next fetch until the current node is decoded.

## Consequences

- One unsafe module in the crate (`store/io/uring.rs`): pushing SQEs and
  popping CQEs is `unsafe` in the `io-uring` crate because the ring memory
  is kernel-shared. The module confines that unsafe to a documented,
  buffer-lifetime-preserving pattern (buffers are owned by the issuing call
  and never released before completion) and every other module keeps
  `#![forbid(unsafe_code)]`.
- The on-disk format is untouched: the backend is a transport, not a format.
  A store is equally mountable with either backend.
- The sync engine stays as the reference path and default until the uring
  courts are sealed; the default may flip once parity is proven and the
  mounted court shows no regression.
- Not in scope: registered fixed buffers/files, `SQPOLL`, `IOPOLL`, and
  async submission from multiple threads (one mutex-guarded ring; batches
  are the parallelism unit). These are follow-ups on the same seam.
