# EntropyFS evidence campaign

- campaign dir: `evidence/performance/campaign-1787671040-923df7b`
- created: unix 1787671040

## Admission checklist (methodology §8)

- [x] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
- [x] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
- [x] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
- [x] ablations identify which mechanism caused the gain (cumulative ladder A0-A8 + leave-one-out) — leave-one-out table 12 rows; cumulative ladder 10 rows (A0-A8)
- [x] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
- [x] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
- [x] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md

## Summary

- `structured[full]`: 5 runs — write 1119.1 MiB/s (p50 52493µs, p95 69864µs, p99 69864µs), read 2961.1 MiB/s, fsync p50 1658µs, physical median 50528 bytes, ratio 1328.152x
- `src[full]`: 3 runs — write 57.1 MiB/s (p50 33767µs, p95 33966µs, p99 33966µs), read 406.6 MiB/s, fsync p50 1649µs, physical median 571134 bytes, ratio 3.556x
- `urandom[full]`: 3 runs — write 93.5 MiB/s (p50 340389µs, p95 341048µs, p99 341048µs), read 4069.8 MiB/s, fsync p50 1639µs, physical median 33652515 bytes, ratio 0.997x
- `compressed-z19[full]`: 3 runs — write 96.9 MiB/s (p50 3099µs, p95 3193µs, p99 3193µs), read 3961.7 MiB/s, fsync p50 1652µs, physical median 317603 bytes, ratio 0.994x
- `versioned[full]`: 3 runs — write 79.0 MiB/s (p50 36310µs, p95 97973µs, p99 98187µs), read 541.0 MiB/s, fsync p50 1680µs, physical median 1392236 bytes, ratio 3.013x
- `versioned[no-base]`: 3 runs — write 116.0 MiB/s (p50 37072µs, p95 39608µs, p99 39646µs), read 1536.3 MiB/s, fsync p50 1651µs, physical median 1214754 bytes, ratio 3.453x
- `shuffled[full]`: 3 runs — write 79.9 MiB/s (p50 50662µs, p95 66217µs, p99 66966µs), read 892.0 MiB/s, fsync p50 1646µs, physical median 2345529 bytes, ratio 1.788x

Device writes during campaign window (nvme1n1p1): 438853632 bytes written, 86016 bytes read.

## Raw output

