# Format v1 compatibility policy (Phase 12E.3)

The normative contract for the on-disk format. The versioning model and
the feature-bit registry live in `docs/format/compatibility.md` and
`docs/format/feature-registry.md`; this document is the POLICY that
governs them — what a reader/writer MUST and MUST NOT do.

## The rules

```text
unknown COMPAT
    may be ignored

unknown RO_COMPAT
    refuse writable open
    permit read-only operation when semantics allow it

unknown INCOMPAT
    refuse open/mount

known retired representation
    decoder remains able to read it

new encoder release
    may stop emitting an old representation

semantic break impossible to express through feature flags
    requires a new format major
```

Each rule is implementation-tested: `src/tests/compat_seal.rs` pins the
behavior for unknown bits in every set, the read-only fallback for
`ro_compat`, and the typed errors (`docs/operations/fsck-json.md` is the
operator view; the library errors are `CompatibilityError`).

## Typed compatibility errors

The library error carries, structurally (never via string parsing):

- format major/minor of the store;
- the unknown bit's number and the mask it appeared in;
- the required access mode (read-only permitted / writable refused /
  open refused);
- the suggested remediation.

The CLI renders an upgrade/fsck hint on top of the same structured
error; the library surface is the structured error.

## Feature registry discipline

- One normative registry: `docs/format/feature-registry.md` (bits,
  names, sets, first writer/reader versions, affected persistent
  structures, whether an encoder may retire the bit, decoder support
  commitment).
- No speculative ranges are reserved for application domains ("video",
  "game", "AI", ...). Ranges are reserved only per generic
  format-extension policy.
- A feature bit is added only with its registry row, its compat-seal
  test, and its first writer + first reader version recorded.

## Representation retirement

Retiring an encoder's emission of a representation NEVER retires the
decoder: the hostile-media corpus and the golden stores (12E.4) keep
every historical representation readable. A future release that cannot
decode a supported golden store fails CI.
