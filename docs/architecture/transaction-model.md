# Transaction model

## 1. Principles

- **Append-only immutable records.** All persistent state changes are new
  records in append-only segment files. Nothing is overwritten in place
  except the two superblock slots.
- **Atomic root.** A commit is: build a new immutable root object that
  references the new state, then flip the inactive superblock slot to point
  at it with a higher generation.
- **Ordered durability** so that a crash can never expose a root whose
  referenced records are not durable.

## 2. Commit protocol (exact order)

```text
1. append all new immutable records to the current segment
2. fdatasync(segment)                                  [AFTER_SEGMENT_FDATASYNC]
3. if a new segment file was created:
     fsync(parent dir of segments/)                   [AFTER_SEGMENT_DIR_FSYNC]
4. build new root object (its records are already appended)
5. write inactive superblock slot {generation N+1, root_id, ...}
                                                        [AFTER_SUPERBLOCK_WRITE]
6. fsync(superblock file)                              [BEFORE_SUPERBLOCK_FSYNC]
                                                        [AFTER_SUPERBLOCK_FSYNC]
7. mark transaction durable; only now ack to caller
```

Bracketed names are crash-court injection points
(`docs/recovery/crash-consistency.md`).

## 3. Superblock slots

Two slots at fixed offsets (A, B). Each slot: magic, format version,
feature bits, UUID, generation, root object ID, current segment sequence,
creation parameters, integrity checksum. Commit alternates slots by
`generation & 1`.

## 4. Recovery (mount-time)

```text
read slot A, validate checksum+magic        → candidate if valid
read slot B, validate checksum+magic        → candidate if valid
choose the valid candidate with the highest generation
if both valid and equal generation → either is fine (idempotent commit)
rebuild derived index by scanning segments (records → hash index)
reject unsupported incompat feature bits
```

The crash invariant — *complete previous transaction or complete new
transaction, never a hybrid* — holds because:

- the new root object's transitive records were appended and `fdatasync`ed
  in steps 1–3, strictly before the slot write in step 5;
- the slot write is atomic-ish in practice (single aligned sector-sized
  structure with its own checksum; a torn write fails validation and the
  other slot remains the valid highest generation);
- records appended but unreferenced by any root are garbage by definition
  and reclaimed by GC — they can never become authoritative.

## 5. Concurrency

A single narrow commit coordinator serializes steps 1–7 (ADR-0013).
Mutations build their new nodes outside the coordinator; only the append +
flip is serialized. Multiple concurrent mutations of disjoint inodes share
segments safely (per-segment append lock).

## 6. Generation / CAS

Every descriptor read for background optimization records the root
generation `G`. Before the optimizer commits a replacement it re-checks the
generation; a newer foreground commit invalidates the optimization. This is
the only synchronization the optimizer needs (ADR-0013).

## 7. Segment rollover

When the current segment reaches its size cap (initial default 128 MiB,
benchmarked): seal it (append segment trailer with record count + CRC),
create the next segment file, and ensure the directory entry is durable
(step 3). The superblock's `current segment sequence` is advanced in the
next commit.

## 8. ENOSPC safety

Before accepting a transaction, the store checks that the emergency GC
reserve (ADR-0009) plus the worst-case transaction size fits; otherwise the
write is rejected with ENOSPC *before* any partial append. Never corrupt
existing data because storage is full.
