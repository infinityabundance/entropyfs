# Garbage collection

## 1. Why GC exists

The store is append-only; every mutation creates new records and leaves old
ones behind. Unreachable records are reclaimable space. Reachability from
**all roots** (current root + every snapshot) is the only source of truth;
reference counts are hints only.

## 2. Algorithm (tracing mark-and-sweep with compaction)

```text
mark:
  roots = { current root } ∪ { snapshot roots }
  worklist = roots
  while worklist not empty:
      obj = pop
      if obj not marked:
          mark(obj)
          push referenced objects (inode tree nodes, extent descriptors,
                                  raw/rANS payloads, models, bases, ...)
sweep:
  for each segment:
      live_ratio = marked_bytes_in_segment / segment_bytes
  target = segments with live_ratio < threshold (default 0.6) until
           reclaimable ≥ goal
compact:
  copy live records from target segments into a fresh segment
  (rewriting object locations in a new derived index)
commit:
  transaction: new root (new segment seq, new index epoch) → durable
  delete obsolete segments ONLY AFTER the new root is durable
```

## 3. Safety ordering

1. New root referencing the compacted copies is committed (ADR-0008).
2. Old segments are deleted only after that commit is durable.
3. A crash between 1 and 2: recovery sees the old root (old segments still
   present) or the new root (old segments now garbage) — both correct.
4. fsck verifies GC results: no reachable object in deleted segments, no
   unreachable-but-marked live objects.

## 4. Derived indexes

The object index (hash → segment location) is derived and disposable. GC
rebuilds it from segments rather than migrating it. This keeps GC simple and
makes fsck's independent rebuild the same code path.

## 5. Emergency reserve and watermarks

- **GC reserve**: configurable fraction of physical capacity (default 4%)
  reserved for GC's own compaction writes. Normal writes may not consume it.
- **High watermark** (default 92% used): background GC accelerates; the
  foreground optimizer stops creating speculative alternatives; writes may
  throttle (per-inode backpressure).
- **Critical** (reserve only): writes rejected with ENOSPC *before* any
  partial append; GC still runs.

## 6. Free-space accounting exposed

`status` reports: physical capacity, physical used, physical reclaimable,
logical bytes stored, effective ratio, reachable metadata/residual/model
bytes, snapshot-pinned bytes, GC reserve (ADR-0018).

## 7. Interaction with snapshots

Snapshot-pinned objects are marked from snapshot roots. GC never reclaims
them; `snapshot-pinned bytes` quantifies the cost of retained history.

## 8. GC correctness tests

- Simulated power-failure at every GC boundary (`BEFORE_OLD_SEGMENT_DELETE`
  etc.) in crash courts.
- fsck leak report must match GC's own reclaimable accounting.
- ENOSPC tests: fill to critical, verify reads still work and writes fail
  cleanly, then GC restores writability.
