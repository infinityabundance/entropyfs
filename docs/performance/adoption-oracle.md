# Phase-12E.13 court: the object-store adoption oracle

Sealed: `evidence/performance/adoption-oracle-1787840768-1b9926a/`.
Oracle: `src/tests/adoption_oracle.rs`. Driver:
`tools/court-adoption.sh`.

## The question

The 12E.13 brief: benchmark and verify the embeddable immutable-object
engine through its STABLE facade — `put_blob` / `get_blob` /
`read_blob_range` / `sync` / `compact` / `metrics` — NOT FUSE, to
discover an adoption wedge. "No compelling 10× pain-point win found
yet" is an explicitly valid conclusion; the data must not be distorted
to produce a headline.

## The measurement

Six brief-mandated workloads, each in its OWN engine store (clean dedup
attribution, no cross-workload leakage), each: PUT every blob (`Ack`
durability), one SYNC, GET every blob byte-exact (the engine's own hash
gate), RANGE-read every 10th blob (4 KiB window at one-third offset,
verified byte-for-byte), SETTLE (compact + metrics). Baselines: the
same blobs as raw files on the same device (one file per blob, one
trailing fsync).

## Results (release, sealed run)

| workload | blobs | logical | unique | dedup saved | settled physical | footprint vs raw |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `build-artifacts` (12 versions × 200) | 2400 | 18 666 000 | 4 945 040 | 13 720 960 (73%) | 908 411 | **0.049×** |
| `scientific-outputs` (6 × 20 × 64–512 KiB) | 120 | 8 013 660 | 8 013 660 | 0 | 441 787 | **0.055×** |
| `container-layers` (8 layers) | 800 | 6 553 600 | 3 112 960 | 3 440 640 (52%) | 552 512 | **0.084×** |
| `generated-assets` (50 template assets) | 50 | 819 200 | 819 200 | 0 | 78 510 | **0.096×** |
| `ci-cache` (300, 40% cache hits) | 300 | 3 830 531 | 3 141 726 | 688 805 (18%) | 402 651 | 0.105× |
| `source-trees` (10 versions × 150) | 1500 | 9 088 550 | 9 088 550 | 0 | 1 737 891 | 0.191× |

Throughput (all workloads): put 33–138 MiB/s (CPU-bound; the foreground
search prices every blob), get 114–691 MiB/s, per-blob get p50/p95/p99
20–90 / 22–117 / 28–154 µs, sync 0.3–5 ms per batch, compact reclaimed
94 KiB–3.8 MiB per workload, range reads complete in <13 ms across each
workload. Raw-file write baseline: ~0.009 s for the 18.7 MiB
build-artifacts set (~2 GiB/s page-cache) — the engine put is ~14×
slower on this corpus; the wedge is FOOTPRINT, not speed.

## The gate decision: WEDGE-CANDIDATE (four workloads clear 10×)

The brief's 10× bar: **build-artifacts 0.049× (20×)**, scientific-
outputs 0.055× (18×), container-layers 0.084× (12×), generated-assets
0.096× (10.4×). The adoption story is specific and honest:

- **Versioned / layered immutable object populations** (build
  artifacts, container layers) win through **dedup first** (73% / 52%
  of logical bytes cost nothing) then structure-aware compression of
  the unique remainder;
- **Structured single-version populations** (scientific outputs,
  generated assets) win through **compression alone** (18× / 10.4×)
  — the template/skeleton structure is exactly what the representation
  machinery exploits;
- `source-trees` (5.2×) is the weakest of the six — small edited
  files defeat both dedup (every version differs) and long-range
  context.

Attribution is exact: dedup_saved = logical − unique (bytes that cost
nothing), physical_after = the settled footprint (dedup remainder +
compression + structural metadata). Every GET and RANGE was verified
byte-exact through the facade's own gates.

Recorded caveats (the honesty rules): the raw baseline is plain files
with no dedup/compression — a tighter baseline (git-style delta, zstd
archives) would narrow the headline, and per-workload stores isolate
attribution at the cost of not measuring cross-population dedup. The
put-speed deficit is real and recorded as the adoption tradeoff: the
engine buys 10–20× footprint for versioned/structured immutable object
sets and sells ~14× put throughput versus raw file writes. The adoption
wedge is a **storage-density wedge for versioned and structured
immutable object populations**, not a general speed claim.

## What stays

The oracle remains in the suite as the facade-level benchmark and
correctness surface (462 lib tests green): the C ABI (12E.14) and Go
binding (12E.15) correctness courts reuse its exact corpus and
byte-exactness discipline, and any future adoption packaging (the
12E.13 continuation) benchmarks against these same rows.
