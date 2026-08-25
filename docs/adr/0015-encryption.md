# ADR-0015: Encryption layering defined now, implemented after storage is correct

**Status:** accepted · **Date:** 2026-08-25

## Context

At-rest encryption interacts with deduplication and with structural
representation. Encrypting application bytes *before* representation would
destroy nearly all useful structure; encrypting the final persisted stream
keeps structure while protecting confidentiality.

## Decision

Defined layering (implementation deferred until the storage engine is
correct and sealed):

```text
logical bytes
 ↓
representation selection
 ↓
dedup/reference/configuration
 ↓
rANS/residual coding
 ↓
authenticated encryption
 ↓
physical storage
```

- Encryption is optional (per-store, opt-in), authenticated (AEAD), and
  applied to the persisted record payloads/segments — i.e., after
  representation, so structure survives.
- The integrity model (ADR-0011) composes: AEAD tags replace/augment
  physical CRC32C when encryption is on.
- Documentation must state the deduplication side-channel implication:
  content-addressed dedup leaks equality; users who need to hide that must
  disable dedup or use per-tenant keys.
- Key management is out of scope for the format: the store holds key
  *references* (a key id), never key material; the keyring is supplied by
  the mount environment.

## Consequences

- No encryption code ships in the first correct milestone.
- The format reserves the feature-bit and record-flag space needed for the
  encryption layer so it can be added compatibly.
