# Information accounting

How EntropyFS counts every persistent bit, and why.

## 1. Per-extent accounting

For each extent, after a representation is chosen, the following are
recorded exactly:

```text
logical_bytes          materialized length (the application-visible X)
descriptor_bytes       encoded representation descriptor (tags, lengths, coords)
model_bytes            rANS model state attributable to this extent
residual_bytes         residual payload (XOR/edit literals, patch data)
seed_state_bytes       seed/state/coordinate bits for ENTROPY_REF
reference_bytes        base/target content-ID references (32 B each)
configurational_bytes  rank/unrank coordinate bytes
integrity_bytes        attributable checksums/hashes (CRC32C per record,
                       BLAKE3 content ID where persisted per chunk)
```

`persisted_bytes(extent) = descriptor + model + residual + seed + reference
+ configurational + integrity` (GC overhead accounted at the FS level).

## 2. Filesystem-level accounting

```text
logical bytes stored          Σ logical_bytes over reachable extents
physical reachable bytes      Σ persisted_bytes over reachable records
total backing-store bytes     physical capacity of the backing store
metadata bytes                inode/directory/extent-tree objects
snapshot-pinned bytes         bytes reachable only via snapshots
unreachable / GC bytes        records not reachable from any root
GC reserve                    reserved physical capacity (ADR-0009)
```

## 3. Derived metrics (always defined explicitly)

```text
effective ratio        = logical_bytes / physical_reachable_bytes
dedup savings          = bytes NOT re-persisted because of EXACT_REF/RAW sharing
rANS savings           = logical − (model + encoded) over RANS extents
configurational savings= logical − (rank + literals + descriptor) over
                         SPARSE/PALETTE/PERMUTATION/PERIODIC extents
```

Ablation rules (§43) forbid crediting one mechanism with another's savings:
dedup savings are computed *excluding* rANS extents; rANS savings exclude
referenced bases; configurational savings exclude anything already counted
as dedup.

## 4. What is NOT counted as savings

- The content index (it is derived and rebuildable) — but its marginal RAM
  is reported in `status`.
- Any DSFB runtime state (deleting it must not change decodability, so it
  is not stored entropy).
- Model *sharing* across extents is real savings only when the model object
  is referenced, not duplicated; `model_bytes` then counts the reference,
  and the shared object's bytes are counted once (dedup applies).

## 5. Accounting invariants (tested)

1. `Σ reachable persisted_bytes ≤ total_backing_bytes − gc_reserve`.
2. `physical_reachable_bytes = Σ over reachable records of their persisted
   bytes` (fsck recomputes this independently).
3. For every extent, `materialize(descriptor)` has length `logical_bytes`
   and BLAKE3 equal to the recorded logical content ID (ADR-0011).
4. Effective ratio is reported with the same units on both sides (bytes).

These invariants are enforced by property tests and fsck
(`docs/recovery/fsck.md`).
