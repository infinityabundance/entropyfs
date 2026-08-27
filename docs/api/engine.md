# The Engine API (Phase 12E.1)

The stable embeddable storage-engine facade: content-addressed immutable
blobs over the persistent store. `src/engine/` (Rust), with the stable C
ABI (`docs/api/c-abi.md`, `include/entropyfs.h`) and the Go binding
(`docs/api/go.md`) as the language adapters. FUSE and ublk are peers
above the same store — the engine does not depend on them conceptually.

```text
              Engine  (this facade)
                 │
       persistent Store
                 │
representation / materialization
                 │
     persistent object layer
        ┌───────┼────────┐
        │       │        │
      FUSE    ublk   application API
```

## Identity

`BlobId` is the 32-byte BLAKE3 hash of the blob's logical bytes. The
semantics are explicit and stable:

- equal logical bytes always receive the same id (dedup);
- ids are stable across compaction, representation migration,
  encoder-policy changes, GC, and io-backend choice;
- ids are independent of the physical record type;
- the id never changes for a given byte sequence — it is the content
  identity, not a location.

## Durability

- `put_blob` acknowledges at the mutation log: process-crash-safe and
  visible to later opens, NOT power-durable.
- `sync()` is the durability boundary: after it returns, every
  acknowledged put survives power loss (the group-commit generations,
  Phase 12B — concurrent syncs coalesce onto one physical barrier).
- A read-only open observes only the last durable checkpoint (replay is
  a write), so acknowledged-but-unsynced blobs are not visible through
  a read-only open until a sync/checkpoint has run.

## Operations

| Method | Semantics |
| --- | --- |
| `put_blob(bytes)` | store exact bytes; return the id (Ack durability) |
| `get_blob(id)` | materialize exact bytes; the hash gate verifies them |
| `read_blob_range(id, off, len)` | EOF-clipped range read (pread semantics) |
| `contains(id)` | whether the id was put and acknowledged |
| `sync()` | the durability barrier |
| `compact()` | reclaim unreachable bytes (report) |
| `metrics()` | the versioned metrics DTO (see `docs/operations/metrics.md`) |
| `close()` | drain in-flight ops, release the store (exclusive) |

## Concurrency

One `Engine` is safe for many concurrent readers + writers. `close` is
the lifecycle barrier: it drains in-flight operations before releasing
the store. The store's mount lock is exclusive — only ONE engine may
hold a given store open at a time (one reader OR writer per store).

## Errors

Typed `EngineError` with a stable `ErrorCode` class
(OK/NOT_FOUND/INVALID_ARGUMENT/CORRUPT_STORE/INCOMPATIBLE_FORMAT/
RESOURCE_LIMIT/IO/BUSY/UNSUPPORTED/INTERNAL/CLOSED). The C ABI and the
Go binding expose exactly these classes; programs switch on them, never
on messages.

## Stability boundary

This facade is the adoption surface: applications embed it without
understanding FUSE inode internals, epochs, B-tree nodes, MutationLog
encoding, representation tags, DSFB state, rANS layout, segment
records, or the io_uring implementation. Those remain implementation
detail, and the facade's semantics above are the public contract.
