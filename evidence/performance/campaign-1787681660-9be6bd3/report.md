# EntropyFS evidence campaign

- campaign dir: `evidence/performance/campaign-1787681660-9be6bd3`
- created: unix 1787681660

## Admission checklist (methodology §8)

- [x] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
- [x] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
- [x] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
- [x] ablations identify which mechanism caused the gain (cumulative ladder A0-A8 + leave-one-out) — leave-one-out table 15 rows; cumulative ladder 13 rows (A0-A8)
- [x] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
- [x] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
- [x] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md

## Summary

- `structured[full]`: 3 runs — write 1278.5 MiB/s (p50 45233µs, p95 62577µs, p99 62577µs), read 2981.6 MiB/s, fsync p50 1646µs, physical median 50528 bytes, ratio 1328.152x
- `src[full]`: 3 runs — write 33.4 MiB/s (p50 76815µs, p95 76890µs, p99 76890µs), read 160.5 MiB/s, fsync p50 1644µs, physical median 646427 bytes, ratio 4.173x
- `urandom[full]`: 3 runs — write 32.6 MiB/s (p50 979913µs, p95 991724µs, p99 991724µs), read 4059.2 MiB/s, fsync p50 1815µs, physical median 33652515 bytes, ratio 0.997x
- `compressed-z19[full]`: 3 runs — write 36.3 MiB/s (p50 9863µs, p95 9916µs, p99 9916µs), read 3900.5 MiB/s, fsync p50 1646µs, physical median 378638 bytes, ratio 0.995x
- `versioned[full]`: 3 runs — write 81.8 MiB/s (p50 33958µs, p95 92261µs, p99 95275µs), read 532.5 MiB/s, fsync p50 1658µs, physical median 1392236 bytes, ratio 3.013x
- `versioned[no-base]`: 3 runs — write 74.1 MiB/s (p50 57521µs, p95 61256µs, p99 62058µs), read 1532.3 MiB/s, fsync p50 1656µs, physical median 1214754 bytes, ratio 3.453x
- `shuffled[full]`: 3 runs — write 62.8 MiB/s (p50 66127µs, p95 81634µs, p99 83642µs), read 868.8 MiB/s, fsync p50 1658µs, physical median 2345529 bytes, ratio 1.788x

Device writes during campaign window (nvme1n1p1): 312524800 bytes written, 8192 bytes read.

## Raw output

