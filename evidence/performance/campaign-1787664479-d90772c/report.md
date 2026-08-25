# EntropyFS evidence campaign

- campaign dir: `evidence/performance/campaign-1787664479-d90772c`
- created: unix 1787664479

## Admission checklist (methodology §8)

- [x] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
- [x] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
- [x] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
- [x] ablations identify which mechanism caused the gain — ablation ladder has 9 rows (A0–A8 families)
- [x] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
- [x] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
- [x] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md

## Summary

- `structured[full]`: 5 runs — write 382.3 MiB/s (p50 40908µs, p95 45155µs, p99 45155µs), read 3110.4 MiB/s, fsync p50 1646µs, physical median 17151 bytes, ratio 978.206x
- `src[full]`: 3 runs — write 55.8 MiB/s (p50 23604µs, p95 24175µs, p99 24175µs), read 382.6 MiB/s, fsync p50 1650µs, physical median 11083 bytes, ratio 124.824x
- `urandom[full]`: 3 runs — write 97.2 MiB/s (p50 81824µs, p95 82590µs, p99 82590µs), read 4122.4 MiB/s, fsync p50 1663µs, physical median 8413743 bytes, ratio 0.997x
- `compressed-z19[full]`: 3 runs — write 98.1 MiB/s (p50 2516µs, p95 2535µs, p99 2535µs), read 3807.0 MiB/s, fsync p50 1653µs, physical median 261181 bytes, ratio 0.994x
- `versioned[full]`: 3 runs — write 97.1 MiB/s (p50 27185µs, p95 82961µs, p99 83088µs), read 926.5 MiB/s, fsync p50 1651µs, physical median 1449257 bytes, ratio 2.894x
- `versioned[no-base]`: 3 runs — write 98.7 MiB/s (p50 43760µs, p95 45735µs, p99 46179µs), read 1124.0 MiB/s, fsync p50 1659µs, physical median 1128735 bytes, ratio 3.716x
- `shuffled[full]`: 3 runs — write 86.1 MiB/s (p50 50534µs, p95 54224µs, p99 55430µs), read 1083.8 MiB/s, fsync p50 1654µs, physical median 1150655 bytes, ratio 3.645x

Device writes during campaign window (nvme1n1p1): 334225408 bytes written, 0 bytes read.

## Raw output

```text
entropyfs evidence campaign — 1787664479
revision: d90772c
  structured [full] 5 runs: write 382.3 MiB/s (p50 40908µs, p95 45155µs, p99 45155µs) read 3110.4 MiB/s fsync p50 1646µs p95 2802µs p99 2872µs physical median 17151 ratio 978.206x
  src [full] 3 runs: write 55.8 MiB/s (p50 23604µs, p95 24175µs, p99 24175µs) read 382.6 MiB/s fsync p50 1650µs p95 2440µs p99 2440µs physical median 11083 ratio 124.824x
  urandom [full] 3 runs: write 97.2 MiB/s (p50 81824µs, p95 82590µs, p99 82590µs) read 4122.4 MiB/s fsync p50 1663µs p95 5284µs p99 5284µs physical median 8413743 ratio 0.997x
  compressed-z19 [full] 3 runs: write 98.1 MiB/s (p50 2516µs, p95 2535µs, p99 2535µs) read 3807.0 MiB/s fsync p50 1653µs p95 2450µs p99 2450µs physical median 261181 ratio 0.994x

== ablation ladder (structured) ==
  full       physical        17151 ratio 978.206x write    384.3 MiB/s cpu 0.040+0.010s (p95 write 40513µs)
  raw        physical       277382 ratio  60.484x write    544.6 MiB/s cpu 0.020+0.000s (p95 write 28317µs)
  raw-rans   physical        29046 ratio 577.608x write    250.7 MiB/s cpu 0.060+0.010s (p95 write 62631µs)
  no-dedup   physical        17151 ratio 978.206x write    374.7 MiB/s cpu 0.040+0.000s (p95 write 41380µs)
  no-base    physical        17151 ratio 978.206x write    384.7 MiB/s cpu 0.040+0.000s (p95 write 40280µs)
  no-config  physical        29046 ratio 577.608x write    265.7 MiB/s cpu 0.060+0.000s (p95 write 59036µs)
  no-rans    physical        79235 ratio 211.740x write    446.7 MiB/s cpu 0.030+0.000s (p95 write 34560µs)
  no-universe physical        17151 ratio 978.206x write    376.6 MiB/s cpu 0.040+0.000s (p95 write 41227µs)
  no-dsfb    physical        17151 ratio 978.206x write    372.9 MiB/s cpu 0.040+0.010s (p95 write 41845µs)

== DSFB search-budget investigation (structured) ==
  full      write median   380.6 MiB/s (min 374.0, max 388.2) cpu median 0.040s physical [17151, 17151, 17151, 17151, 17151]
  no-dsfb   write median   370.1 MiB/s (min 366.3, max 378.5) cpu median 0.040s physical [17151, 17151, 17151, 17151, 17151]
  physical identical across modes: true

== versioned experiment (H2) ==
  versioned [full] 3 runs: write 97.1 MiB/s (p50 27185µs, p95 82961µs, p99 83088µs) read 926.5 MiB/s fsync p50 1651µs p95 18365µs p99 18365µs physical median 1449257 ratio 2.894x
  versioned [no-base] 3 runs: write 98.7 MiB/s (p50 43760µs, p95 45735µs, p99 46179µs) read 1124.0 MiB/s fsync p50 1659µs p95 7915µs p99 7915µs physical median 1128735 ratio 3.716x
  shuffled [full] 3 runs: write 86.1 MiB/s (p50 50534µs, p95 54224µs, p99 55430µs) read 1083.8 MiB/s fsync p50 1654µs p95 7522µs p99 7522µs physical median 1150655 ratio 3.645x
  sequential median reachable: 1449257 bytes (2.894x)
  shuffled    median reachable: 1150655 bytes (3.645x)
  base+residual savings vs shuffled: -298602 bytes (-26.0% of shuffled reachable)

== GC and optimizer traffic ==
  unreachable before 50903851 → reclaimed 47922933 → after 2981156; physical 59651602 → 11669508; gc 0.010s; optimizer scanned 128 rewrote 0 saved 0

== baselines ==
  waived: btrfs with compression: waived — writable compressed-FS baseline requires root for loop-mounting a test image
  waived: EROFS/SquashFS: waived — read-only compressed-image baseline deferred (requires root and/or mkfs.erofs)
  raw file (ext4): 3288.5 MiB/s write, ratio 1.000x
  zstd -1: 1383420 → 373536 bytes (3.704x), 0.004s
  zstd -19: 1383420 → 259520 bytes (5.331x), 0.200s
  direct rANS (same backend, src corpus): 1383420 → 11083 bytes (124.824x)
device nvme1n1p1: 652784 sectors written (334225408 bytes), 0 sectors read (0 bytes)

== admission checklist (methodology §8) ==
  [OK ] benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command) — environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory
  [OK ] every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed) — per-run Accounting tables pass the reachable-bytes cross-check
  [OK ] all listed baselines run or explicitly waived — raw file present; zstd present; direct rANS present; waivers: 2
  [OK ] ablations identify which mechanism caused the gain — ablation ladder has 9 rows (A0–A8 families)
  [OK ] negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear) — urandom ratio 0.997x (expected ≤1.5x); compressed present true; shuffled present true
  [OK ] materialized output hashes match the input corpus hashes — result-hashes.json: all runs match corpus content hashes
  [OK ] raw result artifacts are archived — raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md
```
