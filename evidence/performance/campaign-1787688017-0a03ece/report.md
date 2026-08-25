# EntropyFS evidence campaign

- campaign dir: `evidence/performance/campaign-1787688017-0a03ece`
- created: unix 1787688017

## Admission checklist (methodology §8)

- [x] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
- [x] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
- [x] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
- [x] ablations identify which mechanism caused the gain (cumulative ladder A0-A8 + leave-one-out) — leave-one-out table 15 rows; cumulative ladder 13 rows (A0-A8)
- [x] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
- [x] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
- [x] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md

## Summary

- `structured[full]`: 3 runs — write 1279.7 MiB/s (p50 45297µs, p95 63198µs, p99 63198µs), read 2998.9 MiB/s, fsync p50 1636µs, physical median 50528 bytes, ratio 1328.152x
- `src[full]`: 3 runs — write 35.0 MiB/s (p50 87393µs, p95 87450µs, p99 87450µs), read 181.6 MiB/s, fsync p50 1658µs, physical median 730161 bytes, ratio 4.406x
- `urandom[full]`: 3 runs — write 32.3 MiB/s (p50 986905µs, p95 992719µs, p99 992719µs), read 4028.2 MiB/s, fsync p50 1652µs, physical median 33652515 bytes, ratio 0.997x
- `compressed-z19[full]`: 3 runs — write 36.6 MiB/s (p50 10799µs, p95 11215µs, p99 11215µs), read 4182.0 MiB/s, fsync p50 1656µs, physical median 416851 bytes, ratio 0.995x
- `versioned[full]`: 3 runs — write 81.7 MiB/s (p50 35617µs, p95 92133µs, p99 96018µs), read 567.9 MiB/s, fsync p50 1647µs, physical median 1359786 bytes, ratio 3.085x
- `versioned[no-base]`: 3 runs — write 74.1 MiB/s (p50 58506µs, p95 60567µs, p99 60630µs), read 1731.6 MiB/s, fsync p50 2186µs, physical median 1180500 bytes, ratio 3.553x
- `shuffled[full]`: 3 runs — write 66.2 MiB/s (p50 60421µs, p95 82005µs, p99 83018µs), read 844.9 MiB/s, fsync p50 1674µs, physical median 2346969 bytes, ratio 1.787x

Device writes during campaign window (nvme1n1p1): 294096896 bytes written, 12288 bytes read.

## Raw output

