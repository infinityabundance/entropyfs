# EntropyFS evidence campaign

- campaign dir: `evidence/performance/campaign-1787666036-43bf17e`
- created: unix 1787666036

## Admission checklist (methodology §8)

- [x] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
- [x] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
- [x] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
- [x] ablations identify which mechanism caused the gain — ablation ladder has 9 rows (A0–A8 families)
- [x] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
- [x] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
- [x] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md

## Summary

- `structured[full]`: 5 runs — write 390.2 MiB/s (p50 40112µs, p95 44059µs, p99 44059µs), read 3168.3 MiB/s, fsync p50 1655µs, physical median 19844 bytes, ratio 845.455x
- `src[full]`: 3 runs — write 57.2 MiB/s (p50 26316µs, p95 26969µs, p99 26969µs), read 392.0 MiB/s, fsync p50 1658µs, physical median 467025 bytes, ratio 3.388x
- `urandom[full]`: 3 runs — write 97.5 MiB/s (p50 81516µs, p95 82015µs, p99 82015µs), read 4089.8 MiB/s, fsync p50 1651µs, physical median 8413743 bytes, ratio 0.997x
- `compressed-z19[full]`: 3 runs — write 98.7 MiB/s (p50 2657µs, p95 2672µs, p99 2672µs), read 3743.4 MiB/s, fsync p50 1642µs, physical median 278504 bytes, ratio 0.993x
- `versioned[full]`: 3 runs — write 78.8 MiB/s (p50 38314µs, p95 94925µs, p99 95375µs), read 876.4 MiB/s, fsync p50 1652µs, physical median 1524135 bytes, ratio 2.752x
- `versioned[no-base]`: 3 runs — write 98.6 MiB/s (p50 43725µs, p95 46289µs, p99 46411µs), read 1121.5 MiB/s, fsync p50 1766µs, physical median 1208262 bytes, ratio 3.471x
- `shuffled[full]`: 3 runs — write 79.1 MiB/s (p50 51306µs, p95 67158µs, p99 67540µs), read 872.1 MiB/s, fsync p50 1649µs, physical median 2351510 bytes, ratio 1.784x

Device writes during campaign window (nvme1n1p1): 342736896 bytes written, 12288 bytes read.

## Raw output

```text
entropyfs evidence campaign — 1787666036
revision: 43bf17e
  structured [full] 5 runs: write 390.2 MiB/s (p50 40112µs, p95 44059µs, p99 44059µs) read 3168.3 MiB/s fsync p50 1655µs p95 2783µs p99 3814µs physical median 19844 ratio 845.455x
  src [full] 3 runs: write 57.2 MiB/s (p50 26316µs, p95 26969µs, p99 26969µs) read 392.0 MiB/s fsync p50 1658µs p95 2560µs p99 2560µs physical median 467025 ratio 3.388x
  urandom [full] 3 runs: write 97.5 MiB/s (p50 81516µs, p95 82015µs, p99 82015µs) read 4089.8 MiB/s fsync p50 1651µs p95 5268µs p99 5268µs physical median 8413743 ratio 0.997x
  compressed-z19 [full] 3 runs: write 98.7 MiB/s (p50 2657µs, p95 2672µs, p99 2672µs) read 3743.4 MiB/s fsync p50 1642µs p95 2479µs p99 2479µs physical median 278504 ratio 0.993x

== ablation ladder (structured) ==
  full       physical        19844 ratio 845.455x write    390.5 MiB/s cpu 0.040+0.000s (p95 write 39882µs)
  raw        physical       277382 ratio  60.484x write    558.0 MiB/s cpu 0.030+0.000s (p95 write 27525µs)
  raw-rans   physical        32816 ratio 511.251x write    266.1 MiB/s cpu 0.050+0.000s (p95 write 58943µs)
  no-dedup   physical        19844 ratio 845.455x write    382.6 MiB/s cpu 0.040+0.010s (p95 write 40590µs)
  no-base    physical        19844 ratio 845.455x write    394.4 MiB/s cpu 0.030+0.000s (p95 write 39441µs)
  no-config  physical        32816 ratio 511.251x write    265.9 MiB/s cpu 0.060+0.000s (p95 write 58637µs)
  no-rans    physical        79235 ratio 211.740x write    466.5 MiB/s cpu 0.040+0.000s (p95 write 33004µs)
  no-universe physical        19844 ratio 845.455x write    390.6 MiB/s cpu 0.040+0.000s (p95 write 39741µs)
  no-dsfb    physical        19844 ratio 845.455x write    379.3 MiB/s cpu 0.040+0.000s (p95 write 41076µs)

== DSFB search-budget investigation (structured) ==
  full      write median   392.3 MiB/s (min 390.0, max 393.3) cpu median 0.040s physical [19844, 19844, 19844, 19844, 19844]
  no-dsfb   write median   379.0 MiB/s (min 368.1, max 381.8) cpu median 0.040s physical [19844, 19844, 19844, 19844, 19844]
  physical identical across modes: true

== versioned experiment (H2) ==
  versioned [full] 3 runs: write 78.8 MiB/s (p50 38314µs, p95 94925µs, p99 95375µs) read 876.4 MiB/s fsync p50 1652µs p95 17967µs p99 17967µs physical median 1524135 ratio 2.752x
  versioned [no-base] 3 runs: write 98.6 MiB/s (p50 43725µs, p95 46289µs, p99 46411µs) read 1121.5 MiB/s fsync p50 1766µs p95 8021µs p99 8021µs physical median 1208262 ratio 3.471x
  shuffled [full] 3 runs: write 79.1 MiB/s (p50 51306µs, p95 67158µs, p99 67540µs) read 872.1 MiB/s fsync p50 1649µs p95 8168µs p99 8168µs physical median 2351510 ratio 1.784x
  sequential median reachable: 1524135 bytes (2.752x)
  shuffled    median reachable: 2351510 bytes (1.784x)
  base+residual savings vs shuffled: 827375 bytes (35.2% of shuffled reachable)

== GC and optimizer traffic ==
  unreachable before 50903851 → reclaimed 47922933 → after 2981156; physical 59651602 → 11669508; gc 0.009s; optimizer scanned 128 rewrote 0 saved 0

== baselines ==
  waived: btrfs with compression: waived — writable compressed-FS baseline requires root for loop-mounting a test image
  waived: EROFS/SquashFS: waived — read-only compressed-image baseline deferred (requires root and/or mkfs.erofs)
  raw file (ext4): 4559.9 MiB/s write, ratio 1.000x
  zstd -1: 1582503 → 398979 bytes (3.966x), 0.004s
  zstd -19: 1582503 → 276659 bytes (5.720x), 0.232s
  direct rANS (same backend, src corpus): 1582503 → 467025 bytes (3.388x)
device nvme1n1p1: 669408 sectors written (342736896 bytes), 24 sectors read (12288 bytes)

== admission checklist (methodology §8) ==
  [OK ] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
  [OK ] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
  [OK ] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
  [OK ] ablations identify which mechanism caused the gain — ablation ladder has 9 rows (A0–A8 families)
  [OK ] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
  [OK ] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
  [OK ] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md
```
