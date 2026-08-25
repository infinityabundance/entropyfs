# Baselines and expected regimes

Status: **measured evidence** (campaigns `campaign-1787665094-a6641d1` …
`campaign-1787668526-d04227f`, FUSE court `fuse-court-1787664579-d90772c`,
all in `evidence/performance/INDEX.md`). Machines differ → re-measure per
machine; the numbers here are the admission-baseline for this project's
current state.

## 1. Measured baselines (current floor)

Source pack (EntropyFS repo, length-prefixed; 1,471,135 bytes):

| Baseline | Output bytes | Ratio | Notes |
|----------|--------------|-------|-------|
| raw file (ext4) | 1,471,135 | 1.000× | write 5901.2 MiB/s (single dd, warm cache) |
| zstd -1 | 383,949 | 3.832× | 0.004 s |
| zstd -19 | 267,366 | 5.502× | 0.213 s |
| direct ryg-rans-rs (store, per 64 KiB chunk) | 439,989 | 3.344× | same backend, same chunking |
| EntropyFS (full pipeline) | 439,989 | 3.344× | == direct rANS on this pack |

EntropyFS on the source pack is at parity with direct per-chunk rANS and
behind zstd (-1 by 14%, -19 by 39% on ratio). The foreground LZ matcher
(greedy, chain-depth 16, 131-byte copy cap, one candidate per chunk) adds
little over entropy coding on source text; the deeper background matcher
(Phase-8 §4) is the identified next step, not a claim.

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
| materially lower physical storage | clear double-digit % over best conventional baseline | structured corpus 845×–989× (structural/configurational — ZERO/FILL/PERIODIC/rANS; dedup measures 0 on this single-batch corpus, see `d04227f`); duplicated trees dedup via EXACT_REF at ~40 B/extent | ✅ demonstrated for structural + content-addressed dedup content (attribution now measured, not labeled) |
| versioned/base+residual advantage | temporal savings | H2 series: +7.2% (RANS-era) → −24% (SequenceRans floor, positional residuals only) → **+35.0%** with BASE_SEQUENCE deltas (sequential 2.745× vs shuffled 1.784×); Phase-8B post-GC permanent footprint: sequential full 1,528,175 → **1,366,816 B** (10.6% historical index metadata pruned) | ✅ met with the delta family (the −24% intermediate campaign isolates the positional-residual cost; the post-GC gap is the real base-chain cost, not index bloat) |

**Honest summary:** the FLOOR is substantially met (reads, RAW overhead,
negative controls); the BASELINE density gate is not yet met (zstd is
ahead; the deeper matcher is the next engineering step); the ADVANTAGE
gate is met for dedup/configurational content and — with the BASE_SEQUENCE
delta family — for versioned content (the H2 series is the controlled
record). These are the current measured states.

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
