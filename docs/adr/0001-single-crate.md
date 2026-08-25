# ADR-0001: Single-crate architecture

**Status:** accepted · **Date:** 2026-08-25 · **Deciders:** EntropyFS architecture

## Context

EntropyFS has deep subsystem concerns: representation algebra, entropy
configuration mathematics, rANS adaptation, DSFB observation, persistent
format, crash-consistent store, FUSE adaptation, GC, fsck, CLI, evidence,
platform integration. A common reflex is to split these into
`entropyfs-core`, `entropyfs-format`, `entropyfs-store`, ... crates or a
Cargo workspace.

Splitting would buy: independent versioning, compile-time isolation, separate
feature sets. It would cost: cross-cutting refactors become package releases,
`pub(crate)` discipline is replaced by `pub` API churn, evidence/reproducibility
span multiple lockfiles, and architectural drift between "crate boundary" and
"module boundary" appears.

## Decision

EntropyFS is **one Cargo package** (`entropyfs`), one library target
(`src/lib.rs`) and one binary target (`src/main.rs`), and no workspace.
Architecture is expressed with:

- a deliberate module tree (`core`, `entropy`, `rans`, `dsfb`, `format`,
  `store`, `fuse`, `optimizer`, `cache`, `integrity`, `fsck`, `cli`,
  `evidence`, `platform`, `tests`);
- aggressive `pub(crate)` visibility with a deliberately small public API;
- private modules by default;
- dependency direction: `fuse`/`cli` may depend on `store`+`core`; `store`
  depends on `format`+`core`; `core` knows nothing about FUSE or disk;
  `dsfb` never appears on any materialization path.

A second crate is prohibited unless a hard external constraint makes a
separate ABI, process boundary, `no_std` artifact, or independently
distributable component objectively unavoidable — and even then it must be
justified with measured evidence, not neatness.

## Consequences

- One lockfile, one version, one build. Reproducible evidence is simple.
- Compile-time isolation is weaker than package isolation; we compensate with
  `pub(crate)`, traits, and tests that assert architectural properties
  (e.g., `core` must not depend on `store`).
- Cargo features are used only for build-time concerns (e.g., fuzzers), not
  for subsystem boundaries.
