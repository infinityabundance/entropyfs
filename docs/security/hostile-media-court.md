# Hostile-media court (Phase-11A)

The backing store is treated as **untrusted/corrupt input** (the threat
model's first line). This court attacks the one dimension the valid-path
suite barely exercised: input EntropyFS did not produce itself. It lives
in `src/tests/hostile_media/` and is driven by proptest — the in-package
coverage-guided harness (ADR-0001 keeps one package; the driver is the
existing dev-dependency rather than a `fuzz/` Cargo package).

## 1. The oracle

```text
arbitrary hostile bytes
        ↓
persistent decoder / graph traversal
        ↓
must terminate boundedly
        ↓
Ok(valid bounded result)
    OR
typed rejection

NEVER:
panic, OOM, infinite loop, unbounded recursion, unbounded CPU,
silent wrong bytes
```

Two refinements keep the oracle honest:

- We never assert that arbitrary data must be rejected: some random
  inputs legitimately describe valid content. `Either` (bounded-valid or
  typed-reject) is the default outcome class; `MustAccept`/`MustReject`
  are asserted only where the format's outcome is fully determined.
- "Never return bytes inconsistent with the descriptor's authenticated
  content identity" is asserted **store-side**, through the opened
  store's own view: when every reachable extent binds to the chunk index
  (materializes to bytes whose content id maps to an index entry that
  materializes to the same bytes), the reads must succeed and return
  exactly those authenticated bytes.

## 2. The three courts

### 2.1 Descriptor court (`descriptor_court.rs`)

Every bounded byte string through `format::descriptor::decode` under
deliberately tight limits **and** the real defaults. The contract
(ADR-0016 "typed error, never panic"):

- decode-OK ⇒ `rep.validate(&limits)` succeeds (enforced inside `decode`
  since Phase-11A — see §4);
- the encoded size stays within the descriptor cap;
- every derived size stays within the declared bounds;
- the encoding is **canonical**: re-encoding the decoded representation
  reproduces the exact input bytes.

Strategy mix: uniform noise (every possible bounded byte string),
seeded mutation of one real descriptor of every family (flip / set /
insert / delete / truncate / overwrite), valid seeds plus trailing
garbage, and random sub-slices. Deterministic companions: truncation at
every byte boundary of every canonical descriptor, and the 8192/8193
descriptor-cap boundary.

### 2.2 Materialization-graph court (`graph_court.rs`)

A representation bomb is inherently graph-shaped: `EXACT_REF`,
`BASE_RESIDUAL`, `SequenceDict` and `SequenceSharedDict` resolve other
chunks, and locally valid descriptors compose into globally invalid
graphs (the Phase-10E diamond-depth bug is exactly this class). The fuzz
input defines a **descriptor table** (id → descriptor bytes), an
**object table** (id → bytes) and an **entry descriptor id**; the entry
is materialized through `HostileResolver`, an in-memory
`DecoderContext` whose `fetch_descriptor` decodes hostile bytes through
the real codec — the same path a hostile store's chunk index takes.

The corpus pins the valid seeds' exact materialized bytes (the §32
byte-exactness contract) and carries the structural bombs:

- self-reference `A → A`; two-node cycle `A → B → A`;
- depth bomb (20-long chain); chains of exactly 4 (accepts) and 5
  (typed `DepthExceeded`) at the default cap;
- diamonds (the same node reached shallow and deep; the deepest path
  must be reported);
- shared-dict double branches; invalid dictionary → invalid dictionary;
- valid descriptor → corrupted model object;
- valid sequence descriptor → hostile command stream (COPY before local
  history, exhausted offset/literal/source streams, dictionary copies at
  the exact end and one byte beyond, `SparseBlock64` popcount > 64 and
  rank out of range, reserved deep command bytes);
- model objects just below / at / above `max_model_bytes`.

### 2.3 Store court (`store_court.rs`)

The **CRC-aware distinction** — the user's critical point: "flip random
bits in a store image" as the principal strategy would fuzz CRC32C. The
envelope rejects the vast majority of mutations before the deep parsers
see them. Two complementary courts therefore run over real tiny stores
(dirs, files with rANS/sequence/RAW payloads, xattrs, snapshots, epoch
ops, a checkpointed durability barrier, and an un-checkpointed
mutation-log tail):

- **Physical corruption court**: mutate record/superblock bytes and
  leave the CRC (and the content-id binding) broken. The expectation is
  integrity rejection: a payload-region flip makes `record::decode` fail
  at the envelope, so open and fsck both reject typed. Length-field
  flips may degrade to a torn tail — the crash-consistency design's
  *admissible* recovery (the store falls back to the complete previous
  state) — asserted as boundedness.
- **Semantic adversarial court**: mutate descriptor / tree / model /
  inode / mutation-log payloads and RECOMPUTE the envelope CRC (and the
  content id), forcing the hostile payload through the deeper parsers:
  descriptor codec, B-tree walks, inode decode, materializer, epoch
  replay. The acceptance criterion is the hostile-media oracle; the
  authenticated-bytes clause is checked through the opened store's own
  view (§1).

Dedicated exhibits: B-tree fanout exactly 4096 (decodes) and 4097
(rejected typed), unsorted and duplicate B-tree keys, a valid-CRC
envelope containing a malicious (self-referential) descriptor, and
mutation-log duplicate / non-monotonic sequence numbers.

The **whole-store mutator** (proptest) applies seeded recipes over the
tiny-store image — flip, truncate, splice, duplicate, reorder, alter
lengths, replace tags, replace payloads, and recompute the CRC
selectively (both flavors) — then runs open / fsck / materialize.

## 3. Corpus discipline

`corpus.rs` is the permanent hand-crafted exhibit set:

- one canonical descriptor of every representation family (ZERO, FILL,
  INLINE, RAW, RANS, EXACT_REF, BASE_RESIDUAL × every residual kind,
  SPARSE, PALETTE, PERIODIC, ENTROPY_REF, PERMUTATION, SEQUENCE_RANS,
  SPARSE_BLOCK64, SEQUENCE_DICT, SEQUENCE_SHARED_DICT, SEQUENCE_DEEP) —
  the fuzz seeds, so mutation penetrates deep variant-specific logic
  instead of spending all day discovering valid tags and lengths;
- graph seeds with real rANS/sequence streams that materialize to pinned
  bytes;
- every boundary the format defines: descriptor size 8192/8193, unknown
  tags, truncation at every boundary, trailing garbage, logical lengths
  at/over the chunk cap, sparse/palette/periodic/permutation rank and
  count violations, residual edit ordering/overlap/bounds, sequence
  stream sanity, dictionary bounds, model size bounds, base length
  bounds, entropy-ref residual mismatches.

## 4. Layering change the court required

The court's descriptor oracle (decode-OK ⇒ validate-OK) exposed a
layering gap: `decode` accepted descriptors that `validate` rejects (the
write path gated with `validate`; the read path never did). `decode` now
takes the full `&Limits` and passes every decoded representation through
`validate` before returning — the read path never hands an unvalidated
descriptor to the materializer, matching the write path's gate. The
on-disk format is unchanged; the parser is stricter about *accepting*,
never about *encoding* (encoders are gated by `validate` and the
round-trip tests prove every encoded descriptor decodes).

## 5. Evidence

`tools/hostile-court.sh` runs the full court with scaled `PROPTEST_CASES`
in release mode and archives the receipts under
`evidence/hostile-media/court-<ts>-<rev>/` (revision, kernel, unix time,
per-test results). The release convention: the court's `resource-bounds`
section is only ever written as "implemented" when the sealed evidence
exists in the repository.
