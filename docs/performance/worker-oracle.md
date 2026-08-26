# The worker-pool decision oracle (Phase 11D)

Status: implemented; the decision is sealed at
`evidence/performance/worker-oracle-1787765041-052bc46/` (rev `052bc46`).
Reproduce with `cargo test --release --lib worker_oracle -- --nocapture`.

Phase 11C ended with a deliberately coarse scheduler: a process-wide
worker SEMAPHORE that gives a multi-chunk batch up to the machine's full
worker capacity and parks competing batches until a whole batch finishes.
11B's accounting proved the epoch mutex was the write-path serialization
resource and 11C removed it; the reconciliation after 11C showed `prepare`
dominating request time, but `prepare` was one opaque bucket. Before
deciding whether a persistent fair worker pool is worth building, the
oracle decomposes it:

```text
prepare = useful search/decode CPU
        + worker_queue_wait   (parked on the semaphore, Gate A)
        + spawn/join overhead (scoped-thread construction, Gate B)
        + compose/phase-3/hash/validation + gaps
```

with the worker budget's counters (requested/granted/blocked batches,
peak parked waiters) and per-write latency percentiles (Q5).

## 1. Instrumentation

- `worker_queue_wait` — the time inside `workers::grant` (both the
  search batch and `materialize_decode`'s multi-extent decode). This IS
  the semaphore queue wait.
- `worker_scope_wall` — the scoped-thread scope duration (search batch /
  decode batch).
- `worker_useful_cpu` — each worker's TRUE thread-CPU time
  (`CLOCK_THREAD_CPUTIME` via rustix's `time` feature; wall fallback),
  SUMMED across the parallel workers. It is a CPU sum, not a wall slice:
  it may exceed 100% of `prepare`, which is the point. Only
  `worker_queue_wait` + `worker_scope_wall` (wall segments) partition
  `prepare`; the identity assertions check those, never the CPU sum.
- `worker_tasks` — chunks processed by the workers (the workload went
  through the semaphore at all).
- The budget's cumulative counters (`requested`/`granted`/`blocked`/
  `batches`, peak `waiters`) give the queue-depth column.
- Two workload-validity probes live in the search: the exact-dedup hit
  fraction and the decisive early-exit fraction. The oracle ASSERTS both
  are zero — a non-zero value means the sweep is measuring the EXACT_REF
  cache, not search CPU.

## 2. Workload discipline (the first oracle run was wrong)

The first run reused ONE store across the 1/2/4/8/16 sweep. That was a
methodology bug the probes caught: each sweep appended another 4 MiB per
file, and when the epoch crossed its 1024-op checkpoint threshold the
committed chunk index filled — the 16-thread row then measured a
checkpoint-fed EXACT_REF dedup cache, not search CPU:

```text
16T (invalid run): dedup_hit_frac=1.0000  rans_ms=0.0  search 11.2 s -> 0.21 s
```

The fix is the 11C court's corpus rule, applied to the oracle: a FRESH
store, FRESH files, and PER-WRITE-DISTINCT content per thread count (the
sweep never repeats a 64 KiB content, so exact dedup can never fire and
the search must run on every chunk). The valid sweep shows
`dedup_hit_frac = 0.0000` and `decisive1_frac = 0.0000` at every thread
count, and the search CPU constant at ~11.2–11.6 s (wall-sum) / ~9.8–10.0 s
(thread-CPU) — the sweep is measuring the search.

## 3. The sealed numbers (rev 052bc46, release)

256 × 1 MiB epoch writes per thread count (fresh store each), 8-core /
16-SMT Ryzen 7 9800X3D:

| threads | wall | queue% | spawn% | useful CPU | util | p50 | p99 |
| ------: | ---: | -----: | -----: | ---------: | ---: | --: | --: |
| 1 | 1.59 s | 4.6% | 32.0% | 9.99 s | 0.62 | 5.3 ms | 9.5 ms |
| 2 | 1.11 s | 29.5% | 27.2% | 10.00 s | 0.57 | 7.6 ms | 10.3 ms |
| 4 | 1.11 s | 67.1% | 12.7% | 9.97 s | 0.56 | 14.2 ms | 37.5 ms |
| 8 | 1.13 s | 83.7% | 6.4% | 9.94 s | 0.55 | 24.8 ms | 121.4 ms |
| 16 | 1.14 s | 91.7% | 3.2% | 9.83 s | 0.55 | 52.4 ms | 177.6 ms |

`queue%`/`spawn%` are shares of `prepare`; `useful CPU` is the summed
thread-CPU (constant across thread counts); `util` is
`useful / (granted × per-batch scope wall)`. The reconciliation identity
holds at every thread count (no overlap, residual ≤ 0.9% — the rows are
drill-downs inside `prepare`, the request partition is untouched).

## 4. The gates

**Gate A (queue wait) FIRES.** The semaphore queue grows 4.6% → 91.7% of
`prepare` from 1 to 16 writers. This is the 11C design's inherent
head-of-line blocking: a batch reserves ALL its slots or none, so at T
writers the batches run strictly one-at-a-time and every competing writer
parks ~50 ms (16 T). This is exactly the batch-granularity serialization
the 11D brief predicted.

**Gate B (spawn/join) is weak.** The `spawn%` column is the scope-wall
gap beyond the CPU floor — an UPPER BOUND on thread construction that
also includes SMT sharing and scheduler gaps (16 workers on 8 physical
cores). It is 32% at 1 T but only 3.2% at 16 T. A pool would recover the
construction part (tens of µs per batch), not the SMT part.

**Gate C (useful CPU) does NOT differentiate.** The summed search CPU is
9.83–10.00 s (±2%) at every thread count. The semaphore wastes no CPU —
it parks threads, it does not inflate work (the rejected `grant(0) →
run-inline` fallback was the CPU-inflating alternative).

**Throughput is exhausted.** 16 T wall = 1.14 s against a CPU floor of
9.83 s / 8 physical cores ≈ 1.23 s (SMT gives the ~0.9–1.1 s effective
band). The 16 T wall is AT the floor and below the 1 T wall (1.59 s),
because the serial per-write phases (compose/hash/stage/append) overlap
across writers at 16 T. A pool cannot push the wall below the true
`useful CPU / cores` floor — no scheduler can.

**Tail latency is the only real headroom.** p50 5.3 → 52.4 ms and p99
9.5 → 177.6 ms from 1 to 16 writers is the per-request cost of batch
serialization, not CPU. A task-level (chunk-level) fair pool could
interleave chunks from all writers and compress the tail toward the
mean — the 11D brief's "A0 B0 C0 D0 / A1 B1 C1 D1" scheduling.

## 5. The decision

Gate A fires, so the oracle's decision tree points at a pool — but with
an important correction to the naive reading: **the wall is already at
the CPU floor, so the pool's only available win is latency fairness
(p50/p99 at 8/16 T), not throughput.** The 11D brief's hard success
criterion therefore resolves to:

> The pool must beat the semaphore's TAIL (p50 52.4 ms / p99 177.6 ms at
> 16 T, 24.8 / 121.4 ms at 8 T) without increasing total search CPU
> (9.8–10.0 s baseline). The wall (1.14 s at 16 T) is already at the
> floor and is not a pool target.

Next step, if pursued: a NARROW typed pool probe per the 11D design —
`EncodeChunk`/`DecodeExtent` tasks with `(request_id, ordinal)`, results
reassembled in ordinal order (persistent output stays deterministic),
a bounded global queue, task-level (not batch-level) fairness, and a
foreground FUSE class that beats the background optimizer. Scheduling
must never become decoding authority, and the pool must be rejected if
it merely reproduces the 1.14 s floor with more code — the semaphore
stays as the simpler fallback.

Two items explicitly NOT this phase's levers: the `commit_lock_wait`
fsync convoy (contract-inherent; group durability is a separate project)
and the DSFB observer mutex, which adds ~0.5 s of contention to the
search wall at 16 T (search wall-sum 11.3 s vs useful CPU 9.8 s) — a
per-chunk lock-free or sharded observer would recover part of that gap
independently of any pool.

## 6. Evidence

- `evidence/performance/worker-oracle-1787765041-052bc46/` — the sealed
  run: `run.log` (raw), `summary.tsv` (the table above), `identity.tsv`
  (per-thread-count reconciliation), `results.json` (machine-readable
  gates + decision).
- `src/tests/worker_oracle.rs` — the oracle + the identity assertion +
  the workload-validity gates; part of the 419-test lib suite.
