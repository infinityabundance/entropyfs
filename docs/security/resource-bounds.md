# Resource bounds

Every limit below is enforced **before** the allocation/loop it guards.
All are configurable at mkfs/mount; defaults are conservative.

## 1. Decode/materialization limits

| Limit | Default | Guards |
|-------|---------|--------|
| `max_chunk_size` | 262144 (256 KiB) | largest logical chunk class |
| `max_chunk_class` | 64 KiB default write class | ADR-0006 |
| `max_descriptor_bytes` | 8192 | descriptor parse |
| `max_reference_depth` | 4 | EXACT_REF/BASE_RESIDUAL chains |
| `max_decode_work` | 64 Mi operations | op-budget counter (every materialize step decrements) |
| `max_alloc_bytes` | 1 MiB | any single decode allocation |
| `max_fanout` | 4096 | B-tree node entry count, residual edit count |
| `max_model_bytes` | 2048 | rANS model object |
| `max_inline_bytes` | 4096 | INLINE representation |

## 2. Filesystem limits

| Limit | Default |
|-------|---------|
| `max_file_size` | 16 TiB (u64 offsets, but enforced) |
| `max_name_len` | 255 bytes |
| `max_xattr_value` | 65536 bytes |
| `max_xattr_count` | 1024 per inode |
| `max_symlink_target` | 4095 bytes |
| `max_entries_per_dir` | 16 M |

## 3. Cache budgets (ADR-0014)

| Cache | Default budget |
|-------|----------------|
| materialized chunks | 256 MiB |
| metadata nodes | 64 MiB |
| models | 32 MiB |
| descriptors | 16 MiB |

Every cache is bounded; eviction is LRU-ish and never affects correctness.

## 4. Store/GC limits

| Limit | Default |
|-------|---------|
| `segment_size` | 128 MiB (benchmarked) |
| `gc_reserve_ratio` | 0.04 (4% of physical capacity) |
| `gc_high_watermark` | 0.92 |
| `gc_target_segment_ratio` | 0.6 |
| `max_segments` | 1 M |

## 5. Enforcement points

- Persistent-data parsers (`src/format/*`): all lengths checked with
  `checked` arithmetic against these limits before `Vec` allocation.
- Materializer (`src/core/materialize.rs`): output length checked before
  allocation; work budget decremented per step; depth checked per
  reference.
- Store (`src/store/*`): record lengths checked against segment bounds;
  transaction worst-case size checked against free space (ADR-0009).

## 6. Testing

IMPLEMENTED — the Phase-11A hostile-media court
(`src/tests/hostile_media/`, spec in `docs/security/hostile-media-court.md`;
sealed evidence under `evidence/hostile-media/`):

- **Descriptor court** (`descriptor_court.rs`): every bounded byte string
  through `format::descriptor::decode` under deliberately tight limits
  and the defaults; decode-OK ⇒ `validate` OK (enforced inside `decode`),
  encoded size within the descriptor cap, byte-exact canonical
  re-encode. Corpus: one real descriptor of every family (all 17 +
  every residual kind), truncated at every byte boundary, plus the
  8192/8193 descriptor-cap boundary and every rank/count/ordering
  violation the format defines.
- **Materialization-graph court** (`graph_court.rs`): a fuzz-defined
  descriptor table + object table + entry descriptor materialized
  through an in-memory hostile resolver (`HostileResolver`, mirroring
  the store's `DecoderContext`). Materialization either succeeds within
  all declared resource bounds (valid seeds pin the exact bytes) or
  returns a typed error — never panic, never OOM, never unbounded CPU
  (the budget/depth/allocation counters are what this court proves).
  Attacks self-reference, cycles, depth bombs, chains at exactly 4/5,
  diamonds (deepest-path), shared-dict double branches, invalid
  dictionary chains, corrupted models, hostile command streams.
- **Store court** (`store_court.rs`): the CRC-aware distinction over real
  tiny stores — physical corruption (broken envelope → integrity
  rejection) vs semantic adversarial mutation (recomputed envelope CRC →
  deep parsers), plus the whole-store mutator (flip / truncate / splice
  / duplicate / reorder / alter lengths / replace tags / replace
  payloads / recompute CRC selectively) driving open/fsck/materialize,
  with the authenticated-bytes clause checked store-side. B-tree fanout
  4096/4097, unsorted/duplicate keys, a valid-CRC envelope containing a
  malicious descriptor, and mutation-log duplicate / non-monotonic
  sequences.

The court asserts "typed error, never panic, never OOM" on malformed
inputs (ADR-0016), and its `resource-bounds` claim is only ever written
as implemented when the sealed evidence exists in the repository.

The rest of the suite (implemented today):

- property tests asserting `allocation ≤ limit` for the parse paths
  (proptest round trips, `src/tests/`);
- crash courts over both io backends with byte-identical store parity
  (`src/tests/crash_recovery.rs`, `src/tests/io_backend_parity.rs`);
- the write path's structural gate (`Representation::validate`) before any
  descriptor is persisted (`put_chunk_in_tx`), and the materializer's
  independent bound checks (output size, allocation size, reference
  depth, work budget) on every read path.
