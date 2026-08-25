# EntropyFS evidence campaign

- campaign dir: `evidence/performance/campaign-1787685723-60ecaf2`
- created: unix 1787685723

## Admission checklist (methodology §8)

- [x] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
- [x] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
- [x] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
- [x] ablations identify which mechanism caused the gain (cumulative ladder A0-A8 + leave-one-out) — leave-one-out table 15 rows; cumulative ladder 13 rows (A0-A8)
- [x] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
- [x] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
- [x] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md

## Summary

- `structured[full]`: 3 runs — write 1263.0 MiB/s (p50 46096µs, p95 61632µs, p99 61632µs), read 2999.0 MiB/s, fsync p50 1657µs, physical median 50528 bytes, ratio 1328.152x
- `src[full]`: 3 runs — write 35.6 MiB/s (p50 82431µs, p95 83014µs, p99 83014µs), read 189.0 MiB/s, fsync p50 1642µs, physical median 705488 bytes, ratio 4.366x
- `urandom[full]`: 3 runs — write 33.0 MiB/s (p50 966547µs, p95 968527µs, p99 968527µs), read 3974.4 MiB/s, fsync p50 1637µs, physical median 33652515 bytes, ratio 0.997x
- `compressed-z19[full]`: 3 runs — write 36.7 MiB/s (p50 10475µs, p95 10534µs, p99 10534µs), read 4042.8 MiB/s, fsync p50 1641µs, physical median 405985 bytes, ratio 0.995x
- `versioned[full]`: 3 runs — write 82.9 MiB/s (p50 33015µs, p95 91344µs, p99 91502µs), read 567.3 MiB/s, fsync p50 1656µs, physical median 1359786 bytes, ratio 3.085x
- `versioned[no-base]`: 3 runs — write 76.3 MiB/s (p50 56054µs, p95 59282µs, p99 60393µs), read 1818.4 MiB/s, fsync p50 1644µs, physical median 1180500 bytes, ratio 3.553x
- `shuffled[full]`: 3 runs — write 67.0 MiB/s (p50 58450µs, p95 78944µs, p99 78950µs), read 816.0 MiB/s, fsync p50 1635µs, physical median 2346969 bytes, ratio 1.787x

Device writes during campaign window (nvme1n1p1): 301215744 bytes written, 20480 bytes read.

## Raw output

