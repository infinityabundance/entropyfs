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

- Fuzz targets assert "typed error, never panic, never OOM" on malformed
  inputs (ADR-0016).
- Property tests assert `allocation ≤ limit` for every parse path.
- A stress test feeds a store full of hostile descriptors (huge lengths,
  deep chains, huge fanouts) and asserts bounded CPU/memory via the budget
  counters.
