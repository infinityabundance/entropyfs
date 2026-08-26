# Write-path request reconciliation (Phase 11B)

Status: implemented and sealed (`evidence/performance/recon-court-1787757073-e5b0592/`;
run `tools/recon-court.sh` to reproduce).

Phase 11B is the performance equivalent of Phase 9H's physical byte
reconciliation. 9H made every backing byte accountable:

```text
file bytes = live + dead + hidden + padding + format + unexplained
```

11B makes every request microsecond accountable:

```text
request latency = Σ exclusive phases + residual
```

The question it answers is the post-10G one: *"with cheap foreground
selection, parallel chunk preparation, mutation epochs, io_uring and true
FUSE concurrency all enabled, where does each remaining microsecond of
write latency go?"* — and specifically, what is the shared write-side
serialization resource behind the 4→16-thread write plateau
(10G: 1T 375.8 → 4T 543.1 → 16T 558.1 MB/s).

## 1. The collector

`src/perf/mod.rs` gains a request ledger alongside the Phase-10A phase
table:

- `Timings::request(name)` opens a request envelope. Nested re-opens
  (the FUSE handler opens `fuse_write`; the store entry point re-opens
  `epoch_write` for direct callers) are pass-throughs — the inner
  exclusive phases attach to the OUTER envelope, so the total is the full
  request including FUSE overhead, and direct callers still get a
  reconciliation.
- `Timings::time_request(phase, f)` is an exclusive partition row: it
  times `f`, records it in the global phase table, AND attaches it to the
  thread's request envelope. The rows are LEAF blocks only — a row must
  never wrap a call that itself emits rows (that would double-count and
  the identity flags it as OVERLAP). Internal helper reads that are part
  of a larger row (`rmw_read`, `base_chunk_at`, the prev-version
  materialization inside `prepare`) run inside `Timings::detach`, which
  suppresses attachment.
- On close, `residual = total − Σ phases`. `reconcile()` aggregates over
  the closed requests: the stacked table is `Σ phases` vs `Σ totals` with
  the explicit residual (`unaccounted`) row. A negative aggregate
  residual (or a negative per-request residual) is an instrumentation
  bug — a nested row — and the court fails.

The partition rows for the FUSE write path are:

```text
fuse_write (total)
├─ inode_lock_wait        per-inode mutation lock
├─ epoch_lock_wait        epoch mutex acquisition (x2: view + staging)
├─ read_scan/read_deps/
│  read_prefetch/
│  read_decode            the chunk prefill (RMW materialization)
├─ prepare                hash + CAS + candidate search + §32 validation
├─ stage                  descriptor/object/inode/envelope encode
├─ commit_lock_wait       commit-coordinator wait
├─ append                 segment envelope append
├─ flush                  segment page-cache write
├─ epoch_wait             checkpoint-threshold epoch read
└─ cp_*                   checkpoint merge, when one fires
unaccounted               FUSE/scheduler/other (the residual)
```

The fsync path (`fuse_fsync`) partitions into the barrier rows
(`barrier_commit_lock_wait`, `barrier_fdatasync`, `barrier_dir_sync`,
`barrier_sb_write`, `barrier_sb_fsync`) plus the checkpoint's `cp_*`
rows.

## 2. The court

`src/tests/perf_reconciled.rs` (direct-store, diagnostic) and
`tools/recon-court.sh` (mounted FUSE, evidence-sealed) drive the epoch
write path at 1/2/4/8/16 writer threads and assert the identity per
thread count: no overlap, residual share below 15% (the runs land at
≤ 4%). `recon-court.sh` also verifies the written bytes byte-exactly and
archives the per-thread stats dumps plus a machine-readable stacked
table.

## 3. What the accounting found

### 3.1 The write-side serialization resource is the EPOCH MUTEX, not the commit coordinator

Direct-store epoch writes, before the 11B fix (release build, 256×1 MiB
writes):

