# EntropyFS evidence campaign

- campaign dir: `evidence/performance/campaign-1787683904-da26c75`
- created: unix 1787683904

## Admission checklist (methodology §8)

- [x] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
- [x] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
- [x] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
- [x] ablations identify which mechanism caused the gain (cumulative ladder A0-A8 + leave-one-out) — leave-one-out table 15 rows; cumulative ladder 13 rows (A0-A8)
- [x] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
- [x] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
- [x] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md

## Summary

- `structured[full]`: 3 runs — write 1264.1 MiB/s (p50 45636µs, p95 61369µs, p99 61369µs), read 3013.9 MiB/s, fsync p50 1644µs, physical median 50528 bytes, ratio 1328.152x
- `src[full]`: 3 runs — write 34.2 MiB/s (p50 78349µs, p95 78951µs, p99 78951µs), read 166.7 MiB/s, fsync p50 1659µs, physical median 665032 bytes, ratio 4.235x
- `urandom[full]`: 3 runs — write 32.7 MiB/s (p50 975475µs, p95 997631µs, p99 997631µs), read 3942.9 MiB/s, fsync p50 1660µs, physical median 33652515 bytes, ratio 0.997x
- `compressed-z19[full]`: 3 runs — write 35.2 MiB/s (p50 10371µs, p95 10682µs, p99 10682µs), read 3825.0 MiB/s, fsync p50 1635µs, physical median 386059 bytes, ratio 0.995x
- `versioned[full]`: 3 runs — write 81.2 MiB/s (p50 34574µs, p95 93045µs, p99 94658µs), read 538.5 MiB/s, fsync p50 1655µs, physical median 1392236 bytes, ratio 3.013x
- `versioned[no-base]`: 3 runs — write 75.1 MiB/s (p50 56847µs, p95 59528µs, p99 60486µs), read 1573.8 MiB/s, fsync p50 1660µs, physical median 1214754 bytes, ratio 3.453x
- `shuffled[full]`: 3 runs — write 64.9 MiB/s (p50 63761µs, p95 80178µs, p99 80843µs), read 826.1 MiB/s, fsync p50 1675µs, physical median 2345529 bytes, ratio 1.788x

Device writes during campaign window (nvme1n1p1): 303472640 bytes written, 40960 bytes read.

## Raw output

