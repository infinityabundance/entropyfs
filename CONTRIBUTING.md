# Contributing to EntropyFS

EntropyFS is a single-crate research implementation (ADR-0001). The
project treats the code itself as custodial research evidence.

## Code commentary

Non-trivial code must document not only *what* it does, but *why the
design exists*, *how correctness is preserved*, the relevant
persistence/concurrency/resource invariants, and any measured evidence
that explains a non-obvious choice. This is a repository rule, not a
style preference.

See **[docs/architecture/commentary-standard.md](docs/architecture/commentary-standard.md)**.

## Evidence discipline

- All material performance and density claims must point to sealed
  artifacts in `evidence/` (see `evidence/performance/INDEX.md`).
- Withdrawn or superseded artifacts are amended or replaced, never kept
  as claims. Failed experiments and falsified hypotheses are recorded in
  the CHANGELOG's development phase ledger.
- The documentation roles are fixed: README describes the present
  system; CHANGELOG preserves its history; evidence proves measured
  claims; ADRs explain architectural decisions.

## Workflow

- Every phase lands as: code (with tests) → sealed evidence run (the
  archive name carries the code revision) → evidence/docs commit →
  version bump + tag + publish when an actual change lands.
- The full test suite must stay green (`cargo test --lib`); perf/decision
  gates are release-only where unoptimized numbers cannot judge them.
