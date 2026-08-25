# Write path

## 1. Request flow

```text
FUSE write(ino, offset, data)  (write-through; writeback off by default)
  → fuse::file::write
  → per-inode mutation lock (ADR-0013)
  → merge into 64 KiB logical extents:
        for each affected extent boundary:
            old = materialize(existing extent)     (or ZERO for holes)
            new = merge(old, offset, data)
            rep = encode_candidate(new)            (bounded search)
            validate: materialize(rep) == new      (exact, always)
            stage descriptor + payload records
  → transaction.commit()
  → ack
```

## 2. Foreground candidate pipeline (bounded, latency-conscious)

```text
1. exact dedup        content index hit → EXACT_REF (verify length + bytes)
2. ZERO / FILL / PERIODIC checks
3. SPARSE / PALETTE cheap structural candidates (rank/unrank, u128-checked)
4. BASE_RESIDUAL vs previous version / adjacent / file-family base
   (only when a basis exists and depth < 4)
5. rANS candidate     per-chunk model, exact cost
6. RAW                always available (escape hatch)
winner = min over candidates of J (ADR-0010), policy-aware
```

Foreground budget: fixed wall/budget counters (e.g., at most one rANS trial
plus structural trials; configurable). Deep searches are *not* foreground
work — they belong to the background optimizer.

## 3. Validation before commit (non-negotiable)

Every candidate is materialized and compared byte-for-byte (or by BLAKE3
of the materialized output) against the target chunk before its descriptor
may be committed. The invariant is enforced in one place
(`optimizer`/`store::transaction` validation gate) and tested adversarially
(fault-injected wrong-descriptor tests).

## 4. Partial extents and truncate

- Writes not aligned to chunk boundaries read-modify-write the affected
  chunk(s). Chunk boundaries are never user-visible (ADR-0006).
- `truncate` to a shorter size: the trailing partial chunk is re-encoded
  with the cut bytes; extents beyond the new EOF are dropped from the
  extent tree (the objects remain until GC — immutable store).
- `truncate` to a longer size: the extended region is a hole (ZERO).
- `fallocate(0)` (allocate) punches nothing physical — extents are
  assigned only on write; `fallocate(PUNCH_HOLE)` drops extents in range.

## 5. fsync semantics

`fsync` on an inode first performs a transaction commit of any pending
mutations (the write-through model means writes are already committed per
request; fsync then flushes the *superblock/segment* durability) and only
then returns. This matches the commit protocol ordering in
`docs/architecture/transaction-model.md`.

## 6. The background optimizer (Phase 4+, architecture fixed now)

```text
cold extent (age/access heuristic or DSFB recommendation)
  → deep candidate generation (rank/unrank families, residuals,
    universes, rebase, densification)
  → exact cost measurement
  → validate: hash(materialize(new)) == hash(materialize(old))
  → generation-CAS check (discard if a newer write landed)
  → commit replacement descriptor (same logical content ID)
```

The optimizer never defines correctness; it only proposes validated
alternatives. It runs on idle time with a configurable CPU budget, and is
throttled near the GC watermark (ADR-0009).
