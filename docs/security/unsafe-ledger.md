# Unsafe ledger

## Policy

`#![forbid(unsafe_code)]` applies to the entire crate **except** the
narrowly isolated `src/platform/` module (and any other explicitly bounded
module that must touch raw Linux ABI surfaces). Any `unsafe` code must be:

1. confined to `src/platform/` (or an explicitly designated module);
2. accompanied by this ledger entry with exact preconditions, lifetime and
   alignment explanation, kernel ABI reference, tests, and Miri run where
   meaningful;
3. never reachable from persistent-data parsing (parsers are
   `forbid(unsafe_code)` and cannot call platform code).

## Current ledger

*No entries.* As of this revision, the crate builds with
`#![forbid(unsafe_code)]` crate-wide with no exceptions. The Linux
integration surfaces used (FUSE via `fuser`, file I/O via `std`/`rustix`,
`flock` via `rustix`, `fdatasync`/`fsync` via `std`/`rustix`) are safe APIs;
no raw `unsafe` is required for the current phase.

## If unsafe becomes unavoidable

Example that would require an entry (not currently needed): a
`target_feature`-gated SIMD materialization kernel that must be portable
across CPUs. Before adding it, prefer: (a) a safe library (e.g.,
`ryg-rans-rs` SIMD crate, which carries its own upstream ledger), or
(b) `std::arch` wrappers isolated in `platform` with runtime ISA detection,
Miri-hostile intrinsics documented, and a disassembly test asserting the
intended instruction is emitted.

## Enforcement

- CI: `cargo geiger`-style scan (or `grep -rn unsafe src/`) fails if any
  `unsafe` appears outside the ledger.
- The ledger file is itself tested: a test walks `src/` and asserts the
  set of files containing `unsafe` equals the ledger's file list.
