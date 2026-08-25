# EntropyFS changelog

## v0.5.1 (2026-08-25)

**Evidence only (no codec change, no format change):** the Phase-9F gap
decomposition is sealed into the tree court. Measured on the real source
tree (280 files, per-file writes):

- zstd -1 per-file 3.57×; zstd -1 per-file **with the same per-directory
  anchor** (`-D`, self-matches excluded) 3.91× — a mature coder extracts
  only ~8.5% from the shared-dictionary concept, less than EntropyFS's
  pool + deep pass already recovers (2.22× → 2.39×). **The anchor policy
  is not the cap.**
- The residual gap to per-file zstd is **~2/3 per-extent persistence
  overhead** — per-chunk multi-stream rANS model objects + descriptors on
  small files: 309.7 KB = 26.5% of the EntropyFS footprint (11.1% of
  logical; models alone 275.9 KB) — and **~1/3 coder quality**.
- Falsified: `scale_bits` does not shrink model objects (the model
  encoding is symbol-count-dominated: sb14 367 B vs sb8 295 B on a 3 KB
  file).

This refocuses the engineering target: **9G = amortized/shared entropy
models** (model objects are content-addressed and immutable, so N extents
can reference ONE persisted model object with no decoding chain), not
more anchor tuning or parser deepening.

## v0.5.0 (2026-08-25)

**Format note:** v0.5.0 retains **format version v1** (no format-version
bump) and adds one new **incompat representation feature** as an explicit
on-disk feature bit — `SEQUENCE_DEEP` (bit 14). An implementation that
does not understand that bit must refuse the store. Additive,
feature-gated, same pattern as v0.2.0 (bits 10/11), v0.3.0 (bit 12), and
v0.4.0 (bit 13). **Correction:** the superblock feature-bit tracker now
also records `SEQUENCE_SHARED_DICT` (bit 13) — the v0.4.0 write path
committed the descriptors but the tracker missed the bit, so an old
tool could open such a store and only fail at descriptor decode instead
of at the incompat gate. The tracker is now exhaustive over the
sequence families and regression-tested (`tests/shared_dict.rs`,
`tests/seqdeep.rs`).

Milestone content (Phase 9D + 9E):

- Phase 9D — the anchor POOL: `shared_dict_pass` now selects up to four
  per-directory anchors greedily by marginal savings against member
  incumbents, and each extent picks its best pool anchor during the
  rewrite. Heterogeneous directories (mixed styles/content classes) get
  per-file dictionary choice; the pool beats the single anchor by ~2×
  savings on a two-cluster directory fixture.
- Phase 9E — `SEQUENCE_DEEP` (tag 0x11, feature bit 14): repcodes
  (REP0/REP1 copies carry no offset symbol) + extended length codes (one
  XCOPY/XLIT plus a u16 extra instead of runs of 131-byte continuation
  commands) fed by a deep background matcher (hash chains to depth 256,
  lazy parsing with a minimum-gain threshold, rep-distance priority).
  Background-only; terminal (depth 0).
- Ablation: `allow_sequence_rans_deep` gate, `no-deep` leave-one-out
  mode, cumulative-ladder step E4 (post-registration extension),
  `raw_sequence_deep()` standalone baseline (foreground write + a
  background pass, since the family is background-only).
- Evidence (sealed campaign, 7/7 admission): tree court on the real
  source tree — EntropyFS per-file writes **2.194× → 2.354× post-GC**
  after the shared-dict pool + deep background pass (151 extents, ~93.3
  KiB saved); standalone deep floor **3.786× vs the fast floor 3.736×**
  on the src pack (deep wins all chunks); ladder E4 densifies the
  structured corpus 50,528 → 50,238 B. Archived under
  `evidence/performance/` (`INDEX.md` is authoritative).

## v0.4.0 (2026-08-25)

**Format note:** v0.4.0 retains **format version v1** (no format-version
bump) and adds one new **incompat representation feature** as an explicit
on-disk feature bit — `SEQUENCE_SHARED_DICT` (bit 13). An implementation
that does not understand that bit must refuse the store. Additive,
feature-gated, same pattern as v0.2.0 (bits 10/11) and v0.3.0 (bit 12).

