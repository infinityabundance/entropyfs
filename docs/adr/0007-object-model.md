# ADR-0007: Immutable content-addressed object model, COW mutation, atomic roots

**Status:** accepted · **Date:** 2026-08-25

## Context

EntropyFS needs snapshots, cheap clones, rollback, exact deduplication, and
crash-consistent commits without a giant mutable metadata database.

## Decision

Persist an **immutable, content-addressed object graph**:

```text
FilesystemRoot
  ├── inode index tree (ino → inode object)
  ├── snapshot tree (name → root)
  ├── model index tree (model id → model object)
  └── allocator/segment state

Inode
  ├── metadata (mode, uid/gid, times, size, nlink, rdev)
  ├── xattr tree reference
  ├── directory tree reference      [directory]
  └── extent tree reference         [regular file]

Extent
  └── Representation descriptor
```

- A mutation creates new nodes along the affected path; unchanged nodes are
  shared by reference (persistent copy-on-write).
- Object identity is the BLAKE3 hash of the object's serialized record
  payload (logical content ID for data chunks; structural ID for metadata
  nodes). Two identical nodes collapse to one object.
- The filesystem root is an immutable object; commits atomically replace the
  root reference in the superblock (ADR-0008).
- The in-memory object index (hash → segment location) is a **derived,
  disposable** index rebuilt from segment records at mount; authoritative
  information is always reconstructable from segments.

## Consequences

- Snapshots = pin an old root; clones = copy an inode reference; rollback =
  repoint the root; dedup = hash-level aliasing with byte-level verification.
- GC is reachability over this graph (ADR-0009); reference counts may exist
  as hints but are never the source of truth.
