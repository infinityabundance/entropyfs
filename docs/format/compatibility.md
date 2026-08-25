# Format compatibility

## 1. Versioning model

- `format_major` bump ⇒ incompatible; new tool refuses old stores and old
  tools refuse new stores.
- `format_minor` bump ⇒ additive, backward-compatible (old tools may mount
  new stores if feature bits allow).
- Feature bits: three sets, mirroring ext4/btrfs semantics:

| Set | Unknown bit ⇒ |
|-----|----------------|
| `compat` | still mount rw, feature may simply not activate |
| `ro_compat` | mount read-only, refuse rw |
| `incompat` | refuse mount entirely |

Feature bit registry (v1):

| Bit | Set | Meaning |
|-----|-----|---------|
| 1 | incompat | `CHUNK_4K` chunk class present in a store |
| 2 | incompat | `CHUNK_16K` chunk class present |
| 3 | incompat | `CHUNK_256K` chunk class present |
| 4 | incompat | `ENTROPY_REF` descriptors present |
| 5 | incompat | `PALETTE` descriptors present |
| 6 | incompat | `PERMUTATION` descriptors present (reserved; not emitted in v1) |
| 7 | ro_compat | `ENCRYPTED` record payloads (AEAD) — ro without key material |
| 8 | compat | `EXTENT_DELTA_INDEX` derived index present (disposable) |
| 9 | compat | `OPTIMIZER_REWRITE` history markers present |

`CHUNK_64K` is the baseline and needs no bit (v1 always supports it).

## 2. Rules

1. The superblock's feature bits are authoritative; descriptors referencing
   features not granted by the bits are format violations (fsck error).
2. A store is written only by the format version that created it unless a
   ro_compat/incompat gate explicitly permits otherwise; the superblock is
   rewritten at upgrade with a documented migration (never in-place format
   surgery).
3. New chunk classes and new representation tags MUST be introduced with a
   feature bit first, descriptors second.
4. Metadata entropy coding (§28) applies only to *derived* metadata
   streams; bootstrap metadata remains independently recoverable, so a
   decoder can always locate itself.
5. `UniverseId` and `TransformId` registries are part of the format: a
   descriptor referencing an unknown universe/transform id is a typed
   decode error, never a panic.

## 3. Downgrade policy

Downgrading the tool against a newer store is refused by the superblock's
feature bits (unknown incompat ⇒ refuse). This is the safe default; forced
downgrade is never automatic.

## 4. Testing

- Format golden tests: every encoder's bytes are pinned as hex fixtures so
  accidental format drift is caught (fixtures in `src/tests/`).
- Cross-version tests: v1 stores decode with the pinned v1 decoder only
  until v2 exists; the compatibility matrix is extended with each minor.