```text
entropyfs evidence campaign — 1787685723
revision: 60ecaf2
  structured [full] 3 runs: write 1263.0 MiB/s (p50 46096µs, p95 61632µs, p99 61632µs) read 2999.0 MiB/s fsync p50 1657µs p95 3899µs p99 3899µs physical median 50528 ratio 1328.152x
  src [full] 3 runs: write 35.6 MiB/s (p50 82431µs, p95 83014µs, p99 83014µs) read 189.0 MiB/s fsync p50 1642µs p95 2756µs p99 2756µs physical median 705488 ratio 4.366x
  urandom [full] 3 runs: write 33.0 MiB/s (p50 966547µs, p95 968527µs, p99 968527µs) read 3974.4 MiB/s fsync p50 1637µs p95 12134µs p99 12134µs physical median 33652515 ratio 0.997x
  compressed-z19 [full] 3 runs: write 36.7 MiB/s (p50 10475µs, p95 10534µs, p99 10534µs) read 4042.8 MiB/s fsync p50 1641µs p95 2506µs p99 2506µs physical median 405985 ratio 0.995x

== leave-one-out ablation (structured) ==
  full       physical        50528 ratio 1328.152x write   1246.4 MiB/s cpu 0.050+0.000s (p95 write 45875µs)
  raw        physical       319070 ratio 210.326x write   1478.2 MiB/s cpu 0.040+0.010s (p95 write 38420µs)
  raw-byte-rans physical       267181 ratio 251.174x write    660.5 MiB/s cpu 0.090+0.000s (p95 write 92332µs)
  no-exact-ref physical        67868 ratio 988.815x write   1249.4 MiB/s cpu 0.050+0.000s (p95 write 46566µs)
  no-base    physical        50528 ratio 1328.152x write   1255.3 MiB/s cpu 0.050+0.000s (p95 write 45738µs)
  no-temporal physical        50528 ratio 1328.152x write   1260.2 MiB/s cpu 0.050+0.000s (p95 write 45967µs)
  no-config  physical        64939 ratio 1033.414x write    790.8 MiB/s cpu 0.080+0.010s (p95 write 75880µs)
  no-rans    physical       112301 ratio 597.580x write   1375.9 MiB/s cpu 0.050+0.000s (p95 write 41880µs)
  no-byte-rans physical        50528 ratio 1328.152x write   1275.5 MiB/s cpu 0.040+0.000s (p95 write 45226µs)
  no-sequence-rans physical       112301 ratio 597.580x write   1357.2 MiB/s cpu 0.050+0.000s (p95 write 42035µs)
  no-deep    physical        50528 ratio 1328.152x write   1276.3 MiB/s cpu 0.050+0.000s (p95 write 45462µs)
  no-sequence-dict physical        50528 ratio 1328.152x write   1277.8 MiB/s cpu 0.050+0.010s (p95 write 45121µs)
  no-shared-dict physical        50528 ratio 1328.152x write   1270.4 MiB/s cpu 0.050+0.000s (p95 write 45697µs)
  no-universe physical        50528 ratio 1328.152x write   1268.1 MiB/s cpu 0.050+0.000s (p95 write 45043µs)
  no-dsfb    physical        50528 ratio 1328.152x write   1242.3 MiB/s cpu 0.050+0.000s (p95 write 46433µs)

== cumulative ladder A0-A8 (structured) ==
  A0-raw             physical       319070 ratio 210.326x write   1496.6 MiB/s cpu 0.040+0.000s (p95 write 37738µs)
  A1-byte-rans       physical       267181 ratio 251.174x write    655.8 MiB/s cpu 0.100+0.000s (p95 write 92664µs)
  A2-exact-ref       physical       251881 ratio 266.431x write    651.8 MiB/s cpu 0.090+0.000s (p95 write 92409µs)
  A3-base-residual   physical       251881 ratio 266.431x write    658.4 MiB/s cpu 0.090+0.000s (p95 write 92466µs)
  A4-config          physical       112301 ratio 597.580x write   1384.6 MiB/s cpu 0.050+0.000s (p95 write 41212µs)
  A5-temporal-bases  physical       112301 ratio 597.580x write   1383.0 MiB/s cpu 0.040+0.000s (p95 write 41487µs)
  A6-universe        physical       112301 ratio 597.580x write   1401.6 MiB/s cpu 0.050+0.000s (p95 write 41004µs)
  A7-dsfb            physical       112301 ratio 597.580x write   1397.5 MiB/s cpu 0.040+0.000s (p95 write 40843µs)
  A8-background      physical       112301 ratio 597.580x write   1393.9 MiB/s cpu 0.040+0.000s (p95 write 41230µs)
  E1-sequence-rans   physical        50528 ratio 1328.152x write   1273.4 MiB/s cpu 0.050+0.000s (p95 write 45211µs)
  E2-sequence-dict   physical        50528 ratio 1328.152x write   1276.9 MiB/s cpu 0.050+0.000s (p95 write 45229µs)
  E3-shared-dict     physical        50528 ratio 1328.152x write   1287.0 MiB/s cpu 0.050+0.000s (p95 write 44590µs)
  E4-deep            physical        50238 ratio 1335.819x write   1280.8 MiB/s cpu 0.050+0.000s (p95 write 45171µs)

== DSFB search-budget investigation (structured) ==
  full      write median  1282.2 MiB/s (min 1276.0, max 1300.2) cpu median 0.050s physical [50528, 50528, 50528]
  no-dsfb   write median  1275.9 MiB/s (min 1273.5, max 1285.1) cpu median 0.050s physical [50528, 50528, 50528]
  physical identical across modes: true

== versioned experiment (H2) ==
  versioned [full] 3 runs: write 82.9 MiB/s (p50 33015µs, p95 91344µs, p99 91502µs) read 567.3 MiB/s fsync p50 1656µs p95 5474µs p99 5474µs physical median 1359786 ratio 3.085x
  versioned [no-base] 3 runs: write 76.3 MiB/s (p50 56054µs, p95 59282µs, p99 60393µs) read 1818.4 MiB/s fsync p50 1644µs p95 5560µs p99 5560µs physical median 1180500 ratio 3.553x
  shuffled [full] 3 runs: write 67.0 MiB/s (p50 58450µs, p95 78944µs, p99 78950µs) read 816.0 MiB/s fsync p50 1635µs p95 9033µs p99 9033µs physical median 2346969 ratio 1.787x
  sequential median reachable: 1359786 bytes (3.085x)
  shuffled    median reachable: 2346969 bytes (1.787x)
  base+residual savings vs shuffled: 987183 bytes (42.1% of shuffled reachable)
  post-GC reachable: sequential full 1233336 (3.401x) / no-base 1131427 (3.707x) / shuffled 2289960 (1.832x)

== GC and optimizer traffic ==
  unreachable before 5455393 → reclaimed 5455393 → after 2274864; physical 39268875 → 35927915; gc 0.014s; optimizer scanned 512 rewrote 0 saved 0
  unreachable by record tag (post-GC): {"BtreeNode": 2274626, "Root": 238}

== post-GC physical footprint ==
  src: logical 3079911 → reachable 705488 (4.37x) / total backing 711043 (4.33x) / allocated 716800 (4.30x)
  structured: logical 67108864 → reachable 50528 (1328.15x) / total backing 56083 (1196.60x) / allocated 61440 (1092.27x)
  urandom: logical 33554432 → reachable 33652515 (1.00x) / total backing 33658070 (1.00x) / allocated 33665024 (1.00x)
  compressed-z19: logical 403772 → reachable 405985 (0.99x) / total backing 411540 (0.98x) / allocated 417792 (0.97x)

== Phase-9C tree court ==
  files 298 (single-chunk 295), logical 3069073 B
  zstd -1 whole              598715 B  (5.151x)
  zstd -19 whole             403966 B  (7.635x)
  zstd -1 per-file           848302 B  (3.618x)
  zstd -19 per-file          742611 B  (4.133x)
  zstd -1 per-64KiB          754659 B  (4.087x)
  zstd -19 per-64KiB         625647 B  (4.930x)
  efs tree (post-GC):            1239597 B reachable (2.476x) / 3027088 B backing
  efs tree + shared dict:        1090858 B reachable (2.813x) / 3682453 B backing (rewrote 258 extents, saved 168383 B)
  efs + model bundles:          1065145 B reachable (2.881x) / 3656740 B backing (rewrote 65 extents, saved 7486 B)  [Phase-9G]
  families before: {"RANS": 35, "RAW": 11, "SEQUENCE_DICT": 3, "SEQUENCE_RANS": 252}
  families after:  {"RANS": 20, "RAW": 7, "SEQUENCE_DEEP": 33, "SEQUENCE_DICT": 3, "SEQUENCE_RANS": 13, "SEQUENCE_SHARED_DICT": 225}
  families +models: {"RANS": 20, "RAW": 7, "SEQUENCE_DEEP": 33, "SEQUENCE_DICT": 3, "SEQUENCE_RANS": 13, "SEQUENCE_SHARED_DICT": 225}
  zstd -1 per-file +dir anchor:     773676 B  (3.967x)  [Phase-9F anchor-policy control]
  per-extent overhead: 43965 B descriptors + 74886 B models = 118851 B (11.2% of footprint, 3.9% of logical)  [Phase-9F]

== baselines ==
  waived: btrfs with compression: waived — writable compressed-FS baseline requires root for loop-mounting a test image
  waived: EROFS/SquashFS: waived — read-only compressed-image baseline deferred (requires root and/or mkfs.erofs)
  raw file (ext4): 2534.1 MiB/s write, ratio 1.000x
  zstd -1: 3079911 → 598744 bytes (5.144x), 0.004s
  zstd -19: 3079911 → 403772 bytes (7.628x), 0.415s
  zstd -1 per 64KiB: 3079911 → 751985 bytes (4.096x), 0.031s
  zstd -19 per 64KiB: 3079911 → 623320 bytes (4.941x), 0.602s
  direct byte rANS (same backend, src corpus): 3079911 → 1879832 bytes (1.638x)
  standalone SequenceRans (src corpus): 3079911 → 805639 bytes (3.823x)
  standalone SequenceDeep (src corpus): 3079911 → 776653 bytes (3.966x)
device nvme1n1p1: 588312 sectors written (301215744 bytes), 40 sectors read (20480 bytes)

== admission checklist (methodology §8) ==
  [OK ] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
  [OK ] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
  [OK ] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
  [OK ] ablations identify which mechanism caused the gain (cumulative ladder A0-A8 + leave-one-out) — leave-one-out table 15 rows; cumulative ladder 13 rows (A0-A8)
  [OK ] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
  [OK ] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
  [OK ] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md
```
