# Performance methodology

Every performance claim must be reproducible; a claim without evidence is
not a project fact (Forensic evidence discipline, §50).

## 1. Mandatory context per benchmark run

```text
input corpus hash
EntropyFS git revision
ryg-rans-rs revision (locked in Cargo.lock)
dsfb revision (locked in Cargo.lock)
kernel version
mount configuration (FUSE options, cache mode, policy mode)
CPU feature set (/proc/cpuinfo flags)
benchmark command (verbatim)
representation distribution (per-tag counts + bytes)
logical/stored byte accounting (docs/theory/information-accounting.md)
throughput/latency percentiles
result hashes
```

## 2. Baselines (distinguish writable vs read-only)

| Baseline | Mode |
|----------|------|
| plain ext4 / XFS | writable, no compression |
| Btrfs (compress=zstd:1) | writable, compression |
| direct `ryg-rans-rs` | entropy-only reference |
| zstd (`-19` and `-3`) | external compression reference |
| EROFS / SquashFS | cold read-only image comparisons where appropriate |
| EntropyFS RAW-only | ablation baseline (below) |

## 3. Required metrics (§42)

Always report: logical bytes, physical reachable bytes, total backing-store
bytes, metadata/model/descriptor/residual bytes, dedup/rANS/configurational
savings (attributed), unreachable/GC bytes; read/write amplification;
physical bytes read/written; encode MB/s, materialize MB/s, cold-read MB/s,
warm-read MB/s, random IOPS; p50/p95/p99 read and write latency; fsync
latency; mount time; recovery time; GC throughput; peak RAM; CPU
cycles/byte where feasible.

**`logical bytes delivered / physical bytes fetched`** is a first-class
metric (§45): regeneration may beat fetching expanded bytes; it is never
assumed without measurement.

## 4. Corpora (no cherry-picking)

- source trees (Rust, Linux);
- Rust build trees / compiler artifacts;
- VM/container images; package caches;
- database snapshots; SQLite databases;
- mixed home-directory files; large logs;
- binaries/libraries; media;
- already-compressed archives (zip/tar.gz); encrypted/random data;
- synthetic zero/fill/sparse/periodic/low-cardinality data.

Corpora are pinned by hash and kept out of git (recorded, derived, like
ryg-rans-rs's workload discipline).

## 5. Ablation science (§43) — attribution is mandatory

Benchmark the engine in strict increments:

```text
RAW only
RAW + rANS
+ exact dedup
+ base residuals
+ configurational coding
+ entropy universes
+ DSFB ranking
+ background optimizer
```

Additionally compare:

```text
DSFB-ranked candidate search
vs exhaustive same candidate set
vs simple heuristic ranking
```

Rules:

- A DSFB improvement is legitimate only if it reaches the same/better
  representation with fewer candidates, less CPU, or better temporal
  adaptation.
- Never credit DSFB with savings produced by deduplication or rANS.
- Savings components are computed as disjoint byte sets
  (docs/theory/information-accounting.md §3).
- Negative findings remain in the repository (H6; §44).

## 6. Statistical practice

- Latency benchmarks: ≥ 1000 samples per point; report p50/p95/p99;
  machine-pinned (taskset) where available; cache warm/cold clearly
  separated (page cache dropped between cold runs).
- Throughput: multiple runs; report median and spread; preflight
  verification (output hash) before timing.
- No `RUSTFLAGS` changes between compared builds unless documented (SIMD
  tiers get their own rows).

## 7. Tooling

`entropyfs benchmark` (in-process, reproducible, emits JSON evidence) is the
official measurement surface; scripts in `tools/` drive CI benchmarks;
evidence lands in `evidence/performance/`.
