# The C ABI (Phase 12E.14)

The stable C surface over the embeddable engine facade (12E.1). Header:
`include/entropyfs.h`. Implementation: `src/ffi/mod.rs` (the crate's
second ledger-designated unsafe file — see
`docs/security/unsafe-ledger.md`). Proofs: the Rust FFI court
(`src/tests/ffi_cabi.rs`) and the C smoke test (`tools/ffi-smoke.sh` +
`tools/ffi-smoke/smoke.c`).

## Versioning

- `entropyfs_abi_version()` returns `EFS_ABI_VERSION` (currently 1),
  INDEPENDENT of the on-disk format version. They are separate
  compatibility domains. Check it at runtime and fail gracefully on
  mismatch.
- The error classes (`EFS_*` in the header's `enum entropyfs_error`) are
  the machine-readable contract. Programs switch on them; the
  `entropyfs_last_error` string is diagnostic detail only, never parsed.

## The surface

| Function | Semantics |
| --- | --- |
| `entropyfs_engine_open(path, mode, &handle)` | `EFS_ENGINE_OPEN` (existing store) or `EFS_ENGINE_CREATE` (fresh); writes the owned opaque handle |
| `entropyfs_engine_close(handle)` | CONSUMES the handle; close exactly once; use-after-close is UB |
| `entropyfs_blob_put(h, data, len, id[32])` | Content-addressed put (Ack durability; power-durable after `sync`); dedup: equal bytes → equal id |
| `entropyfs_blob_get(h, id, &buf, &len)` | Full blob, byte-exact (the engine's hash gate); callee-allocated output |
| `entropyfs_blob_read_range(h, id, off, len, &buf, &len)` | EOF-clipped range read; callee-allocated output |
| `entropyfs_contains(h, id, &int)` | Whether the blob id was put and acknowledged |
| `entropyfs_sync(h)` | The durability boundary (group-commit generations) |
| `entropyfs_compact(h, &reclaimed, &physical)` | Reclaim unreachable bytes |
| `entropyfs_metrics_json(h, &buf, &len)` | The versioned `EngineMetrics` DTO as JSON (same schema as `entropyfs metrics --json`) |
| `entropyfs_last_error(buf, cap)` | Thread-local diagnostic detail |
| `entropyfs_free(ptr)` | The ONE release mechanism for callee-allocated outputs |
| `entropyfs_abi_version()` | ABI version query |

## Ownership and concurrency

1. **Handles** are opaque; the caller owns them; close exactly once.
   They are safe to share across threads for concurrent operations
   (many readers + writers; close drains in-flight ops).
2. **Inputs** (`data`/`id`) are borrowed for the call.
3. **Outputs** are a single self-describing allocation (`[len u64]data…`,
   header part of the allocation) freed with `entropyfs_free` — never
   any other way, never twice.
4. No Rust panic unwinds across the boundary; a caught panic is surfaced
   as `EFS_INTERNAL` and logged — a defect, never normal control flow.

## Building

```sh
cargo build --release          # produces target/release/libentropyfs.so
cc -I include app.c -L target/release -lentropyfs -o app
LD_LIBRARY_PATH=target/release ./app
```

## Testing

```sh
tools/ffi-smoke.sh   # compiles + runs the C smoke (21 checks)
cargo test --release --lib ffi_cabi   # the Rust-side FFI court (5 tests)
```

The Go binding (12E.15) is the next consumer of this surface and
inherits exactly these semantics.