```text
entropyfs evidence campaign — 1787688017
revision: 0a03ece
  structured [full] 3 runs: write 1279.7 MiB/s (p50 45297µs, p95 63198µs, p99 63198µs) read 2998.9 MiB/s fsync p50 1636µs p95 3588µs p99 3588µs physical median 50528 ratio 1328.152x
  src [full] 3 runs: write 35.0 MiB/s (p50 87393µs, p95 87450µs, p99 87450µs) read 181.6 MiB/s fsync p50 1658µs p95 2593µs p99 2593µs physical median 730161 ratio 4.406x
  urandom [full] 3 runs: write 32.3 MiB/s (p50 986905µs, p95 992719µs, p99 992719µs) read 4028.2 MiB/s fsync p50 1652µs p95 12196µs p99 12196µs physical median 33652515 ratio 0.997x
  compressed-z19 [full] 3 runs: write 36.6 MiB/s (p50 10799µs, p95 11215µs, p99 11215µs) read 4182.0 MiB/s fsync p50 1656µs p95 2485µs p99 2485µs physical median 416851 ratio 0.995x

== leave-one-out ablation (structured) ==
  full       physical        50528 ratio 1328.152x write   1291.5 MiB/s cpu 0.050+0.000s (p95 write 44371µs)
  raw        physical       319070 ratio 210.326x write   1485.7 MiB/s cpu 0.040+0.000s (p95 write 38517µs)
  raw-byte-rans physical       267181 ratio 251.174x write    652.5 MiB/s cpu 0.100+0.000s (p95 write 93321µs)
  no-exact-ref physical        67868 ratio 988.815x write   1247.4 MiB/s cpu 0.050+0.000s (p95 write 46355µs)
  no-base    physical        50528 ratio 1328.152x write   1273.3 MiB/s cpu 0.050+0.000s (p95 write 45544µs)
  no-temporal physical        50528 ratio 1328.152x write   1259.5 MiB/s cpu 0.040+0.000s (p95 write 45323µs)
  no-config  physical        64939 ratio 1033.414x write    778.1 MiB/s cpu 0.080+0.000s (p95 write 77267µs)
  no-rans    physical       112301 ratio 597.580x write   1363.0 MiB/s cpu 0.050+0.000s (p95 write 41947µs)
  no-byte-rans physical        50528 ratio 1328.152x write   1249.6 MiB/s cpu 0.050+0.010s (p95 write 46101µs)
  no-sequence-rans physical       112301 ratio 597.580x write   1346.3 MiB/s cpu 0.040+0.000s (p95 write 42554µs)
  no-deep    physical        50528 ratio 1328.152x write   1280.7 MiB/s cpu 0.050+0.000s (p95 write 44867µs)
  no-sequence-dict physical        50528 ratio 1328.152x write   1274.8 MiB/s cpu 0.050+0.000s (p95 write 45619µs)
  no-shared-dict physical        50528 ratio 1328.152x write   1273.1 MiB/s cpu 0.050+0.000s (p95 write 44983µs)
  no-universe physical        50528 ratio 1328.152x write   1287.5 MiB/s cpu 0.050+0.000s (p95 write 45257µs)
  no-dsfb    physical        50528 ratio 1328.152x write   1282.7 MiB/s cpu 0.050+0.000s (p95 write 44782µs)

== cumulative ladder A0-A8 (structured) ==
  A0-raw             physical       319070 ratio 210.326x write   1474.9 MiB/s cpu 0.040+0.010s (p95 write 38315µs)
  A1-byte-rans       physical       267181 ratio 251.174x write    647.1 MiB/s cpu 0.100+0.000s (p95 write 93947µs)
  A2-exact-ref       physical       251881 ratio 266.431x write    648.7 MiB/s cpu 0.100+0.000s (p95 write 93948µs)
  A3-base-residual   physical       251881 ratio 266.431x write    655.8 MiB/s cpu 0.090+0.000s (p95 write 92725µs)
  A4-config          physical       112301 ratio 597.580x write   1369.1 MiB/s cpu 0.050+0.000s (p95 write 41926µs)
  A5-temporal-bases  physical       112301 ratio 597.580x write   1365.9 MiB/s cpu 0.040+0.010s (p95 write 41886µs)
  A6-universe        physical       112301 ratio 597.580x write   1364.0 MiB/s cpu 0.050+0.000s (p95 write 41742µs)
  A7-dsfb            physical       112301 ratio 597.580x write   1384.3 MiB/s cpu 0.050+0.000s (p95 write 40650µs)
  A8-background      physical       112301 ratio 597.580x write   1330.3 MiB/s cpu 0.040+0.000s (p95 write 42491µs)
  E1-sequence-rans   physical        50528 ratio 1328.152x write   1247.0 MiB/s cpu 0.050+0.010s (p95 write 46445µs)
  E2-sequence-dict   physical        50528 ratio 1328.152x write   1279.0 MiB/s cpu 0.050+0.000s (p95 write 45347µs)
  E3-shared-dict     physical        50528 ratio 1328.152x write   1271.3 MiB/s cpu 0.050+0.000s (p95 write 45484µs)
  E4-deep            physical        50238 ratio 1335.819x write   1196.0 MiB/s cpu 0.050+0.000s (p95 write 48405µs)

== DSFB search-budget investigation (structured) ==
  full      write median  1246.2 MiB/s (min 1242.6, max 1267.4) cpu median 0.050s physical [50528, 50528, 50528]
  no-dsfb   write median  1274.6 MiB/s (min 1235.1, max 1288.3) cpu median 0.050s physical [50528, 50528, 50528]
  physical identical across modes: true

== versioned experiment (H2) ==
  versioned [full] 3 runs: write 81.7 MiB/s (p50 35617µs, p95 92133µs, p99 96018µs) read 567.9 MiB/s fsync p50 1647µs p95 4131µs p99 4131µs physical median 1359786 ratio 3.085x
  versioned [no-base] 3 runs: write 74.1 MiB/s (p50 58506µs, p95 60567µs, p99 60630µs) read 1731.6 MiB/s fsync p50 2186µs p95 10256µs p99 10256µs physical median 1180500 ratio 3.553x
  shuffled [full] 3 runs: write 66.2 MiB/s (p50 60421µs, p95 82005µs, p99 83018µs) read 844.9 MiB/s fsync p50 1674µs p95 6752µs p99 6752µs physical median 2346969 ratio 1.787x
  sequential median reachable: 1359786 bytes (3.085x)
  shuffled    median reachable: 2346969 bytes (1.787x)
  base+residual savings vs shuffled: 987183 bytes (42.1% of shuffled reachable)
  post-GC reachable: sequential full 1233207 (3.401x) / no-base 1131427 (3.707x) / shuffled 2289831 (1.832x)

== GC and optimizer traffic ==
  unreachable before 5455393 → reclaimed 5455393 → after 201033; physical 39268875 → 33853181; gc 0.018s; optimizer scanned 512 rewrote 0 saved 0
  unreachable by record tag (post-GC): {"BtreeNode": 200795, "Root": 238}

== post-GC physical footprint ==
  src: logical 3217274 → reachable 730161 (4.41x) / total backing 735716 (4.37x) / allocated 741376 (4.34x)
  structured: logical 67108864 → reachable 50528 (1328.15x) / total backing 56083 (1196.60x) / allocated 61440 (1092.27x)
  urandom: logical 33554432 → reachable 33652515 (1.00x) / total backing 33658070 (1.00x) / allocated 33665024 (1.00x)
  compressed-z19: logical 414638 → reachable 416851 (0.99x) / total backing 422406 (0.98x) / allocated 430080 (0.96x)

== Phase-9C tree court ==
  files 307 (single-chunk 304), logical 3205831 B
  zstd -1 whole              617325 B  (5.219x)
  zstd -19 whole             414779 B  (7.767x)
  zstd -1 per-file           881656 B  (3.636x)
  zstd -19 per-file          771568 B  (4.155x)
  zstd -1 per-64KiB          782862 B  (4.115x)
  zstd -19 per-64KiB         650044 B  (4.956x)
  efs tree (post-GC):            1285868 B reachable (2.493x) / 1290480 B backing
  efs tree + shared dict:        1127173 B reachable (2.844x) / 1131785 B backing (rewrote 268 extents, saved 178715 B)
  efs + model bundles:          1100157 B reachable (2.914x) / 1104769 B backing (rewrote 67 extents, saved 8075 B)  [Phase-9G]
  families before: {"RANS": 36, "RAW": 11, "SEQUENCE_DICT": 3, "SEQUENCE_RANS": 260}
  families after:  {"RANS": 21, "RAW": 7, "SEQUENCE_DEEP": 36, "SEQUENCE_DICT": 3, "SEQUENCE_RANS": 11, "SEQUENCE_SHARED_DICT": 232}
  families +models: {"RANS": 21, "RAW": 7, "SEQUENCE_DEEP": 36, "SEQUENCE_DICT": 3, "SEQUENCE_RANS": 11, "SEQUENCE_SHARED_DICT": 232}
  zstd -1 per-file +dir anchor:     802944 B  (3.993x)  [Phase-9F anchor-policy control]
  physical post-GC:    1100161 B backing = reachable + 0 B dead-indexed + 0 B index-hidden + 0 B unindexed  [Phase-9H]
  + full compact:    1100161 B backing (4 B overhead over reachable; reclaimed 0 B)  [Phase-9H]
  per-extent overhead: 45308 B descriptors + 76413 B models = 121721 B (11.1% of footprint, 3.8% of logical)  [Phase-9F]

== baselines ==
  waived: btrfs with compression: waived — writable compressed-FS baseline requires root for loop-mounting a test image
  waived: EROFS/SquashFS: waived — read-only compressed-image baseline deferred (requires root and/or mkfs.erofs)
  raw file (ext4): 4833.2 MiB/s write, ratio 1.000x
  zstd -1: 3217274 → 616783 bytes (5.216x), 0.004s
  zstd -19: 3217274 → 414638 bytes (7.759x), 0.444s
  zstd -1 per 64KiB: 3217274 → 782977 bytes (4.109x), 0.032s
  zstd -19 per 64KiB: 3217274 → 650178 bytes (4.948x), 0.637s
  direct byte rANS (same backend, src corpus): 3217274 → 1965323 bytes (1.637x)
  standalone SequenceRans (src corpus): 3217274 → 840867 bytes (3.826x)
  standalone SequenceDeep (src corpus): 3217274 → 810812 bytes (3.968x)
device nvme1n1p1: 574408 sectors written (294096896 bytes), 24 sectors read (12288 bytes)

== admission checklist (methodology §8) ==
  [OK ] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
  [OK ] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
  [OK ] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
  [OK ] ablations identify which mechanism caused the gain (cumulative ladder A0-A8 + leave-one-out) — leave-one-out table 15 rows; cumulative ladder 13 rows (A0-A8)
  [OK ] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
  [OK ] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
  [OK ] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md
```
