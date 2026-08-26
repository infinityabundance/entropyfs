# Unsafe ledger

## Policy

`#![forbid(unsafe_code)]` applies to every module in the crate **except**
`src/platform/io_uring.rs` — the ONE designated file that must touch the
raw Linux io_uring ABI. Any future `unsafe` must be:

1. confined to `src/platform/` (or an explicitly designated module);
2. accompanied by this ledger entry with exact preconditions, lifetime and
   alignment explanation, kernel ABI reference, tests, and Miri run where
   meaningful;
3. never reachable from persistent-data parsing (parsers are
   `forbid(unsafe_code)` and cannot call platform code).

## Enforcement

- The crate root carries `#![deny(unsafe_code)]` as the backstop; every
  other module forbids it. `deny` at the root is overridable only by the
  designated `allow` in `platform/io_uring.rs`.
- A test (`unsafe_files_match_ledger` in `src/tests/unsafe_ledger.rs`)
  walks `src/` and asserts the set of files containing `unsafe` equals the
  ledger's file list below.

## Current ledger

| File | Unsafe calls | Kernel ABI | Tests |
|------|--------------|------------|-------|
| `src/platform/io_uring.rs` | `SubmissionQueue::push` (the io-uring crate's sole unsafe primitive) | `io_uring_setup` / `io_uring_enter` (kernel ≥ 5.1; READ/WRITE ops 5.6, UNLINKAT 5.11) | `nop_completes`, `batch_out_of_order_ok`, `write_read_roundtrip`; full-store crash-court parity matrix (`src/tests/io_backend_parity.rs`) |

### `src/platform/io_uring.rs` — exact preconditions

**What is unsafe:** pushing an SQE into the submission ring
(`io_uring::squeue::SubmissionQueue::push`). The ring is memory shared
with the kernel; `push` copies the caller's `Entry` into the mmap'd ring
and the kernel later reads it.

**Preconditions (all enforced by the calling pattern):**

1. *Buffer lifetime.* Every buffer referenced by a submitted SQE (read
   destination, write payload, unlink pathname, superblock slot) is owned
   by the calling frame — a local `Vec`/`CString` — and is never dropped
   before the completion for its operation is consumed. `Uring::submit_and_wait`
   does not return until exactly one completion per submitted op has been
   collected, so "valid until the call returns" is the entire contract and
   it is satisfied by construction.
2. *File descriptor validity.* Every fd referenced by an SQE comes from
   the backend's `Arc<File>` cache; deletion evicts the cache entry
   *before* the unlink op and happens strictly after the new root is
   durable, so no in-flight op can reference a closed fd.
3. *Alignment.* io_uring READ/WRITE ops place no alignment requirement on
   the buffer for buffered I/O (the storage engine does not use
   `IORING_SETUP_IOPOLL`); buffers are standard `Vec<u8>` allocations.
4. *No aliasing.* Each SQE's buffers are exclusively owned by one op per
   submission batch (distinct `user_data` tokens); the module never
   submits overlapping buffers.

**Kernel ABI reference:** `io_uring_setup(2)`, `io_uring_enter(2)`,
`IORING_OP_READ`/`WRITE` (5.6), `IORING_OP_FSYNC` (5.1),
`IORING_OP_UNLINKAT` (5.11). The `io-uring` crate (0.7.14, already a
transitive dependency of `libublk`) is the sole ABI wrapper; it carries
its own upstream safety documentation.

**Why the exception cannot be avoided:** `SubmissionQueue::push` is the
crate's only way to enqueue an operation; there is no safe wrapper (the
completion queue, by contrast, is consumed through its safe `Iterator`
impl). io_uring is inherently a kernel-shared-memory ABI.

## If unsafe becomes unavoidable elsewhere

Example that would require a new entry (not currently needed): a
`target_feature`-gated SIMD materialization kernel that must be portable
across CPUs. Before adding it, prefer: (a) a safe library (e.g.,
`ryg-rans-rs` SIMD crate, which carries its own upstream ledger), or
(b) `std::arch` wrappers isolated in `platform` with runtime ISA detection,
Miri-hostile intrinsics documented, and a disassembly test asserting the
intended instruction is emitted.
