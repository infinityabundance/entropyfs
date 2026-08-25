# EntropyFS evidence campaign

- campaign dir: `evidence/performance/campaign-1787668313-0a7d800`
- created: unix 1787668313

## Admission checklist (methodology §8)

- [x] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
- [x] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
- [x] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
- [x] ablations identify which mechanism caused the gain (cumulative ladder A0-A8 + leave-one-out) — leave-one-out table 10 rows; cumulative ladder 9 rows (A0-A8)
- [x] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
- [x] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
- [x] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md

## Summary

- `structured[full]`: 5 runs — write 347.6 MiB/s (p50 179761µs, p95 196803µs, p99 196803µs), read 3098.5 MiB/s, fsync p50 1651µs, physical median 67868 bytes, ratio 988.815x
- `src[full]`: 3 runs — write 56.8 MiB/s (p50 28804µs, p95 28994µs, p99 28994µs), read 391.8 MiB/s, fsync p50 1645µs, physical median 497751 bytes, ratio 3.455x
- `urandom[full]`: 3 runs — write 94.6 MiB/s (p50 336169µs, p95 338041µs, p99 338041µs), read 3966.1 MiB/s, fsync p50 1664µs, physical median 33652515 bytes, ratio 0.997x
- `compressed-z19[full]`: 3 runs — write 97.8 MiB/s (p50 2815µs, p95 2896µs, p99 2896µs), read 3525.1 MiB/s, fsync p50 1667µs, physical median 292143 bytes, ratio 0.994x
- `versioned[full]`: 3 runs — write 80.7 MiB/s (p50 36046µs, p95 92448µs, p99 92453µs), read 1181.8 MiB/s, fsync p50 1659µs, physical median 1528175 bytes, ratio 2.745x
- `versioned[no-base]`: 3 runs — write 116.9 MiB/s (p50 36321µs, p95 39593µs, p99 39795µs), read 1521.7 MiB/s, fsync p50 1660µs, physical median 1214754 bytes, ratio 3.453x
- `shuffled[full]`: 3 runs — write 79.7 MiB/s (p50 50143µs, p95 66514µs, p99 66525µs), read 857.6 MiB/s, fsync p50 1654µs, physical median 2351510 bytes, ratio 1.784x

Device writes during campaign window (nvme1n1p1): 543047680 bytes written, 69632 bytes read.

## Raw output

