# fsck

`entropyfs fsck` is not optional and not an afterthought: it is implemented
alongside the format, and it does not merely call the happy-path mounted
APIs. It independently walks and validates persistent structures.

## 1. Scope (what fsck verifies)

**Superblock / version**
- both slots: magic, version, checksum; generation selection logic;
  feature-bit compatibility.

**Segments / records**
- inventory of `segments/*.seg` vs superblock sequence;
- every record envelope: tag, version, header_len, stored_len,
  materialized_len, header CRC, payload CRC, content_id == BLAKE3(payload);
- record boundaries and sealed trailers; torn-tail detection;
- no record referenced by content_id that is absent or corrupt.

**Object graph**
- root reachability: every reachable object is present and valid;
- object IDs match content; no cycles (depth-bounded walk);
- reference depth ≤ format limit;
- no missing references.

**Inode invariants**
- mode/type consistency; size matches extent coverage; nlink consistency
  (directory entries pointing to the inode == nlink − 1 for regular files;
  directories nlink rules); timestamps sane; uid/gid range.

**Directories**
- entries sorted by name; no duplicate names; no `.`/`..` stored (they are
  synthesized); name validity (no `/`, no NUL, non-empty, ≤ 255 bytes);
  d_type consistency with the referenced inode's type.

**Extent trees**
- offsets strictly ordered; extents non-overlapping; extent lengths within
  class limits; extent coverage consistent with file size (last extent may
  be partial; nothing beyond size).

**Representations**
- descriptor validity (tag known, payload lengths consistent, u128 ranks in
  range, palette counts sum to len, periodic arithmetic exact);
- materialization smoke: optionally (deep mode) materialize every extent
  and verify BLAKE3 == recorded logical content id — this is the
  "a valid physical record that materializes to wrong logical bytes" check
  (ADR-0011).

**Snapshots / GC**
- snapshot roots are valid and fully reachable;
- leak report: records not reachable from any root (GC reclaimable);
- snapshot-pinned byte accounting matches the report.

## 2. Derived indexes are disposable

fsck rebuilds the object index from segments and compares with the
persisted derived index (if present); a mismatch is reported and the
derived index is rebuilt. Authoritative information is always
reconstructable from segments + root.

## 3. Repair modes

- `--check` (default): report only, exit code = error count.
- `--repair` (safe subset only):
  - rebuild derived indexes;
  - drop corrupt/unreachable garbage records via GC-style compaction
    (never touches records reachable from any root);
  - truncate torn segment tails;
  - refuse to "repair" anything that would change logical content.
- Anything else ⇒ `--repair` fails loudly with a specific error; manual
  recovery guidance is printed. fsck never silently rewrites data.

## 4. Error taxonomy

Every finding is typed: `Superblock`, `Feature`, `Segment`, `Record`,
`Checksum`, `ContentId`, `Reachability`, `Inode`, `Directory`, `Extent`,
`Representation`, `Materialize`, `Leak`, `Index`. Each carries a record
location (segment, offset) and a severity (`Fatal`, `Error`, `Warning`,
`Info`). Exit codes: 0 clean, 1 errors found, 2 usage, 3 io, 4 format
unsupported.

## 5. Testing

- fsck on clean stores ⇒ 0 findings;
- fault injection: flip bytes in segments/superblocks (deterministic seeds)
  ⇒ typed findings, no panics (fuzz target `fsck_walker`);
- fsck-vs-runtime agreement: the leak report matches GC's accounting;
- `--repair` round trips: repaired store passes fsck clean and all logical
  hashes still match.
