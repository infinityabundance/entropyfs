# Baselines and expected regimes

Status: **measured evidence** (campaigns `campaign-1787665094-a6641d1` …
`campaign-1787671040-923df7b`, FUSE court `fuse-court-1787664579-d90772c`,
all in `evidence/performance/INDEX.md`). Machines differ → re-measure per
machine; the numbers here are the admission-baseline for this project's
current state.

## 1. Measured baselines (current floor)

Source pack (EntropyFS repo, length-prefixed; 2,030,799 bytes at
`923df7b`):

| Baseline | Output bytes | Ratio | Notes |
|----------|--------------|-------|-------|
| raw file (ext4) | 2,030,799 | 1.000× | write 7802 MiB/s (single dd, warm cache) |
| zstd -1 (whole) | 459,489 | 4.420× | 0.005 s |
| zstd -19 (whole) | 315,758 | 6.432× | 0.279 s |
| zstd -1 (per 64 KiB) | 543,128 | 3.739× | the dictionary-horizon diagnostic |
| zstd -19 (per 64 KiB) | 450,339 | 4.509× | same chunking as EntropyFS |
| direct byte rANS (A1-pure, per 64 KiB) | 1,243,242 | 1.633× | no SequenceRans |
| standalone SequenceRans | 571,134 | 3.556× | RAW + match finder only |
| EntropyFS (full pipeline) | 571,134 | 3.556× | == standalone SequenceRans on this pack |

**Floor diagnosis (the zstd-per-64KiB experiment):** EntropyFS's
per-extent floor (3.556×) is within 5% of zstd-per-64KiB (3.739×); the
~19% gap to whole-file zstd -1 (4.420×) is cross-chunk dictionary
context, NOT matcher quality. The indicated direction is a cross-chunk
SequenceDict, not a deeper SequenceRans matcher.

FUSE-frontend court (64 MiB shake_128, incompressible → RAW floor):
4K buffered 335.2 MiB/s, 1M writes 620.8 MiB/s, warm read 2256.7 MiB/s,
fsync p50 1649 µs, 1M read p50 1137 µs, bindgen cold build 10.47 s OK.

## 2. The three engineering gates (Phase-8 directive §10)

Measured against the admission evidence, with the same corpus caveats as
the campaigns:

### FLOOR — general/uncompressible workloads

| Criterion | Target | Measured | Verdict |
|-----------|--------|----------|---------|
| RAW storage overhead after GC | within a few % of ordinary storage | urandom 0.997× (0.3% overhead, pre-GC) | ✅ met |
| competitive sequential read | "striking distance" of ext4 | 2256.7 MiB/s warm (FUSE, page cache) | ✅ met for reads |
| competitive sequential write | "striking distance" of ext4 | 620.8 MiB/s (FUSE, incompressible) vs 5901 MiB/s raw ext4 dd | ❌ not yet — FUSE round-trips + per-commit cost dominate |
| bounded p99 latency | bounded | 1M write p99 45 µs; fsync p99 2.9 ms; 4K dsync 7.9 ms/op | ⚠ 4K per-op durability is the outlier (write-through path) |

### BASELINE — ordinary compressible workloads

| Criterion | Target | Measured | Verdict |
|-----------|--------|----------|---------|
| density vs mature transparent compression | competitive with zstd-class | src pack 3.344× vs zstd -1 3.832×, zstd -19 5.502× | ❌ not yet — behind zstd on ratio |
| respectable random access | materialization bounded | 1M read p50 1137 µs warm | ✅ met |

### ADVANTAGE — structured/versioned/configurational workloads

| Criterion | Target | Measured | Verdict |
|-----------|--------|----------|---------|
| materially lower physical storage | clear double-digit % over best conventional baseline | structured corpus 1,328× (structural + EXACT_REF aliasing + CAS object sharing; attribution measured per layer from `923df7b`); duplicated trees dedup via EXACT_REF/canonical reuse at ~40 B/extent | ✅ demonstrated for structural + dedup content (attribution measured, not labeled) |
| versioned/base+residual advantage | temporal savings | H2 series: +7.2% (RANS-era) → −24% (SequenceRans floor, positional residuals only) → **+35.0%** with BASE_SEQUENCE deltas (sequential 2.745× vs shuffled 1.784×); Phase-8B post-GC permanent footprint: sequential full 1,528,175 → **1,366,816 B** (10.6% historical index metadata pruned) | ✅ met with the delta family (the −24% intermediate campaign isolates the positional-residual cost; the post-GC gap is the real base-chain cost, not index bloat) |

**Honest summary:** the FLOOR is substantially met (reads, RAW overhead,
negative controls); the BASELINE density gate is not yet met (zstd
whole-file is ahead; the per-extent floor is within 5% of zstd-per-64KiB,
so the gap is cross-chunk context — SequenceDict is the indicated next
step, not a deeper matcher); the ADVANTAGE gate is met for
structural/dedup content and — with the BASE_SEQUENCE delta family — for
versioned content. Post-GC physical footprint (the usable-capacity
metric) is now measured per corpus: structured 1,092× allocated blocks,
src 2.88×, urandom 0.90× (the honest cost of records + index for
incompressible data), compressed 0.95×.