```text
entropyfs evidence campaign — 1787683904
revision: da26c75
  structured [full] 3 runs: write 1264.1 MiB/s (p50 45636µs, p95 61369µs, p99 61369µs) read 3013.9 MiB/s fsync p50 1644µs p95 2390µs p99 2390µs physical median 50528 ratio 1328.152x
  src [full] 3 runs: write 34.2 MiB/s (p50 78349µs, p95 78951µs, p99 78951µs) read 166.7 MiB/s fsync p50 1659µs p95 2575µs p99 2575µs physical median 665032 ratio 4.235x
  urandom [full] 3 runs: write 32.7 MiB/s (p50 975475µs, p95 997631µs, p99 997631µs) read 3942.9 MiB/s fsync p50 1660µs p95 26218µs p99 26218µs physical median 33652515 ratio 0.997x
  compressed-z19 [full] 3 runs: write 35.2 MiB/s (p50 10371µs, p95 10682µs, p99 10682µs) read 3825.0 MiB/s fsync p50 1635µs p95 2758µs p99 2758µs physical median 386059 ratio 0.995x

== leave-one-out ablation (structured) ==
  full       physical        50528 ratio 1328.152x write   1152.3 MiB/s cpu 0.050+0.000s (p95 write 49575µs)
  raw        physical       319070 ratio 210.326x write   1379.3 MiB/s cpu 0.040+0.000s (p95 write 40941µs)
  raw-byte-rans physical       267181 ratio 251.174x write    607.7 MiB/s cpu 0.100+0.000s (p95 write 99836µs)
  no-exact-ref physical        67868 ratio 988.815x write   1146.2 MiB/s cpu 0.050+0.000s (p95 write 50491µs)
  no-base    physical        50528 ratio 1328.152x write   1169.6 MiB/s cpu 0.050+0.010s (p95 write 49690µs)
  no-temporal physical        50528 ratio 1328.152x write   1172.0 MiB/s cpu 0.050+0.000s (p95 write 48821µs)
  no-config  physical        64976 ratio 1032.825x write    776.6 MiB/s cpu 0.070+0.010s (p95 write 77271µs)
  no-rans    physical       112301 ratio 597.580x write   1279.0 MiB/s cpu 0.050+0.000s (p95 write 44378µs)
  no-byte-rans physical        50528 ratio 1328.152x write   1232.9 MiB/s cpu 0.050+0.000s (p95 write 46710µs)
  no-sequence-rans physical       112301 ratio 597.580x write   1324.6 MiB/s cpu 0.050+0.000s (p95 write 43246µs)
  no-deep    physical        50528 ratio 1328.152x write   1255.1 MiB/s cpu 0.050+0.000s (p95 write 46168µs)
  no-sequence-dict physical        50528 ratio 1328.152x write   1255.7 MiB/s cpu 0.050+0.000s (p95 write 45737µs)
  no-shared-dict physical        50528 ratio 1328.152x write   1149.7 MiB/s cpu 0.050+0.000s (p95 write 50262µs)
  no-universe physical        50528 ratio 1328.152x write   1279.3 MiB/s cpu 0.050+0.000s (p95 write 45289µs)
  no-dsfb    physical        50528 ratio 1328.152x write   1289.9 MiB/s cpu 0.040+0.000s (p95 write 44905µs)

== cumulative ladder A0-A8 (structured) ==
  A0-raw             physical       319070 ratio 210.326x write   1452.3 MiB/s cpu 0.040+0.000s (p95 write 39197µs)
  A1-byte-rans       physical       267181 ratio 251.174x write    658.2 MiB/s cpu 0.100+0.000s (p95 write 92681µs)
  A2-exact-ref       physical       251881 ratio 266.431x write    662.6 MiB/s cpu 0.090+0.000s (p95 write 91900µs)
  A3-base-residual   physical       251881 ratio 266.431x write    659.2 MiB/s cpu 0.100+0.000s (p95 write 92264µs)
  A4-config          physical       112301 ratio 597.580x write   1382.6 MiB/s cpu 0.050+0.000s (p95 write 41575µs)
  A5-temporal-bases  physical       112301 ratio 597.580x write   1365.4 MiB/s cpu 0.040+0.000s (p95 write 41523µs)
  A6-universe        physical       112301 ratio 597.580x write   1351.5 MiB/s cpu 0.050+0.000s (p95 write 42229µs)
  A7-dsfb            physical       112301 ratio 597.580x write   1376.0 MiB/s cpu 0.050+0.000s (p95 write 41549µs)
  A8-background      physical       112301 ratio 597.580x write   1385.5 MiB/s cpu 0.040+0.010s (p95 write 41424µs)
  E1-sequence-rans   physical        50528 ratio 1328.152x write   1265.7 MiB/s cpu 0.040+0.000s (p95 write 45984µs)
  E2-sequence-dict   physical        50528 ratio 1328.152x write   1248.8 MiB/s cpu 0.050+0.000s (p95 write 46435µs)
  E3-shared-dict     physical        50528 ratio 1328.152x write   1267.6 MiB/s cpu 0.050+0.000s (p95 write 45756µs)
  E4-deep            physical        50238 ratio 1335.819x write   1289.7 MiB/s cpu 0.050+0.000s (p95 write 44946µs)

== DSFB search-budget investigation (structured) ==
  full      write median  1289.3 MiB/s (min 1260.8, max 1299.3) cpu median 0.050s physical [50528, 50528, 50528]
  no-dsfb   write median  1259.4 MiB/s (min 1246.6, max 1265.6) cpu median 0.050s physical [50528, 50528, 50528]
  physical identical across modes: true

== versioned experiment (H2) ==
  versioned [full] 3 runs: write 81.2 MiB/s (p50 34574µs, p95 93045µs, p99 94658µs) read 538.5 MiB/s fsync p50 1655µs p95 4592µs p99 4592µs physical median 1392236 ratio 3.013x
  versioned [no-base] 3 runs: write 75.1 MiB/s (p50 56847µs, p95 59528µs, p99 60486µs) read 1573.8 MiB/s fsync p50 1660µs p95 5443µs p99 5443µs physical median 1214754 ratio 3.453x
  shuffled [full] 3 runs: write 64.9 MiB/s (p50 63761µs, p95 80178µs, p99 80843µs) read 826.1 MiB/s fsync p50 1675µs p95 10880µs p99 10880µs physical median 2345529 ratio 1.788x
  sequential median reachable: 1392236 bytes (3.013x)
  shuffled    median reachable: 2345529 bytes (1.788x)
  base+residual savings vs shuffled: 953293 bytes (40.6% of shuffled reachable)
  post-GC reachable: sequential full 1265786 (3.314x) / no-base 1165681 (3.598x) / shuffled 2287928 (1.833x)

== GC and optimizer traffic ==
  unreachable before 5537444 → reclaimed 5537444 → after 2274864; physical 39350926 → 35927915; gc 0.014s; optimizer scanned 512 rewrote 0 saved 0
  unreachable by record tag (post-GC): {"BtreeNode": 2274626, "Root": 238}

== post-GC physical footprint ==
  src: logical 2816381 → reachable 665032 (4.23x) / total backing 670587 (4.20x) / allocated 675840 (4.17x)
  structured: logical 67108864 → reachable 50528 (1328.15x) / total backing 56083 (1196.60x) / allocated 61440 (1092.27x)
  urandom: logical 33554432 → reachable 33652515 (1.00x) / total backing 33658070 (1.00x) / allocated 33665024 (1.00x)
  compressed-z19: logical 384030 → reachable 386059 (0.99x) / total backing 391614 (0.98x) / allocated 397312 (0.97x)

== Phase-9C tree court ==
  files 282 (single-chunk 279), logical 2806671 B
  zstd -1 whole              567002 B  (4.975x)
  zstd -19 whole             383999 B  (7.345x)
  zstd -1 per-file           787161 B  (3.566x)
  zstd -19 per-file          689974 B  (4.068x)
  zstd -1 per-64KiB          704574 B  (4.003x)
  zstd -19 per-64KiB         584546 B  (4.825x)
  efs tree (post-GC):            1264003 B reachable (2.220x) / 2860260 B backing
  efs tree + shared dict:        1175511 B reachable (2.388x) / 3270764 B backing (rewrote 157 extents, saved 98799 B)
  families before: {"RANS": 108, "RAW": 19, "SEQUENCE_DICT": 3, "SEQUENCE_RANS": 155}
  families after:  {"RANS": 92, "RAW": 19, "SEQUENCE_DEEP": 16, "SEQUENCE_DICT": 3, "SEQUENCE_RANS": 14, "SEQUENCE_SHARED_DICT": 141}
  zstd -1 per-file +dir anchor:     720081 B  (3.898x)  [Phase-9F anchor-policy control]
  per-extent overhead: 33969 B descriptors + 277556 B models = 311525 B (26.5% of footprint, 11.1% of logical)  [Phase-9F]

== baselines ==
  waived: btrfs with compression: waived — writable compressed-FS baseline requires root for loop-mounting a test image
  waived: EROFS/SquashFS: waived — read-only compressed-image baseline deferred (requires root and/or mkfs.erofs)
  raw file (ext4): 2206.9 MiB/s write, ratio 1.000x
  zstd -1: 2816381 → 566432 bytes (4.972x), 0.005s
  zstd -19: 2816381 → 384030 bytes (7.334x), 0.382s
  zstd -1 per 64KiB: 2816381 → 702585 bytes (4.009x), 0.028s
  zstd -19 per 64KiB: 2816381 → 582857 bytes (4.832x), 0.544s
  direct byte rANS (same backend, src corpus): 2816381 → 1719985 bytes (1.637x)
  standalone SequenceRans (src corpus): 2816381 → 749891 bytes (3.756x)
  standalone SequenceDeep (src corpus): 2816381 → 739547 bytes (3.808x)
device nvme1n1p1: 592720 sectors written (303472640 bytes), 80 sectors read (40960 bytes)

== admission checklist (methodology §8) ==
  [OK ] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
  [OK ] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
  [OK ] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
  [OK ] ablations identify which mechanism caused the gain (cumulative ladder A0-A8 + leave-one-out) — leave-one-out table 15 rows; cumulative ladder 13 rows (A0-A8)
  [OK ] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
  [OK ] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
  [OK ] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md
```
