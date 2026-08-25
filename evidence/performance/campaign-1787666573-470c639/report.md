# EntropyFS evidence campaign

- campaign dir: `evidence/performance/campaign-1787666573-470c639`
- created: unix 1787666573

## Admission checklist (methodology §8)

- [x] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
- [x] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
- [x] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
- [x] ablations identify which mechanism caused the gain — ablation ladder has 9 rows (A0–A8 families)
- [x] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
- [x] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
- [x] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md

## Summary

- `structured[full]`: 5 runs — write 360.3 MiB/s (p50 43196µs, p95 47422µs, p99 47422µs), read 3203.6 MiB/s, fsync p50 1648µs, physical median 19844 bytes, ratio 845.455x
- `src[full]`: 3 runs — write 57.4 MiB/s (p50 27959µs, p95 28182µs, p99 28182µs), read 393.6 MiB/s, fsync p50 1651µs, physical median 489355 bytes, ratio 3.452x
- `urandom[full]`: 3 runs — write 94.9 MiB/s (p50 83816µs, p95 84455µs, p99 84455µs), read 3891.6 MiB/s, fsync p50 1651µs, physical median 8413743 bytes, ratio 0.997x
- `compressed-z19[full]`: 3 runs — write 95.4 MiB/s (p50 2840µs, p95 2853µs, p99 2853µs), read 3473.3 MiB/s, fsync p50 1641µs, physical median 286929 bytes, ratio 0.994x
- `versioned[full]`: 3 runs — write 81.1 MiB/s (p50 35983µs, p95 91890µs, p99 93960µs), read 1156.3 MiB/s, fsync p50 1637µs, physical median 1528175 bytes, ratio 2.745x
- `versioned[no-base]`: 3 runs — write 116.2 MiB/s (p50 36810µs, p95 38856µs, p99 39395µs), read 1502.4 MiB/s, fsync p50 1644µs, physical median 1214754 bytes, ratio 3.453x
- `shuffled[full]`: 3 runs — write 78.7 MiB/s (p50 51015µs, p95 66858µs, p99 67267µs), read 849.5 MiB/s, fsync p50 1663µs, physical median 2351510 bytes, ratio 1.784x

Device writes during campaign window (nvme1n1p1): 346443776 bytes written, 8192 bytes read.

## Raw output

```text
entropyfs evidence campaign — 1787666573
revision: 470c639
  structured [full] 5 runs: write 360.3 MiB/s (p50 43196µs, p95 47422µs, p99 47422µs) read 3203.6 MiB/s fsync p50 1648µs p95 2768µs p99 2855µs physical median 19844 ratio 845.455x
  src [full] 3 runs: write 57.4 MiB/s (p50 27959µs, p95 28182µs, p99 28182µs) read 393.6 MiB/s fsync p50 1651µs p95 2549µs p99 2549µs physical median 489355 ratio 3.452x
  urandom [full] 3 runs: write 94.9 MiB/s (p50 83816µs, p95 84455µs, p99 84455µs) read 3891.6 MiB/s fsync p50 1651µs p95 5446µs p99 5446µs physical median 8413743 ratio 0.997x
  compressed-z19 [full] 3 runs: write 95.4 MiB/s (p50 2840µs, p95 2853µs, p99 2853µs) read 3473.3 MiB/s fsync p50 1641µs p95 2375µs p99 2375µs physical median 286929 ratio 0.994x

== ablation ladder (structured) ==
  full       physical        19844 ratio 845.455x write    363.0 MiB/s cpu 0.040+0.000s (p95 write 42753µs)
  raw        physical       277382 ratio  60.484x write    537.7 MiB/s cpu 0.030+0.000s (p95 write 28555µs)
  raw-rans   physical        32816 ratio 511.251x write    262.3 MiB/s cpu 0.060+0.000s (p95 write 59653µs)
  no-dedup   physical        19844 ratio 845.455x write    364.2 MiB/s cpu 0.040+0.000s (p95 write 42636µs)
  no-base    physical        19844 ratio 845.455x write    367.3 MiB/s cpu 0.050+0.000s (p95 write 42476µs)
  no-config  physical        32816 ratio 511.251x write    261.4 MiB/s cpu 0.050+0.010s (p95 write 60026µs)
  no-rans    physical        79235 ratio 211.740x write    421.0 MiB/s cpu 0.040+0.000s (p95 write 36846µs)
  no-universe physical        19844 ratio 845.455x write    359.3 MiB/s cpu 0.040+0.000s (p95 write 43020µs)
  no-dsfb    physical        19844 ratio 845.455x write    350.4 MiB/s cpu 0.050+0.000s (p95 write 44445µs)

== DSFB search-budget investigation (structured) ==
  full      write median   362.3 MiB/s (min 361.6, max 366.4) cpu median 0.040s physical [19844, 19844, 19844, 19844, 19844]
  no-dsfb   write median   351.9 MiB/s (min 350.4, max 353.6) cpu median 0.040s physical [19844, 19844, 19844, 19844, 19844]
  physical identical across modes: true

== versioned experiment (H2) ==
  versioned [full] 3 runs: write 81.1 MiB/s (p50 35983µs, p95 91890µs, p99 93960µs) read 1156.3 MiB/s fsync p50 1637µs p95 15473µs p99 15473µs physical median 1528175 ratio 2.745x
  versioned [no-base] 3 runs: write 116.2 MiB/s (p50 36810µs, p95 38856µs, p99 39395µs) read 1502.4 MiB/s fsync p50 1644µs p95 11990µs p99 11990µs physical median 1214754 ratio 3.453x
  shuffled [full] 3 runs: write 78.7 MiB/s (p50 51015µs, p95 66858µs, p99 67267µs) read 849.5 MiB/s fsync p50 1663µs p95 7993µs p99 7993µs physical median 2351510 ratio 1.784x
  sequential median reachable: 1528175 bytes (2.745x)
  shuffled    median reachable: 2351510 bytes (1.784x)
  base+residual savings vs shuffled: 823335 bytes (35.0% of shuffled reachable)

== GC and optimizer traffic ==
  unreachable before 50963598 → reclaimed 47974904 → after 2988932; physical 59712501 → 11678436; gc 0.009s; optimizer scanned 128 rewrote 0 saved 0

== baselines ==
  waived: btrfs with compression: waived — writable compressed-FS baseline requires root for loop-mounting a test image
  waived: EROFS/SquashFS: waived — read-only compressed-image baseline deferred (requires root and/or mkfs.erofs)
  raw file (ext4): 4776.9 MiB/s write, ratio 1.000x
  zstd -1: 1689051 → 412439 bytes (4.095x), 0.005s
  zstd -19: 1689051 → 285084 bytes (5.925x), 0.243s
  direct rANS (same backend, src corpus): 1689051 → 489355 bytes (3.452x)
device nvme1n1p1: 676648 sectors written (346443776 bytes), 16 sectors read (8192 bytes)

== admission checklist (methodology §8) ==
  [OK ] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
  [OK ] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
  [OK ] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
  [OK ] ablations identify which mechanism caused the gain — ablation ladder has 9 rows (A0–A8 families)
  [OK ] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
  [OK ] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
  [OK ] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md
```