## 4. Competitive filesystem court (Phase 8H, v2 — root-capable)

The root-capable court is `tools/run-court-docker.sh` (disposable privileged
docker VM: xfsprogs/btrfs-progs/erofs-utils/squashfs-tools/fuse3/zstd;
`tools/docker/Dockerfile.court`); its first zero-waiver run is archived as
`fs-court-1787674397-4f58334` (same machine/kernel via the container,
loop-mounted XFS/Btrfs, drop_caches cold reads). Corpora: source tree,
64 MiB random, 64 MiB zeros, tar.gz control. Symmetric rules: buffered /
durable (cp+sync) writes, warm/cold reads, du apparent AND allocated.

**Storage — EntropyFS's post-GC backing (73.9 MB allocated for 135.7 MB
apparent, 1.836×) is DENSER than the Btrfs+zstd image (78.7 MB, 1.65×)
on the same corpus set** (zeros → ~0 and random → ~1:1 dominate both;
EntropyFS's zeros cost ~0 while Btrfs's per-file overhead and image
metadata remain). XFS image 203.5 MB (no compression); SquashFS-zstd src
4.587× (258 KB) and EROFS-lz4hc src 1.762× (672 KB) for the source
corpus; zstd -1 whole src 3.959×, per-64KiB 3.518×.

**Throughput — the honest gaps.** On the source tree (2,600+ tiny files)
EntropyFS writes 8.5 buffered / 7.7 durable MiB/s and reads 54.9 warm /
48.2 cold, vs ext4 376/49/446/347, XFS 254/78/453/316, Btrfs 97/57/
441/276, Btrfs+zstd 87/42/454/233. On 64 MiB buffered random writes
EntropyFS does 79.6 MiB/s vs 3,752–6,015 for the native filesystems
(~47× gap; per-request FUSE transactions dominate); random warm reads
2,708 vs 10,210–17,125 MiB/s (~4–6×). Density targets are met; the
FUSE write path (request aggregation, per-file commit batching) is the
measured bottleneck to attack after SequenceDict.

## 3. Expected behavior by data class (updated with SequenceRans)

| Data | Expected winning representation | Measured signal |
|------|--------------------------------|-----------------|
| zeros / fills | ZERO / FILL / PERIODIC | ~0 bytes |
| sparse structured (configs, patches) | SPARSE / BASE_RESIDUAL | low |
| low-cardinality text-ish | PALETTE / RANS / PERIODIC | low–moderate |
| natural text / source | SEQUENCE_RANS / RANS | src pack 3.344× (with current matcher) |
| binaries with repetition | SEQUENCE_RANS + dedup | high dedup |
| versioned files (write-in-place) | BASE_RESIDUAL (delta) vs fresh encode | H2 +35.2% with BASE_SEQUENCE deltas (controlled three-campaign series) |
| duplicated trees (build dirs) | EXACT_REF (dedup) | ~40 B/extent |
| already-compressed / encrypted / random | RAW | urandom 0.997×, zstd pack 0.993× |

The H6 requirement holds: random/encrypted data converges to RAW within a
small constant (~0.3% on urandom).

## 4. Overhead budget per extent (RAW path)

For a 64 KiB RAW extent, persisted bytes ≈ 65536 + descriptor (~40 B) +
record envelope (58 B) + content id amortization + GC overhead. The
campaign accounting tables verify the reachable-bytes cross-check per run;
urandom 0.997× means the total RAW overhead is ≈ 0.3% of logical.

## 5. Read/write amplification expectations

- Write amplification: append-only ⇒ ≥ 1 physical write per logical chunk
  at commit, plus GC copying later. The group-commit batch path (Phase-8
  M2) reduces the COW tree/root amplification to one generation per batch.
- Read amplification: 1 physical read per raw/rANS object; references add
  dependent reads (bounded by depth ≤ 4). The H5 question (materialization
  beating fetch on slow storage) remains unmeasured — requires a cold-
  storage court (drop_caches needs root, unavailable in this environment).

## 6. Comparative targets (measured)

- vs zstd: rANS/SequenceRans are byte-alphabet coders; zstd's match finder
  and Huffman/FSE back end currently win on general text. The configurational
  families apply where LZ dictionaries cannot (combinatorial structure).
- vs Btrfs zstd / EROFS / SquashFS / XFS: explicitly waived in the
  campaigns (require root loop mounts in this environment); the comparison
  surface is documented in `docs/performance/methodology.md` §3.

## 7. Latency budgets (measured vs target)

| Op | Target | Measured |
|----|--------|----------|
| cached read p50 (64 KiB extent materialization) | < 50 µs | 1M read p50 1137 µs (FUSE round trip + page cache) |
| write commit (single chunk, no GC) | < 1 ms incl. fdatasync | 4K dsync 7.9 ms/op (write-through); 1M write ≈ 1.6 ms incl. trailing fsync (620.8 MiB/s) |
| fsync | < 2 ms | p50 1649 µs ✅ |
| mount (cold, 10 GiB store, rebuild index) | < 5 s | unmeasured (index rebuild scales with store size) |
