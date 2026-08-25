# ADR-0012: Explicit little-endian byte codecs; serde only for JSON evidence

**Status:** accepted · **Date:** 2026-08-25

## Context

`bincode`, `postcard`, or unconstrained serde layouts make enum discriminants
and struct memory layouts part of the permanent format, which is fragile,
opaque, and hard to audit across versions.

## Decision

- Every permanent on-disk structure is serialized by an **explicit
  byte-level little-endian codec** in `src/format/codec.rs`, with: magic/tag,
  per-structure version, encoded length, checked arithmetic, explicit
  endianness, integrity field, and compatibility rules
  (`docs/format/ondisk-v1.md`).
- Never serialize Rust enum discriminants or struct layouts directly.
- `serde`/`serde_json` are used **only** for human-readable JSON evidence and
  diagnostic artifacts (`evidence/`, `entropyfs inspect --json`, crash-court
  receipts) — never for anything the decoder depends on.

## Consequences

- Format evolution is explicit: bump per-structure versions, set feature
  bits, update the compatibility matrix (`docs/format/compatibility.md`).
- Fuzzing targets decode the same byte codecs the runtime uses
  (ADR-0016).