```text
entropyfs evidence campaign — 1787668313
revision: 0a7d800
  structured [full] 5 runs: write 347.6 MiB/s (p50 179761µs, p95 196803µs, p99 196803µs) read 3098.5 MiB/s fsync p50 1651µs p95 4190µs p99 4404µs physical median 67868 ratio 988.815x
  src [full] 3 runs: write 56.8 MiB/s (p50 28804µs, p95 28994µs, p99 28994µs) read 391.8 MiB/s fsync p50 1645µs p95 3684µs p99 3684µs physical median 497751 ratio 3.455x
  urandom [full] 3 runs: write 94.6 MiB/s (p50 336169µs, p95 338041µs, p99 338041µs) read 3966.1 MiB/s fsync p50 1664µs p95 20771µs p99 20771µs physical median 33652515 ratio 0.997x
  compressed-z19 [full] 3 runs: write 97.8 MiB/s (p50 2815µs, p95 2896µs, p99 2896µs) read 3525.1 MiB/s fsync p50 1667µs p95 3001µs p99 3001µs physical median 292143 ratio 0.994x

== leave-one-out ablation (structured) ==
  full       physical        67868 ratio 988.815x write    351.5 MiB/s cpu 0.180+0.010s (p95 write 177142µs)
  raw        physical       319070 ratio 210.326x write    496.2 MiB/s cpu 0.110+0.020s (p95 write 124276µs)
  raw-rans   physical       115976 ratio 578.644x write    257.0 MiB/s cpu 0.240+0.000s (p95 write 243750µs)
  no-dedup   physical        67868 ratio 988.815x write    340.9 MiB/s cpu 0.180+0.010s (p95 write 182956µs)
  no-base    physical        67868 ratio 988.815x write    348.1 MiB/s cpu 0.180+0.000s (p95 write 178764µs)
  no-temporal physical        67868 ratio 988.815x write    349.6 MiB/s cpu 0.180+0.000s (p95 write 177443µs)
  no-config  physical       115976 ratio 578.644x write    260.4 MiB/s cpu 0.240+0.010s (p95 write 240703µs)
  no-rans    physical       116891 ratio 574.115x write    420.9 MiB/s cpu 0.150+0.000s (p95 write 147459µs)
  no-universe physical        67868 ratio 988.815x write    350.3 MiB/s cpu 0.180+0.000s (p95 write 177377µs)
  no-dsfb    physical        67868 ratio 988.815x write    340.6 MiB/s cpu 0.190+0.010s (p95 write 183213µs)

== cumulative ladder A0-A8 (structured) ==
  A0-raw             physical       319070 ratio 210.326x write    528.4 MiB/s cpu 0.100+0.020s (p95 write 115919µs)
  A1-rans            physical       115976 ratio 578.644x write    261.0 MiB/s cpu 0.240+0.010s (p95 write 239318µs)
  A2-dedup           physical       115976 ratio 578.644x write    261.0 MiB/s cpu 0.240+0.000s (p95 write 240067µs)
  A3-base-residual   physical       115976 ratio 578.644x write    261.2 MiB/s cpu 0.240+0.000s (p95 write 240157µs)
  A4-config          physical        67868 ratio 988.815x write    354.6 MiB/s cpu 0.180+0.000s (p95 write 175751µs)
  A5-temporal-bases  physical        67868 ratio 988.815x write    334.9 MiB/s cpu 0.180+0.010s (p95 write 185704µs)
  A6-universe        physical        67868 ratio 988.815x write    340.7 MiB/s cpu 0.190+0.000s (p95 write 182353µs)
  A7-dsfb            physical        67868 ratio 988.815x write    351.9 MiB/s cpu 0.170+0.010s (p95 write 176979µs)
  A8-full+background physical        67868 ratio 988.815x write    352.0 MiB/s cpu 0.170+0.000s (p95 write 176473µs)

== DSFB search-budget investigation (structured) ==
  full      write median   345.5 MiB/s (min 342.5, max 351.9) cpu median 0.180s physical [67868, 67868, 67868, 67868, 67868]
  no-dsfb   write median   339.4 MiB/s (min 334.0, max 339.9) cpu median 0.180s physical [67868, 67868, 67868, 67868, 67868]
  physical identical across modes: true

== versioned experiment (H2) ==
  versioned [full] 3 runs: write 80.7 MiB/s (p50 36046µs, p95 92448µs, p99 92453µs) read 1181.8 MiB/s fsync p50 1659µs p95 19155µs p99 19155µs physical median 1528175 ratio 2.745x
  versioned [no-base] 3 runs: write 116.9 MiB/s (p50 36321µs, p95 39593µs, p99 39795µs) read 1521.7 MiB/s fsync p50 1660µs p95 7056µs p99 7056µs physical median 1214754 ratio 3.453x
  shuffled [full] 3 runs: write 79.7 MiB/s (p50 50143µs, p95 66514µs, p99 66525µs) read 857.6 MiB/s fsync p50 1654µs p95 8351µs p99 8351µs physical median 2351510 ratio 1.784x
  sequential median reachable: 1528175 bytes (2.745x)
  shuffled    median reachable: 2351510 bytes (1.784x)
  base+residual savings vs shuffled: 823335 bytes (35.0% of shuffled reachable)

== GC and optimizer traffic ==
  unreachable before 59538672 → reclaimed 47974904 → after 13953950; physical 93526879 → 47607009; gc 0.015s; optimizer scanned 512 rewrote 0 saved 0

== baselines ==
  waived: btrfs with compression: waived — writable compressed-FS baseline requires root for loop-mounting a test image
  waived: EROFS/SquashFS: waived — read-only compressed-image baseline deferred (requires root and/or mkfs.erofs)
  raw file (ext4): 6245.7 MiB/s write, ratio 1.000x
  zstd -1: 1719719 → 420482 bytes (4.090x), 0.004s
  zstd -19: 1719719 → 290298 bytes (5.924x), 0.244s
  direct rANS (same backend, src corpus): 1719719 → 497751 bytes (3.455x)
device nvme1n1p1: 1060640 sectors written (543047680 bytes), 136 sectors read (69632 bytes)

== admission checklist (methodology §8) ==
  [OK ] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
  [OK ] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
  [OK ] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
  [OK ] ablations identify which mechanism caused the gain (cumulative ladder A0-A8 + leave-one-out) — leave-one-out table 10 rows; cumulative ladder 9 rows (A0-A8)
  [OK ] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
  [OK ] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
  [OK ] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md
```
