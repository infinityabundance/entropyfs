# EntropyFS evidence campaign

- campaign dir: `evidence/performance/campaign-1787665971-45ed74e`
- created: unix 1787665971

## Admission checklist (methodology §8)

- [x] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
- [x] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
- [x] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
- [x] ablations identify which mechanism caused the gain — ablation ladder has 9 rows (A0–A8 families)
- [x] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
- [x] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
- [x] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md

## Summary

- `structured[full]`: 5 runs — write 394.4 MiB/s (p50 39601µs, p95 44644µs, p99 44644µs), read 3223.2 MiB/s, fsync p50 1655µs, physical median 19844 bytes, ratio 845.455x
- `src[full]`: 3 runs — write 56.4 MiB/s (p50 25402µs, p95 25806µs, p99 25806µs), read 380.1 MiB/s, fsync p50 1651µs, physical median 449796 bytes, ratio 3.346x
- `urandom[full]`: 3 runs — write 96.0 MiB/s (p50 82386µs, p95 82863µs, p99 82863µs), read 4056.0 MiB/s, fsync p50 1669µs, physical median 8413743 bytes, ratio 0.997x
- `compressed-z19[full]`: 3 runs — write 97.9 MiB/s (p50 2642µs, p95 2644µs, p99 2644µs), read 3634.8 MiB/s, fsync p50 1647µs, physical median 274177 bytes, ratio 0.993x
- `versioned[full]`: 3 runs — write 78.6 MiB/s (p50 39768µs, p95 93313µs, p99 93623µs), read 882.9 MiB/s, fsync p50 1662µs, physical median 1524135 bytes, ratio 2.752x
- `versioned[no-base]`: 3 runs — write 100.4 MiB/s (p50 43709µs, p95 48058µs, p99 48724µs), read 1101.2 MiB/s, fsync p50 1649µs, physical median 1208262 bytes, ratio 3.471x
- `shuffled[full]`: 3 runs — write 80.5 MiB/s (p50 49454µs, p95 65431µs, p99 65549µs), read 869.8 MiB/s, fsync p50 1667µs, physical median 2351510 bytes, ratio 1.784x

Device writes during campaign window (nvme1n1p1): 359501824 bytes written, 4096 bytes read.

## Raw output

```text
entropyfs evidence campaign — 1787665971
revision: 45ed74e
  structured [full] 5 runs: write 394.4 MiB/s (p50 39601µs, p95 44644µs, p99 44644µs) read 3223.2 MiB/s fsync p50 1655µs p95 2647µs p99 3858µs physical median 19844 ratio 845.455x
  src [full] 3 runs: write 56.4 MiB/s (p50 25402µs, p95 25806µs, p99 25806µs) read 380.1 MiB/s fsync p50 1651µs p95 3066µs p99 3066µs physical median 449796 ratio 3.346x
  urandom [full] 3 runs: write 96.0 MiB/s (p50 82386µs, p95 82863µs, p99 82863µs) read 4056.0 MiB/s fsync p50 1669µs p95 5928µs p99 5928µs physical median 8413743 ratio 0.997x
  compressed-z19 [full] 3 runs: write 97.9 MiB/s (p50 2642µs, p95 2644µs, p99 2644µs) read 3634.8 MiB/s fsync p50 1647µs p95 2359µs p99 2359µs physical median 274177 ratio 0.993x

== ablation ladder (structured) ==
  full       physical        19844 ratio 845.455x write    385.0 MiB/s cpu 0.040+0.000s (p95 write 40206µs)
  raw        physical       277382 ratio  60.484x write    543.4 MiB/s cpu 0.020+0.010s (p95 write 28364µs)
  raw-rans   physical        32816 ratio 511.251x write    266.9 MiB/s cpu 0.060+0.000s (p95 write 58719µs)
  no-dedup   physical        19844 ratio 845.455x write    397.0 MiB/s cpu 0.040+0.000s (p95 write 39174µs)
  no-base    physical        19844 ratio 845.455x write    391.3 MiB/s cpu 0.040+0.000s (p95 write 39483µs)
  no-config  physical        32816 ratio 511.251x write    267.8 MiB/s cpu 0.060+0.000s (p95 write 58682µs)
  no-rans    physical        79235 ratio 211.740x write    463.0 MiB/s cpu 0.030+0.000s (p95 write 33476µs)
  no-universe physical        19844 ratio 845.455x write    393.7 MiB/s cpu 0.040+0.000s (p95 write 39525µs)
  no-dsfb    physical        19844 ratio 845.455x write    380.2 MiB/s cpu 0.050+0.000s (p95 write 41041µs)

== DSFB search-budget investigation (structured) ==
  full      write median   382.6 MiB/s (min 378.6, max 392.3) cpu median 0.040s physical [19844, 19844, 19844, 19844, 19844]
  no-dsfb   write median   381.4 MiB/s (min 371.5, max 383.3) cpu median 0.040s physical [19844, 19844, 19844, 19844, 19844]
  physical identical across modes: true

== versioned experiment (H2) ==
  versioned [full] 3 runs: write 78.6 MiB/s (p50 39768µs, p95 93313µs, p99 93623µs) read 882.9 MiB/s fsync p50 1662µs p95 23872µs p99 23872µs physical median 1524135 ratio 2.752x
  versioned [no-base] 3 runs: write 100.4 MiB/s (p50 43709µs, p95 48058µs, p99 48724µs) read 1101.2 MiB/s fsync p50 1649µs p95 7175µs p99 7175µs physical median 1208262 ratio 3.471x
  shuffled [full] 3 runs: write 80.5 MiB/s (p50 49454µs, p95 65431µs, p99 65549µs) read 869.8 MiB/s fsync p50 1667µs p95 11512µs p99 11512µs physical median 2351510 ratio 1.784x
  sequential median reachable: 1524135 bytes (2.752x)
  shuffled    median reachable: 2351510 bytes (1.784x)
  base+residual savings vs shuffled: 827375 bytes (35.2% of shuffled reachable)

== GC and optimizer traffic ==
  unreachable before 50903851 → reclaimed 47922933 → after 2981156; physical 59651602 → 11669508; gc 0.010s; optimizer scanned 128 rewrote 0 saved 0

== baselines ==
  waived: btrfs with compression: waived — writable compressed-FS baseline requires root for loop-mounting a test image
  waived: EROFS/SquashFS: waived — read-only compressed-image baseline deferred (requires root and/or mkfs.erofs)
  raw file (ext4): 4484.7 MiB/s write, ratio 1.000x
  zstd -1: 1504803 → 392822 bytes (3.831x), 0.004s
  zstd -19: 1504803 → 272332 bytes (5.526x), 0.218s
  direct rANS (same backend, src corpus): 1504803 → 449796 bytes (3.346x)
device nvme1n1p1: 702152 sectors written (359501824 bytes), 8 sectors read (4096 bytes)

== admission checklist (methodology §8) ==
  [OK ] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
  [OK ] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
  [OK ] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
  [OK ] ablations identify which mechanism caused the gain — ablation ladder has 9 rows (A0–A8 families)
  [OK ] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
  [OK ] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
  [OK ] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md
```