| threads | epoch_lock_wait | epoch_wait | prepare | commit_lock_wait | residual |
| ------: | --------------: | ---------: | ------: | ---------------: | -------: |
| 1 | 0.0% | 0.0% | 98.7% | 0.0% | 0.3% |
| 2 | 24.6% | 25.2% | 47.1% | 0.0% | 0.9% |
| 4 | 37.5% | 37.5% | 23.5% | 0.0% | 0.5% |
| 8 | 43.6% | 43.7% | 11.8% | 0.0% | 0.2% |
| 16 | 46.9% | 46.7% | 5.9% | 0.0% | 0.1% |

`commit_lock_wait` is ~zero at every thread count: the commit coordinator
is NOT the bottleneck. The plateau is the epoch guard: `epoch_write`
held it for its entire body (the 10C/10D prepare dominates the body), so
every writer convoyed on one mutex — at 16 threads ~94% of request time
was waiting for the epoch guard, which is why the plateau never scaled.

### 3.2 The fix: hold the epoch guard only for the overlay reads and the staging

`epoch_write` now acquires the guard for block A (the inode view + chunk
prefill) and block B (staging), and releases it across `prepare_write`
— which is pure CPU + committed reads (its inputs are the pre-filled
overlay bytes). Same-inode writers are already serialized by the
per-inode mutation lock, and a checkpoint can only grow this inode's
size (it merges this thread's own earlier pending writes, which the
block-A read already includes); the size is re-read at staging as a
monotonicity guard.

After the fix (same direct-store court, release build):

| threads | epoch_lock_wait | epoch_wait | prepare | read_decode | residual | wall (before → after) |
| ------: | --------------: | ---------: | ------: | -----------: | -------: | --------------------- |
| 1 | 0.0% | 0.0% | 96.4% | 0.5% | 0.5% | 1.22 s (same) |
| 2 | 0.6% | 0.0% | 71.8% | 22.1% | 2.4% | 1.06 s (was 1.24 s) |
| 4 | 20.5% | 8.8% | 46.1% | 21.3% | 1.3% | 0.94 s (was 1.28 s) |
| 8 | 45.4% | 18.4% | 23.5% | 10.6% | 0.9% | 1.01 s (was 1.29 s) |
| 16 | 60.3% | 20.5% | 12.7% | 5.2% | 0.6% | 1.59 s (was 1.92 s) |

The 2–4-thread region now scales (the guard convoy collapsed from
~50–75% of request time to ~1–29%). The full 415-test suite — crash
courts, hostile-media court, concurrency suites — stays green, and the
reconciliation identity holds at every thread count (residual ≤ 2.4% in
this direct-store run).

### 3.3 The terms that remain (the 11C levers)

The accounting names the next two terms precisely:

1. **The remaining `epoch_lock_wait`/`epoch_wait` at 8–16 threads**
   (60–81%): the guard is still held across block A (the prefill
   materialization, `read_decode`) and block B (staging) and taken again
   by every write's checkpoint-threshold read (`epoch_wait`). Shrinking
   those holds — snapshotting the overlay for the prefill, or a lock-free
   pending-op counter — is the next lever.
2. **`read_decode` (5–22%) and the prepare/search workers**: every
   multi-chunk request spawns `available_parallelism()` workers, so T
   concurrent requests spawn T×N threads on an N-core box — the
   oversubscription inflates decode and search wall time exactly where
   the plateau flattens. A global worker budget (or a per-request cap)
   is the complementary lever.

The mounted court adds a third, durability-path term: at 16 threads
`commit_lock_wait` rises to 29.8% — `cp`'s trailing fsyncs queue on the
commit lock in `durability_barrier` (the fsync convoy).

## 4. Evidence

- `evidence/performance/recon-court-1787757073-e5b0592/` — the sealed
  mounted court: identity holds (no overlap, residual ≤ 4.0%) at
  1/2/4/8/16 threads; stacked tables, per-thread stats dumps, machine-
  readable results.
- `src/tests/perf_reconciled.rs` — the direct-store court (debug + release).
- The daemon's `--stats-file` dump now includes the reconciliation table
  (`Timings::render_reconciled`), so every future `court-threads*` run
  carries it automatically.
