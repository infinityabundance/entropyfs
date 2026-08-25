# EntropyFS evidence campaign

- campaign dir: `evidence/performance/campaign-1787684918-80e36c8`
- created: unix 1787684918

## Admission checklist (methodology §8)

- [x] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
- [x] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
- [x] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
- [x] ablations identify which mechanism caused the gain (cumulative ladder A0-A8 + leave-one-out) — leave-one-out table 15 rows; cumulative ladder 13 rows (A0-A8)
- [x] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
- [x] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
- [x] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md

## Summary

- `structured[full]`: 3 runs — write 1149.5 MiB/s (p50 49062µs, p95 64872µs, p99 64872µs), read 2969.5 MiB/s, fsync p50 1655µs, physical median 50528 bytes, ratio 1328.152x
- `src[full]`: 3 runs — write 34.6 MiB/s (p50 80847µs, p95 82370µs, p99 82370µs), read 178.2 MiB/s, fsync p50 1645µs, physical median 679534 bytes, ratio 4.327x
- `urandom[full]`: 3 runs — write 33.0 MiB/s (p50 968637µs, p95 969722µs, p99 969722µs), read 3877.1 MiB/s, fsync p50 1637µs, physical median 33652515 bytes, ratio 0.997x
- `compressed-z19[full]`: 3 runs — write 36.5 MiB/s (p50 10247µs, p95 10425µs, p99 10425µs), read 4191.3 MiB/s, fsync p50 1650µs, physical median 394787 bytes, ratio 0.995x
- `versioned[full]`: 3 runs — write 80.4 MiB/s (p50 34795µs, p95 93945µs, p99 95024µs), read 564.4 MiB/s, fsync p50 1633µs, physical median 1359786 bytes, ratio 3.085x
- `versioned[no-base]`: 3 runs — write 75.3 MiB/s (p50 57157µs, p95 59808µs, p99 59939µs), read 1755.5 MiB/s, fsync p50 1640µs, physical median 1180500 bytes, ratio 3.553x
- `shuffled[full]`: 3 runs — write 65.3 MiB/s (p50 60633µs, p95 82017µs, p99 82164µs), read 759.2 MiB/s, fsync p50 1653µs, physical median 2346969 bytes, ratio 1.787x

Device writes during campaign window (nvme1n1p1): 330240000 bytes written, 20480 bytes read.

## Raw output

