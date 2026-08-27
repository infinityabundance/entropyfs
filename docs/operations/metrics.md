# Metrics reference (Phase 12E.6)

`entropyfs metrics [--json]` (CLI) and `Engine::metrics()` (the same
versioned DTO) expose the operational accounting. Every metric in the
DTO is defined in the code's normative registry (`METRIC_REGISTRY` in
`src/engine/metrics.rs`): name, unit, snapshot-vs-cumulative, scope,
reset behavior, and authority/source. Nothing is exposed whose semantics
cannot be defined precisely.

## Byte accounting

| key | unit | kind | meaning |
| --- | --- | --- | --- |
| `accounting.logical_bytes` | bytes | snapshot | Σ materialized logical bytes of reachable inodes |
| `accounting.reachable_bytes` | bytes | snapshot | Σ physical record bytes of root-reachable objects (from `StoreStats` — refreshed by maintenance passes, not on every call) |
| `accounting.physical_used_bytes` | bytes | snapshot | Σ segment file lengths (`Store::physical_used`) |
| `accounting.physical_capacity_bytes` | bytes | snapshot | statvfs capacity of the backing store (capped by any override) |
| `accounting.physical_free_bytes` | bytes | snapshot | capacity − used |
| `accounting.object_count` | objects | snapshot | entries in the derived object index |
| `accounting.data_record_count` | records | snapshot | reachable data records |
| `accounting.blob_count` | blobs | snapshot | files in the engine blob namespace (O(n) to collect) |

## Physical reconciliation (Phase 9H)

`physical.live_bytes`, `physical.dead_indexed_bytes`,
`physical.index_hidden_bytes`, `physical.unindexed_bytes`,
`physical.torn_bytes`, `physical.zero_padding_bytes`,
`physical.format_overhead_bytes`, `physical.unexplained_bytes` — the
exact index-vs-physical drift surface. `unexplained_bytes` must be 0 on
a healthy store; the physical scan's contract identity is
`file_bytes == Σ categories`.

## GC / maintenance

`gc.unreachable_bytes` (last-known reclaimable; refresh with
`compact()`/`gc`), and the compaction report
(reclaimed / physical-after) returned by `Engine::compact()`.

## DSFB observer (advisory; zero decoding authority)

`dsfb.tracked_chunks`, `dsfb.steps`, `dsfb.drift_events`,
`dsfb.slew_events`, `dsfb.narrowed_searches`,
`dsfb.candidates_evaluated` — the sharded observer's accounting
(Phase 11F); dropping this state never affects bytes.

## Cache

`cache.model_cache_hits` / `cache.model_cache_misses` — the model-object
cache.

## Write-path phases

`write_path_phases[]` — the perf rows (prepare / append /
commit_lock_wait / epoch_wait / barrier_fdatasync / search / ...):
count, cumulative total ms, and p50/p95/p99 µs over the bounded sample
ring. Units discipline: cumulative rows are WALL time; CPU-sum rows
(`worker_useful_cpu`) are recorded separately and are never conflated
with wall partitions.

## JSON schema

The DTO is `schema_version`-versioned (`src/engine/metrics.rs`); add
fields with a version bump, never rewrite. The Go binding parses the
same JSON into its stable typed subset.
