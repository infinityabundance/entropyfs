# Phase-10A: FUSE thread sweep + millisecond map (diagnostic evidence)

Archives: `fs-court-1787690754-6e67723/` … `fs-court-1787690823-6e67723/`
(host run: loop-image sections XFS/Btrfs/EROFS/SquashFS WAIVED — no root;
the FUSE EntropyFS section is root-free and is the subject of this sweep).

## The question

The mount negotiates `FUSE_WRITEBACK_CACHE | ASYNC_READ | PARALLEL_DIROPS |
BIG_WRITES`, 1 MiB max_write, 64 background requests — but the CLI default
is one event-loop thread. How much performance is sitting unused?

## The answer: none on this workload. Max concurrency is 1.

| threads | src bw | random bw | zeros bw | daemon CPU util |
| ------: | -----: | --------: | -------: | --------------: |
| 1 | 1.9 MiB/s | 70.0 | 332.2 | 0.55× |
| 2 | 1.9 | 70.8 | 374.5 | 0.57× |
| 4 | 1.9 | 64.0 | 334.9 | 0.58× |
| 8 | 1.9 | 65.4 | 335.6 | 0.59× |
| 16 | 1.9 | 65.5 | 328.3 | 0.60× |

`cp` (and most single-threaded writers) serialize namespace + write
operations: the FUSE stats record **max request concurrency = 1** for the
whole workload, so extra event-loop threads have nothing to run. The
daemon sits at ~0.55–0.6 cores — mostly idle, waiting on per-request
latency, not starving for cores.

## Where the milliseconds go (fuse-stats-1.txt, the src+random+zeros+tgz workload)

FUSE per-request latency (p50):
- write 1,831 µs (308 reqs, 1.11 s total) — the dominant cost
- setattr 2,407 µs (135 reqs, 347 ms) — cp's per-file metadata path
- create 2,468 µs (135 reqs, 342 ms) — one transaction per create
- read 43 µs, readdir 34 µs, getattr 6 µs — reads are cheap

Write-size histogram: 173 of 308 writes are 256K–1M (the kernel writeback
aggregation works), but 53 are <4K and 59 are 4–16K (the tiny files).

Write-path phase timings (the millisecond map):
- search 793 ms total (p50 153 µs/chunk): sequence_rans 481 ms +
  sequence_dict 120 ms + configurational 81 ms + byte_rans 23 ms
- prune 193 ms (p50 244 µs per transaction — every commit walks the graph)
- btree_mutation 67 ms (p50 307 µs per write transaction)
- append_flush 40 ms, validation 19 ms, hash 12 ms (5.3 µs/chunk)

The random.bin corpus (1024 chunks that all fall to RAW) pays the full
sequence_rans search (~440 µs × 1024 ≈ 450 ms) to prove it is random.
The direct-Store diagnostic isolates this exactly: 64 MiB random writes
at 37.7 MiB/s with the full search vs 592.7 MiB/s with RAW-only — a
15.7× search penalty on incompressible data, with the phase table
attributing it to sequence_rans/sequence_dict/configurational.

## The Phase-10 direction this evidence sets

1. **10B — foreground classification**: an obvious-random/high-entropy
   chunk must go hash → CAS → RAW without running the LZ/entropy
   families (the random corpus alone reclaims ~450 ms per 64 MiB).
2. **10C — parallel chunk preparation**: the per-chunk search latency
   (440 µs) is the unit of work; 16 chunks of a 1 MiB write are
   embarrassingly parallel before the serialized commit.
3. **10D — the namespace path is co-equal**: create 2.5 ms + setattr
   2.4 ms × 135 files ≈ 690 ms — the transaction-per-op machinery
   (prune 244 µs, btree_mutation 307 µs per op) is the small-file cost.
   Transaction groups / metadata epochs attack exactly this.
4. FUSE thread count: leave the default at 1 until the per-request
   latency falls; revisit concurrency with a genuinely parallel writer
   (fio, not cp).