```text
entropyfs evidence campaign — 1787684918
revision: 80e36c8
  structured [full] 3 runs: write 1149.5 MiB/s (p50 49062µs, p95 64872µs, p99 64872µs) read 2969.5 MiB/s fsync p50 1655µs p95 2356µs p99 2356µs physical median 50528 ratio 1328.152x
  src [full] 3 runs: write 34.6 MiB/s (p50 80847µs, p95 82370µs, p99 82370µs) read 178.2 MiB/s fsync p50 1645µs p95 2731µs p99 2731µs physical median 679534 ratio 4.327x
  urandom [full] 3 runs: write 33.0 MiB/s (p50 968637µs, p95 969722µs, p99 969722µs) read 3877.1 MiB/s fsync p50 1637µs p95 17201µs p99 17201µs physical median 33652515 ratio 0.997x
  compressed-z19 [full] 3 runs: write 36.5 MiB/s (p50 10247µs, p95 10425µs, p99 10425µs) read 4191.3 MiB/s fsync p50 1650µs p95 2376µs p99 2376µs physical median 394787 ratio 0.995x

== leave-one-out ablation (structured) ==
  full       physical        50528 ratio 1328.152x write   1239.6 MiB/s cpu 0.050+0.000s (p95 write 46657µs)
  raw        physical       319070 ratio 210.326x write   1463.6 MiB/s cpu 0.040+0.010s (p95 write 38796µs)
  raw-byte-rans physical       267181 ratio 251.174x write    655.3 MiB/s cpu 0.100+0.000s (p95 write 92882µs)
  no-exact-ref physical        67868 ratio 988.815x write   1234.7 MiB/s cpu 0.050+0.000s (p95 write 46390µs)
  no-base    physical        50528 ratio 1328.152x write   1277.0 MiB/s cpu 0.050+0.000s (p95 write 45322µs)
  no-temporal physical        50528 ratio 1328.152x write   1267.8 MiB/s cpu 0.050+0.000s (p95 write 45451µs)
  no-config  physical        64939 ratio 1033.414x write    792.6 MiB/s cpu 0.080+0.000s (p95 write 75412µs)
  no-rans    physical       112301 ratio 597.580x write   1373.8 MiB/s cpu 0.040+0.000s (p95 write 41457µs)
  no-byte-rans physical        50528 ratio 1328.152x write   1277.1 MiB/s cpu 0.050+0.000s (p95 write 45325µs)
  no-sequence-rans physical       112301 ratio 597.580x write   1321.5 MiB/s cpu 0.050+0.010s (p95 write 42546µs)
  no-deep    physical        50528 ratio 1328.152x write   1259.2 MiB/s cpu 0.050+0.000s (p95 write 45458µs)
  no-sequence-dict physical        50528 ratio 1328.152x write   1170.8 MiB/s cpu 0.050+0.000s (p95 write 49321µs)
  no-shared-dict physical        50528 ratio 1328.152x write   1263.9 MiB/s cpu 0.050+0.000s (p95 write 45819µs)
  no-universe physical        50528 ratio 1328.152x write   1274.5 MiB/s cpu 0.050+0.000s (p95 write 45532µs)
  no-dsfb    physical        50528 ratio 1328.152x write   1290.5 MiB/s cpu 0.050+0.010s (p95 write 45007µs)

== cumulative ladder A0-A8 (structured) ==
  A0-raw             physical       319070 ratio 210.326x write   1448.0 MiB/s cpu 0.040+0.000s (p95 write 39380µs)
  A1-byte-rans       physical       267181 ratio 251.174x write    652.7 MiB/s cpu 0.100+0.000s (p95 write 93192µs)
  A2-exact-ref       physical       251881 ratio 266.431x write    659.8 MiB/s cpu 0.100+0.000s (p95 write 92033µs)
  A3-base-residual   physical       251881 ratio 266.431x write    636.9 MiB/s cpu 0.090+0.000s (p95 write 95322µs)
  A4-config          physical       112301 ratio 597.580x write   1347.9 MiB/s cpu 0.050+0.000s (p95 write 42357µs)
  A5-temporal-bases  physical       112301 ratio 597.580x write   1376.2 MiB/s cpu 0.050+0.000s (p95 write 41033µs)
  A6-universe        physical       112301 ratio 597.580x write   1357.8 MiB/s cpu 0.040+0.000s (p95 write 42070µs)
  A7-dsfb            physical       112301 ratio 597.580x write   1190.5 MiB/s cpu 0.050+0.010s (p95 write 48819µs)
  A8-background      physical       112301 ratio 597.580x write   1031.9 MiB/s cpu 0.060+0.000s (p95 write 52821µs)
  E1-sequence-rans   physical        50528 ratio 1328.152x write   1264.3 MiB/s cpu 0.050+0.000s (p95 write 45558µs)
  E2-sequence-dict   physical        50528 ratio 1328.152x write   1214.9 MiB/s cpu 0.050+0.000s (p95 write 47212µs)
  E3-shared-dict     physical        50528 ratio 1328.152x write   1281.8 MiB/s cpu 0.040+0.010s (p95 write 45252µs)
  E4-deep            physical        50238 ratio 1335.819x write   1264.3 MiB/s cpu 0.050+0.000s (p95 write 45571µs)

== DSFB search-budget investigation (structured) ==
  full      write median  1254.9 MiB/s (min 1250.5, max 1282.8) cpu median 0.050s physical [50528, 50528, 50528]
  no-dsfb   write median  1273.9 MiB/s (min 1273.1, max 1274.4) cpu median 0.050s physical [50528, 50528, 50528]
  physical identical across modes: true

== versioned experiment (H2) ==
  versioned [full] 3 runs: write 80.4 MiB/s (p50 34795µs, p95 93945µs, p99 95024µs) read 564.4 MiB/s fsync p50 1633µs p95 4414µs p99 4414µs physical median 1359786 ratio 3.085x
  versioned [no-base] 3 runs: write 75.3 MiB/s (p50 57157µs, p95 59808µs, p99 59939µs) read 1755.5 MiB/s fsync p50 1640µs p95 5348µs p99 5348µs physical median 1180500 ratio 3.553x
  shuffled [full] 3 runs: write 65.3 MiB/s (p50 60633µs, p95 82017µs, p99 82164µs) read 759.2 MiB/s fsync p50 1653µs p95 5778µs p99 5778µs physical median 2346969 ratio 1.787x
  sequential median reachable: 1359786 bytes (3.085x)
  shuffled    median reachable: 2346969 bytes (1.787x)
  base+residual savings vs shuffled: 987183 bytes (42.1% of shuffled reachable)
  post-GC reachable: sequential full 1233336 (3.401x) / no-base 1131427 (3.707x) / shuffled 2289960 (1.832x)

== GC and optimizer traffic ==
  unreachable before 5455393 → reclaimed 5455393 → after 2274864; physical 39268875 → 35927915; gc 0.014s; optimizer scanned 512 rewrote 0 saved 0
  unreachable by record tag (post-GC): {"BtreeNode": 2274626, "Root": 238}

== post-GC physical footprint ==
  src: logical 2940187 → reachable 679534 (4.33x) / total backing 685089 (4.29x) / allocated 692224 (4.25x)
  structured: logical 67108864 → reachable 50528 (1328.15x) / total backing 56083 (1196.60x) / allocated 61440 (1092.27x)
  urandom: logical 33554432 → reachable 33652515 (1.00x) / total backing 33658070 (1.00x) / allocated 33665024 (1.00x)
  compressed-z19: logical 392758 → reachable 394787 (0.99x) / total backing 400342 (0.98x) / allocated 405504 (0.97x)

== Phase-9C tree court ==
  files 290 (single-chunk 287), logical 2929913 B
  zstd -1 whole              580787 B  (5.070x)
  zstd -19 whole             392630 B  (7.499x)
  zstd -1 per-file           816005 B  (3.591x)
  zstd -19 per-file          714996 B  (4.098x)
  zstd -1 per-64KiB          725242 B  (4.060x)
  zstd -19 per-64KiB         601434 B  (4.896x)
  efs tree (post-GC):            1196335 B reachable (2.449x) / 2934884 B backing
  efs tree + shared dict:        1055712 B reachable (2.775x) / 3585001 B backing (rewrote 251 extents, saved 160143 B)
  families before: {"RANS": 34, "RAW": 11, "SEQUENCE_DICT": 3, "SEQUENCE_RANS": 245}
  families after:  {"RANS": 19, "RAW": 7, "SEQUENCE_DEEP": 32, "SEQUENCE_DICT": 3, "SEQUENCE_RANS": 13, "SEQUENCE_SHARED_DICT": 219}
  zstd -1 per-file +dir anchor:     744835 B  (3.934x)  [Phase-9F anchor-policy control]
  per-extent overhead: 42797 B descriptors + 74348 B models = 117145 B (11.1% of footprint, 4.0% of logical)  [Phase-9F]

== baselines ==
  waived: btrfs with compression: waived — writable compressed-FS baseline requires root for loop-mounting a test image
  waived: EROFS/SquashFS: waived — read-only compressed-image baseline deferred (requires root and/or mkfs.erofs)
  raw file (ext4): 4523.2 MiB/s write, ratio 1.000x
  zstd -1: 2940187 → 580314 bytes (5.067x), 0.004s
  zstd -19: 2940187 → 392758 bytes (7.486x), 0.407s
  zstd -1 per 64KiB: 2940187 → 722630 bytes (4.069x), 0.029s
  zstd -19 per 64KiB: 2940187 → 599133 bytes (4.907x), 0.578s
  direct byte rANS (same backend, src corpus): 2940187 → 1795565 bytes (1.637x)
  standalone SequenceRans (src corpus): 2940187 → 773427 bytes (3.802x)
  standalone SequenceDeep (src corpus): 2940187 → 746114 bytes (3.941x)
device nvme1n1p1: 645000 sectors written (330240000 bytes), 40 sectors read (20480 bytes)

== admission checklist (methodology §8) ==
  [OK ] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
  [OK ] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
  [OK ] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
  [OK ] ablations identify which mechanism caused the gain (cumulative ladder A0-A8 + leave-one-out) — leave-one-out table 15 rows; cumulative ladder 13 rows (A0-A8)
  [OK ] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
  [OK ] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
  [OK ] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md
```