```text
entropyfs evidence campaign — 1787681660
revision: 9be6bd3
  structured [full] 3 runs: write 1278.5 MiB/s (p50 45233µs, p95 62577µs, p99 62577µs) read 2981.6 MiB/s fsync p50 1646µs p95 2374µs p99 2374µs physical median 50528 ratio 1328.152x
  src [full] 3 runs: write 33.4 MiB/s (p50 76815µs, p95 76890µs, p99 76890µs) read 160.5 MiB/s fsync p50 1644µs p95 2904µs p99 2904µs physical median 646427 ratio 4.173x
  urandom [full] 3 runs: write 32.6 MiB/s (p50 979913µs, p95 991724µs, p99 991724µs) read 4059.2 MiB/s fsync p50 1815µs p95 19777µs p99 19777µs physical median 33652515 ratio 0.997x
  compressed-z19 [full] 3 runs: write 36.3 MiB/s (p50 9863µs, p95 9916µs, p99 9916µs) read 3900.5 MiB/s fsync p50 1646µs p95 2832µs p99 2832µs physical median 378638 ratio 0.995x

== leave-one-out ablation (structured) ==
  full       physical        50528 ratio 1328.152x write   1281.4 MiB/s cpu 0.050+0.000s (p95 write 45262µs)
  raw        physical       319070 ratio 210.326x write   1446.2 MiB/s cpu 0.040+0.000s (p95 write 38538µs)
  raw-byte-rans physical       267181 ratio 251.174x write    659.0 MiB/s cpu 0.100+0.010s (p95 write 92383µs)
  no-exact-ref physical        67868 ratio 988.815x write   1235.8 MiB/s cpu 0.050+0.000s (p95 write 46950µs)
  no-base    physical        50528 ratio 1328.152x write   1277.4 MiB/s cpu 0.050+0.000s (p95 write 44997µs)
  no-temporal physical        50528 ratio 1328.152x write   1284.0 MiB/s cpu 0.050+0.000s (p95 write 45009µs)
  no-config  physical        64976 ratio 1032.825x write    786.6 MiB/s cpu 0.080+0.000s (p95 write 76408µs)
  no-rans    physical       112301 ratio 597.580x write   1395.4 MiB/s cpu 0.040+0.000s (p95 write 41311µs)
  no-byte-rans physical        50528 ratio 1328.152x write   1258.4 MiB/s cpu 0.050+0.010s (p95 write 45801µs)
  no-sequence-rans physical       112301 ratio 597.580x write   1346.3 MiB/s cpu 0.050+0.000s (p95 write 42253µs)
  no-deep    physical        50528 ratio 1328.152x write   1228.9 MiB/s cpu 0.050+0.000s (p95 write 46430µs)
  no-sequence-dict physical        50528 ratio 1328.152x write   1257.1 MiB/s cpu 0.040+0.000s (p95 write 46042µs)
  no-shared-dict physical        50528 ratio 1328.152x write   1276.5 MiB/s cpu 0.050+0.000s (p95 write 44973µs)
  no-universe physical        50528 ratio 1328.152x write   1219.1 MiB/s cpu 0.060+0.000s (p95 write 46641µs)
  no-dsfb    physical        50528 ratio 1328.152x write   1263.1 MiB/s cpu 0.050+0.010s (p95 write 45904µs)

== cumulative ladder A0-A8 (structured) ==
  A0-raw             physical       319070 ratio 210.326x write   1456.2 MiB/s cpu 0.040+0.000s (p95 write 38715µs)
  A1-byte-rans       physical       267181 ratio 251.174x write    655.4 MiB/s cpu 0.100+0.000s (p95 write 93066µs)
  A2-exact-ref       physical       251881 ratio 266.431x write    650.9 MiB/s cpu 0.090+0.000s (p95 write 93506µs)
  A3-base-residual   physical       251881 ratio 266.431x write    656.2 MiB/s cpu 0.100+0.000s (p95 write 92179µs)
  A4-config          physical       112301 ratio 597.580x write   1376.5 MiB/s cpu 0.040+0.000s (p95 write 41415µs)
  A5-temporal-bases  physical       112301 ratio 597.580x write   1392.9 MiB/s cpu 0.050+0.000s (p95 write 41021µs)
  A6-universe        physical       112301 ratio 597.580x write   1382.6 MiB/s cpu 0.040+0.000s (p95 write 41434µs)
  A7-dsfb            physical       112301 ratio 597.580x write   1402.0 MiB/s cpu 0.040+0.000s (p95 write 40745µs)
  A8-background      physical       112301 ratio 597.580x write   1402.5 MiB/s cpu 0.050+0.000s (p95 write 40842µs)
  E1-sequence-rans   physical        50528 ratio 1328.152x write   1282.7 MiB/s cpu 0.050+0.000s (p95 write 45081µs)
  E2-sequence-dict   physical        50528 ratio 1328.152x write   1267.8 MiB/s cpu 0.050+0.010s (p95 write 45674µs)
  E3-shared-dict     physical        50528 ratio 1328.152x write   1281.2 MiB/s cpu 0.050+0.000s (p95 write 44880µs)
  E4-deep            physical        50238 ratio 1335.819x write   1292.6 MiB/s cpu 0.040+0.000s (p95 write 44924µs)

== DSFB search-budget investigation (structured) ==
  full      write median  1282.1 MiB/s (min 1254.4, max 1290.3) cpu median 0.050s physical [50528, 50528, 50528]
  no-dsfb   write median  1276.3 MiB/s (min 1246.4, max 1282.5) cpu median 0.050s physical [50528, 50528, 50528]
  physical identical across modes: true

== versioned experiment (H2) ==
  versioned [full] 3 runs: write 81.8 MiB/s (p50 33958µs, p95 92261µs, p99 95275µs) read 532.5 MiB/s fsync p50 1658µs p95 4453µs p99 4453µs physical median 1392236 ratio 3.013x
  versioned [no-base] 3 runs: write 74.1 MiB/s (p50 57521µs, p95 61256µs, p99 62058µs) read 1532.3 MiB/s fsync p50 1656µs p95 5806µs p99 5806µs physical median 1214754 ratio 3.453x
  shuffled [full] 3 runs: write 62.8 MiB/s (p50 66127µs, p95 81634µs, p99 83642µs) read 868.8 MiB/s fsync p50 1658µs p95 5824µs p99 5824µs physical median 2345529 ratio 1.788x
  sequential median reachable: 1392236 bytes (3.013x)
  shuffled    median reachable: 2345529 bytes (1.788x)
  base+residual savings vs shuffled: 953293 bytes (40.6% of shuffled reachable)
  post-GC reachable: sequential full 1265786 (3.314x) / no-base 1165681 (3.598x) / shuffled 2287928 (1.833x)

== GC and optimizer traffic ==
  unreachable before 5537444 → reclaimed 5537444 → after 2274864; physical 39350926 → 35927915; gc 0.014s; optimizer scanned 512 rewrote 0 saved 0
  unreachable by record tag (post-GC): {"BtreeNode": 2274626, "Root": 238}

== post-GC physical footprint ==
  src: logical 2697750 → reachable 646427 (4.17x) / total backing 651982 (4.14x) / allocated 659456 (4.09x)
  structured: logical 67108864 → reachable 50528 (1328.15x) / total backing 56083 (1196.60x) / allocated 61440 (1092.27x)
  urandom: logical 33554432 → reachable 33652515 (1.00x) / total backing 33658070 (1.00x) / allocated 33665024 (1.00x)
  compressed-z19: logical 376609 → reachable 378638 (0.99x) / total backing 384193 (0.98x) / allocated 389120 (0.97x)

== Phase-9C tree court ==
  files 275 (single-chunk 272), logical 2688567 B
  zstd -1 whole              554694 B  (4.871x)
  zstd -19 whole             376572 B  (7.175x)
  zstd -1 per-file           761333 B  (3.531x)
  zstd -19 per-file          667878 B  (4.026x)
  zstd -1 per-64KiB          676959 B  (3.991x)
  zstd -19 per-64KiB         560986 B  (4.817x)
  efs tree (post-GC):            1225501 B reachable (2.194x) / 2768421 B backing
  efs tree + shared dict:        1142101 B reachable (2.354x) / 3171017 B backing (rewrote 151 extents, saved 93327 B)
  families before: {"RANS": 107, "RAW": 19, "SEQUENCE_DICT": 3, "SEQUENCE_RANS": 149}
  families after:  {"RANS": 92, "RAW": 19, "SEQUENCE_DEEP": 15, "SEQUENCE_DICT": 3, "SEQUENCE_RANS": 13, "SEQUENCE_SHARED_DICT": 136}

== baselines ==
  waived: btrfs with compression: waived — writable compressed-FS baseline requires root for loop-mounting a test image
  waived: EROFS/SquashFS: waived — read-only compressed-image baseline deferred (requires root and/or mkfs.erofs)
  raw file (ext4): 8251.9 MiB/s write, ratio 1.000x
  zstd -1: 2697750 → 554372 bytes (4.866x), 0.005s
  zstd -19: 2697750 → 376609 bytes (7.163x), 0.391s
  zstd -1 per 64KiB: 2697750 → 676404 bytes (3.988x), 0.028s
  zstd -19 per 64KiB: 2697750 → 560665 bytes (4.812x), 0.535s
  direct byte rANS (same backend, src corpus): 2697750 → 1648246 bytes (1.637x)
  standalone SequenceRans (src corpus): 2697750 → 722168 bytes (3.736x)
  standalone SequenceDeep (src corpus): 2697750 → 712501 bytes (3.786x)
device nvme1n1p1: 610400 sectors written (312524800 bytes), 16 sectors read (8192 bytes)

== admission checklist (methodology §8) ==
  [OK ] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
  [OK ] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
  [OK ] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
  [OK ] ablations identify which mechanism caused the gain (cumulative ladder A0-A8 + leave-one-out) — leave-one-out table 15 rows; cumulative ladder 13 rows (A0-A8)
  [OK ] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
  [OK ] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
  [OK ] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md
```
