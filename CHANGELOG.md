# EntropyFS changelog

## Unreleased (Phase 9B — SequenceDict)

- `SEQUENCE_DICT` (tag 0x0F, feature bit 12): cross-chunk dictionary match
  coding. The previous same-file chunk is used as an external ≤64 KiB
  dictionary alongside local history: a fourth *copy-source* stream says
  whether each u16 is a LOCAL backward distance (byte-progressive) or a
  DICT absolute offset; a DICT match longer than 131 bytes advances the
  offset across continuation commands. Depth-capped like base chains
  (`dictionary chain + 1 ≤ max_reference_depth`), so cross-chunk
  dictionary references can never defeat bounded random access; periodic
  terminal anchors emerge automatically at the depth cap.
- Write-path integration: the batch overlay provides the previous chunk's
  bytes nearly free (`PendingBatch.depths` registers in-batch reference
  depths); the background optimizer re-encodes RAW extents to
  SEQUENCE_DICT via the committed previous chunk.
- Correctness fixes surfaced by SequenceDict:
  - `flatten_if_deep` now validates the flattened update through a
    resolver that sees the update's own staged objects (previously
    materializing through the bare store failed on object-backed
    families with `MissingObject`).
  - `current_persisted_bytes` now accounts every object a descriptor
    references (RAW/RANS were the only families counted; object-backed
    incumbents looked nearly free and blocked densification).
  - Background candidate ordering now uses FULL persisted bytes (the
    foreground keeps marginal bytes so reuse wins); a chunk whose
    incumbent's objects already exist is no longer immune to
    replacement.
- Ablation: `allow_sequence_dict` gate, `no-sequence-dict` leave-one-out
  mode, cumulative-ladder step E2 (post-registration extension).

## v0.2.0 (2026-08-25)

**Format note (correction to the release commit's wording):** the v0.2.0
release commit stated "no on-disk format changes." That was imprecise:
v0.2.0 retains **format version v1** (no format-version bump) but adds new
**incompat representation features** as explicit on-disk feature bits —
`SEQUENCE_RANS` (bit 10) and `SPARSE_BLOCK64` (bit 11). An implementation
that does not understand those bits must refuse the store (the
superblock's feature-bit gate, `docs/format/compatibility.md`). This is an
additive, feature-gated on-disk format *extension*, not a layout rewrite;
the correction is recorded here rather than rewriting the commit.

Milestone content (Phase 8, M1–M5 + 8A/8B/8C):

- Concurrency refactor: interior-mutability `Store` (root/superblock
  behind `RwLock`, 64-shard object index, per-inode lock table, short
  commit coordinator); reads traverse root snapshots without the global
  writer lock; FUSE writeback-cache negotiation.
- Write aggregation: `write_region_batch` group commit with in-batch
  overlay; deferred durability with fsync barrier; in-batch dedup
  visibility; transaction-local CAS canonicalization (one physical record
  per content id per transaction); marginal-cost candidate selection
  (existing objects cost zero).
- `SEQUENCE_RANS` (tag 0x0D, feature bit 10): the local-match +
  entropy compression floor (LZ77-style hash-chain matcher with three
  rANS-coded or raw streams).
- `BASE_SEQUENCE` residual (kind 0x04 inside `BASE_RESIDUAL`): shift-aware
  copy/literal deltas for versioned data.
- `SPARSE_BLOCK64` (tag 0x0E, feature bit 11): blockwise-64 enumerative
  sparse coding.
- Derived chunk-index rebuild in GC: the chunk index is pruned to the
  reachable set (live extents + transitive reference closure) so
  overwritten unsnapshotted content cannot grow it permanently.
- Evidence: strict cumulative ablation ladder A0–A8 (+ post-registration
  E1 SequenceRans step) beside the leave-one-out table; attribution split
  into CAS object sharing (store invariant) vs EXACT_REF aliasing (gated
  representation); per-corpus post-GC physical footprint (reachable /
  total backing / allocated blocks); the zstd-per-64KiB diagnostic.
  Archived under `evidence/performance/` (`INDEX.md` is authoritative).

## v0.1.0 (2026-08-25)

Initial publication: single-crate native-Rust crash-consistent FUSE
filesystem (`entropyfs`). Format v1, representation set 0x01–0x0C
(ZERO/FILL/RAW/RANS/EXACT_REF/BASE_RESIDUAL/SPARSE/PALETTE/PERIODIC/
ENTROPY_REF/INLINE/PERMUTATION), dual-superblock commit protocol,
reachability GC, snapshots, fsck, crash courts.
