# ADR-0011: Three distinct integrity concepts

**Status:** accepted · **Date:** 2026-08-25

## Context

A checksummed physical record that materializes to the wrong logical bytes
must be detected. Conflating "the bytes we stored are intact" with "the
bytes we stored mean what we think" hides corruption classes.

## Decision

Maintain three distinct integrity concepts:

1. **Logical content hash** — BLAKE3 over the *materialized* logical bytes.
   This is content identity: two different physical representations of the
   same logical bytes MUST have the same logical content ID. Deduplication,
   exact references, and background-rewrite validation all key on it.
2. **Physical record integrity** — CRC32C over each persisted record's
   header and payload. Detects torn/bit-rotted writes at the storage layer.
3. **Root/snapshot hash** — BLAKE3 (or nested CRC32C) over superblock and
   root structures; commits are validated by checksum before acceptance.

`fsck` validates the complete chain: physical record → descriptor →
materialized bytes → logical content hash → reachability.

## Consequences

- `integrity/` owns these primitives and the verification interfaces.
- The cache keys on logical content IDs, so a cache hit is only trusted when
  the ID matches the extent's expected content ID (see ADR-0014).