Milestone content (Phase 9C):

- Phase 9C — the shared amortized dictionary: `SEQUENCE_SHARED_DICT` (tag
  0x10, feature bit 13). Local history + optional previous same-file chunk
  + a *shared cross-file dictionary* in one stream, with a third
  copy-source symbol (`SRC_SHARED`, absolute offset). The background
  `shared_dict_pass` selects a per-directory anchor — an existing terminal
  chunk that maximizes savings against member incumbents (not against raw
  bytes, which under-measured the fix by 12×) — and rewrites extents
  strictly-cheaper with the same CAS-gated, byte-validated commit path as
  `optimize_pass`. Anchors are terminal (v1), so rewritten extents carry
  depth ≤ 1; GC pins the anchor chunk through the reference closure even
  after its owning file is deleted (regression-tested).
- The 9C evidence gate is sealed in the campaign's **tree court**: 279/282
  real-tree files are single-chunk, so the previous-chunk dictionary gets
  almost no opportunity on a real tree, and the packed-stream density is
  mostly cross-FILE structure. Per-file zstd baselines: whole 4.978× /
  per-file 3.541× / per-64KiB 3.991× (-1). EntropyFS per-file writes
  **2.182× → 2.328× post-GC** after the shared-dict pass (102 extents,
  ~85.2 KiB saved). The mechanism is proven by synthetic fixtures
  (random-looking shared headers → large wins); the modest real-tree gain
  is recorded as-is.
- Ablation: `allow_shared_dict` gate, `no-shared-dict` leave-one-out mode,
  cumulative-ladder step E3 (post-registration extension), DSFB channel
  P8 (`shared_dict`).
- Evidence: the sealed Phase-9C campaign `campaign-1787679299-8d6e147`
  (7/7 admission) under `evidence/performance/` (`INDEX.md` is
  authoritative); the two intermediate tree-court measurements (flat
  placement; RAW-scored anchors) are amended in `INDEX.md`, never
  silently kept.

## v0.3.0 (2026-08-25)

**Format note:** v0.3.0 retains **format version v1** (no format-version
bump) and adds one new **incompat representation feature** as an explicit
on-disk feature bit — `SEQUENCE_DICT` (bit 12). An implementation that
does not understand that bit must refuse the store (the superblock's
feature-bit gate, `docs/format/compatibility.md`). This follows the same
additive, feature-gated pattern as the v0.2.0 correction
(`SEQUENCE_RANS` bit 10, `SPARSE_BLOCK64` bit 11).

Milestone content (Phase 9A + 9B):

- Phase 9A — the incompressible physical floor: `Tx::commit_deferred`
  prunes transaction-local COW intermediates (B-tree nodes and inode
  objects unreachable from the final root) before append; ENOSPC guard on
  the pruned footprint; `unreachable_bytes_by_record_tag` evidence.
  urandom reaches 0.997× reachable / 1.00× total backing / 1.00×
  allocated blocks.
- Phase 9B — `SEQUENCE_DICT` (tag 0x0F, feature bit 12): cross-chunk
  dictionary match coding. The previous same-file chunk is used as an
  external ≤64 KiB dictionary alongside local history: a fourth
  *copy-source* stream says whether each u16 is a LOCAL backward distance
  (byte-progressive) or a DICT absolute offset; a DICT match longer than
  131 bytes advances the offset across continuation commands. Reference
  depth is accounted like a base chain (`dictionary chain + 1 ≤
  max_reference_depth`), so cross-chunk dictionary references can never
  defeat bounded random access; periodic terminal anchors emerge
  automatically at the depth cap.
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
- Evidence: `campaign-1787674068-4892644` (9A floor) and
  `campaign-1787676607-8250f6b` (9B, 7/7 admission — src corpus 4.070×
  with SequenceDict vs standalone SequenceRans 3.627× and
  zstd-per-64KiB -1 3.848×). Archived under `evidence/performance/`
  (`INDEX.md` is authoritative).

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
