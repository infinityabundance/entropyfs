# ADR-0017: Intentional dependency posture

**Status:** accepted · **Date:** 2026-08-25

## Context

Dependencies are the largest unexamined risk in a filesystem. The temptation
is to pull in a database or a framework "to hide the metadata design" — that
abdicates the storage engine, which is EntropyFS's entire reason to exist.

## Decision

- **No database dependency** (SQLite, RocksDB, LMDB, sled, or similar) as
  the core metadata store. EntropyFS itself is the storage engine; metadata
  lives in the immutable object graph (ADR-0007).
- **No tokio/async runtime** merely because FUSE is concurrent. The stable
  synchronous `fuser` API is the interface; internal concurrency is threads
  and channels, introduced only with evidence.
- Small, well-understood crates only:
  `ryg-rans-rs` (=0.5.1), `dsfb` (=0.1.2), `blake3` (=1.8.7),
  `crc32c` (=0.6.8), `fuser` (=0.18.0), `rustix` (1.x, safe syscalls),
  `clap` (4.x), `serde`/`serde_json` (diagnostics only, ADR-0012);
  dev: `proptest`, `tempfile`.
- Tooling discipline: `cargo-deny`, `cargo-audit`; `cargo-vet` when
  practical. The dependency graph stays intentional and is documented in
  `docs/security/threat-model.md`.
- Dependencies are dependencies, not EntropyFS subsystem crates: adding a
  dependency never changes the crate architecture (ADR-0001).

## Consequences

- `Cargo.lock` is committed and pinned; upstream pins are exact
  (`=x.y.z`).
- Dependency additions require a written justification in this ADR's
  history.
