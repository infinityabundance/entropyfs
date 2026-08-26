# Threat model

The backing store is treated as **untrusted/corrupt input**. A malicious or
faulty disk image must never cause: unbounded allocation, unbounded CPU,
panics, privilege escalation, or wrong logical bytes that pass integrity
checks.

## 1. Assets

- Logical file contents (confidentiality when encryption is on; integrity
  always).
- The ability to mount and use the store (availability).
- Decoder determinism (a store must decode identically on every host).

## 2. Attack/defect surface

| Surface | Threat | Defense |
|---------|--------|---------|
| Superblock | forged/torn slot, wrong generation, downgrade attack | CRC32C, dual slots, feature-bit rejection, generation selection (ADR-0008) |
| Segment records | forged lengths, huge declared lengths, wrong content_id | checked arithmetic before allocation, limits (docs/security/resource-bounds.md), content_id == BLAKE3(payload) |
| Descriptors | output-length bombs, deep reference chains, cycles, huge fanout | pre-allocation length checks, depth cap 4, budget counters, cycle checks |
| Rank/unrank | rank ≥ C(n,k), palette counts mismatch, overflow | checked u128, in-range validation, reject-on-overflow |
| rANS models | corrupt freq tables, total ≠ 2^s, huge tables | `malformed::validate_freq_model` before table build; model size limits |
| Residuals | residual bombs (edit count ≫ chunk size), overlapping ranges | count/length checks, overlap resolution rules |
| xattrs/filenames | malicious names (`/`, NUL, >255), huge xattr values | byte-level name rules (never UTF-8 assumptions), size limits |
| Symlinks | symlink confusion on the host side | standard VFS semantics; no special handling of link targets in the store |
| Content index | hash-table memory exhaustion | bounded index, derived/disposable (ADR-0007) |
| Transaction replay | replay of old superblock after rollback | generation monotonicity; snapshot semantics are explicit (a rollback is a *feature*, so superblock replay protection is scoped: see below) |
| GC | delete of live segments | mark from all roots; delete only after new root durable (ADR-0009) |
| Encryption (future) | dedup side channel | documented; per-tenant keys or dedup-off |

## 3. Transaction replay note

Rollback via an old root is an intentional snapshot feature, so EntropyFS
does not fight replay wholesale. What it prevents is *accidental* replay:
generation must never decrease except through the explicit snapshot-rollback
operation, and fsck reports a generation regression as a finding.

## 4. Cryptographic posture

- Content IDs: BLAKE3-256 (preimage-resistant; used for identity and
  integrity of logical content).
- Physical integrity: CRC32C (error detection, not adversarial security).
  For adversarial integrity, the future AEAD layer (ADR-0015) is the
  mechanism; until then, physical corruption is detected but an attacker who
  can rewrite the store can also rewrite CRCs — the threat model for
  unencrypted stores is *accidental* corruption, and for adversarial
  tampering it is *detection of logical-content mismatch* via content IDs +
  fsck deep materialization.

## 5. Resource exhaustion

See `docs/security/resource-bounds.md`. No allocation derives directly from
a disk field without a limit check.

## 5b. Fuzz courts and the CRC-aware distinction

The hostile-media court (`docs/security/hostile-media-court.md`) is the
adversarial input suite. It runs TWO complementary corruption flavors,
and the distinction matters: **physical corruption** (mutate bytes, leave
CRC32C broken) is the bit-rot court — the envelope rejects before the
deep parsers run; **semantic adversarial mutation** (mutate descriptor /
tree / model / inode / mutation-log payloads and RECOMPUTE the envelope
CRC and content id) forces the hostile payload through the deeper
parsers. "Flip random bits in a store image" alone would mostly fuzz
CRC32C; the semantic flavor is what reaches the descriptor codec, the
B-tree walks, the inode decode, the materializer and the epoch replay.
The acceptance criterion for both: never panic, never hang, allocations
bounded, and never return bytes inconsistent with the descriptor's
authenticated content identity (checked store-side through the opened
store's own view).

## 6. Dependency trust

`cargo-deny` + `cargo-audit` gate the dependency graph (ADR-0017). The
graph is small and intentional. `ryg-rans-rs` core is
`forbid(unsafe_code)`; the SIMD crate's unsafe surface is ledgered upstream;
we do not enable SIMD in Phase 1.

## 7. Process model

FUSE daemon runs as the mounting user (no `allow_other` by default).
The store directory's permission bits protect the backing files; the daemon
validates that the store is not mounted recursively (backing store must not
reside under the EntropyFS mount).