```text
entropyfs evidence campaign — 1787671040
revision: 923df7b
  structured [full] 5 runs: write 1119.1 MiB/s (p50 52493µs, p95 69864µs, p99 69864µs) read 2961.1 MiB/s fsync p50 1658µs p95 3633µs p99 3679µs physical median 50528 ratio 1328.152x
  src [full] 3 runs: write 57.1 MiB/s (p50 33767µs, p95 33966µs, p99 33966µs) read 406.6 MiB/s fsync p50 1649µs p95 2517µs p99 2517µs physical median 571134 ratio 3.556x
  urandom [full] 3 runs: write 93.5 MiB/s (p50 340389µs, p95 341048µs, p99 341048µs) read 4069.8 MiB/s fsync p50 1639µs p95 13373µs p99 13373µs physical median 33652515 ratio 0.997x
  compressed-z19 [full] 3 runs: write 96.9 MiB/s (p50 3099µs, p95 3193µs, p99 3193µs) read 3961.7 MiB/s fsync p50 1652µs p95 2453µs p99 2453µs physical median 317603 ratio 0.994x

== leave-one-out ablation (structured) ==
  full       physical        50528 ratio 1328.152x write   1120.4 MiB/s cpu 0.060+0.000s (p95 write 52582µs)
  raw        physical       319070 ratio 210.326x write   1251.0 MiB/s cpu 0.050+0.000s (p95 write 46486µs)
  raw-byte-rans physical       267181 ratio 251.174x write    580.5 MiB/s cpu 0.100+0.000s (p95 write 105083µs)
  no-exact-ref physical        67868 ratio 988.815x write   1045.8 MiB/s cpu 0.060+0.000s (p95 write 56505µs)
  no-base    physical        50528 ratio 1328.152x write   1092.0 MiB/s cpu 0.060+0.010s (p95 write 53518µs)
  no-temporal physical        50528 ratio 1328.152x write   1099.3 MiB/s cpu 0.050+0.000s (p95 write 53670µs)
  no-config  physical        64976 ratio 1032.825x write    703.4 MiB/s cpu 0.090+0.000s (p95 write 86093µs)
  no-rans    physical       112301 ratio 597.580x write   1188.2 MiB/s cpu 0.050+0.000s (p95 write 49215µs)
  no-byte-rans physical        50528 ratio 1328.152x write   1117.0 MiB/s cpu 0.060+0.000s (p95 write 52532µs)
  no-sequence-rans physical       112301 ratio 597.580x write   1198.0 MiB/s cpu 0.050+0.000s (p95 write 48746µs)
  no-universe physical        50528 ratio 1328.152x write   1104.6 MiB/s cpu 0.060+0.010s (p95 write 52974µs)
  no-dsfb    physical        50528 ratio 1328.152x write   1099.8 MiB/s cpu 0.060+0.000s (p95 write 53571µs)

== cumulative ladder A0-A8 (structured) ==
  A0-raw             physical       319070 ratio 210.326x write   1189.2 MiB/s cpu 0.050+0.000s (p95 write 48833µs)
  A1-byte-rans       physical       267181 ratio 251.174x write    596.3 MiB/s cpu 0.100+0.000s (p95 write 102562µs)
  A2-exact-ref       physical       251881 ratio 266.431x write    608.2 MiB/s cpu 0.100+0.000s (p95 write 100620µs)
  A3-base-residual   physical       251881 ratio 266.431x write    597.3 MiB/s cpu 0.110+0.010s (p95 write 102141µs)
  A4-config          physical       112301 ratio 597.580x write   1155.7 MiB/s cpu 0.050+0.000s (p95 write 50097µs)
  A5-temporal-bases  physical       112301 ratio 597.580x write   1170.7 MiB/s cpu 0.050+0.000s (p95 write 49894µs)
  A6-universe        physical       112301 ratio 597.580x write   1201.4 MiB/s cpu 0.050+0.000s (p95 write 48540µs)
  A7-dsfb            physical       112301 ratio 597.580x write   1206.9 MiB/s cpu 0.050+0.000s (p95 write 48456µs)
  A8-background      physical       112301 ratio 597.580x write   1188.4 MiB/s cpu 0.050+0.010s (p95 write 48994µs)
  E1-sequence-rans   physical        50528 ratio 1328.152x write   1112.0 MiB/s cpu 0.050+0.010s (p95 write 52896µs)

== DSFB search-budget investigation (structured) ==
  full      write median  1120.8 MiB/s (min 1111.5, max 1128.7) cpu median 0.050s physical [50528, 50528, 50528, 50528, 50528]
  no-dsfb   write median  1106.1 MiB/s (min 1092.4, max 1128.3) cpu median 0.060s physical [50528, 50528, 50528, 50528, 50528]
  physical identical across modes: true

== versioned experiment (H2) ==
  versioned [full] 3 runs: write 79.0 MiB/s (p50 36310µs, p95 97973µs, p99 98187µs) read 541.0 MiB/s fsync p50 1680µs p95 14413µs p99 14413µs physical median 1392236 ratio 3.013x
  versioned [no-base] 3 runs: write 116.0 MiB/s (p50 37072µs, p95 39608µs, p99 39646µs) read 1536.3 MiB/s fsync p50 1651µs p95 7001µs p99 7001µs physical median 1214754 ratio 3.453x
  shuffled [full] 3 runs: write 79.9 MiB/s (p50 50662µs, p95 66217µs, p99 66966µs) read 892.0 MiB/s fsync p50 1646µs p95 8897µs p99 8897µs physical median 2345529 ratio 1.788x
  sequential median reachable: 1392236 bytes (3.013x)
  shuffled    median reachable: 2345529 bytes (1.788x)
  base+residual savings vs shuffled: 953293 bytes (40.6% of shuffled reachable)
  post-GC reachable: sequential full 1265786 (3.314x) / no-base 1165681 (3.598x) / shuffled 2287928 (1.833x)

== GC and optimizer traffic ==
  unreachable before 43262407 → reclaimed 33506524 → after 12030747; physical 77075909 → 45683806; gc 0.015s; optimizer scanned 512 rewrote 0 saved 0

== post-GC physical footprint ==
  src: logical 2030799 → reachable 571134 (3.56x) / total backing 700377 (2.90x) / allocated 704512 (2.88x)
  structured: logical 67108864 → reachable 50528 (1328.15x) / total backing 55921 (1200.07x) / allocated 61440 (1092.27x)
  urandom: logical 33554432 → reachable 33652515 (1.00x) / total backing 37206421 (0.90x) / allocated 37212160 (0.90x)
  compressed-z19: logical 315758 → reachable 317603 (0.99x) / total backing 326712 (0.97x) / allocated 331776 (0.95x)

== baselines ==
  waived: btrfs with compression: waived — writable compressed-FS baseline requires root for loop-mounting a test image
  waived: EROFS/SquashFS: waived — read-only compressed-image baseline deferred (requires root and/or mkfs.erofs)
  raw file (ext4): 7802.3 MiB/s write, ratio 1.000x
  zstd -1: 2030799 → 459489 bytes (4.420x), 0.005s
  zstd -19: 2030799 → 315758 bytes (6.432x), 0.279s
  zstd -1 per 64KiB: 2030799 → 543128 bytes (3.739x), 0.021s
  zstd -19 per 64KiB: 2030799 → 450339 bytes (4.509x), 0.400s
  direct byte rANS (same backend, src corpus): 2030799 → 1243242 bytes (1.633x)
  standalone SequenceRans (src corpus): 2030799 → 571134 bytes (3.556x)
device nvme1n1p1: 857136 sectors written (438853632 bytes), 168 sectors read (86016 bytes)

== admission checklist (methodology §8) ==
  [OK ] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
  [OK ] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
  [OK ] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
  [OK ] ablations identify which mechanism caused the gain (cumulative ladder A0-A8 + leave-one-out) — leave-one-out table 12 rows; cumulative ladder 10 rows (A0-A8)
  [OK ] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
  [OK ] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
  [OK ] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md
```
