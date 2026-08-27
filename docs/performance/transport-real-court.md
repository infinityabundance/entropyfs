# Phase-12E.11 court: SyncIo vs UringIo on real storage

Sealed: `evidence/performance/transport-real-1787839790-a9fc069/`.
Oracle: `src/tests/transport_real_court.rs`. Driver:
`tools/court-transport-real.sh`.

## The question

The 10F transport evidence (ADR-0021, tmpfs-backed) left `SyncIo` as the
default — the crash-consistency oracle — because the ~2.3 µs io_uring
submit/wait floor made `UringIo` 5–27% slower on writes and 7–12% on
reads at sub-µs tmpfs latencies. 12E.11 reruns the comparison on REAL
storage: the syscall-vs-ring tradeoff shifts when the device latency is
microseconds, not sub-microseconds. The gate: **Uring wins robustly
across the target workloads on real storage → consider flipping the
default; Sync wins small-QD / Uring wins high-QD → investigate a
deterministic `auto` policy; Uring still loses → retain the Sync
default.**

## The measurement

One fresh store per (device × backend) on the device itself, driven
through the same store API a mounted filesystem uses, with the same
foreground policy:

- pure group-commit write (256 MiB, one final durability barrier — an
  honest durable write rate) + self-CPU delta;
- fsync-heavy write (a durability barrier per 2 MiB flush, 64 MiB);
- sequential warm read (128 MiB, per-read latency → p50/p95/p99);
- random 4 KiB read (4096 deterministic-LCG samples, p50/p95/p99);
- mixed read/write (interleaved 64 KiB ops in a 1 MiB window);
- the store's write-path phase rows (the backend-attributable cost
  surface: `search` / `prepare` / `validation` / `barrier_fdatasync` /
  `barrier_sb_fsync` / `btree_mutation` …).

Devices: real NVMe (`/mnt/2tb_crucial`, CT2000T705SSD3 ext4), SATA SSD
(`/mnt/256gb_btrfs`, KINGSTON RBU-SC100S37256GD btrfs, USB-attached),
tmpfs control (`/dev/shm`). Backends: `sync`, `uring` (default features).
Six lanes, zero failures, zero waivers; per-lane sealed
`evidence-manifest.json` (schema 1, `court_schema_version` 2).

## Results (release, sealed run)

Headline rows (write/fsync MiB/s, read MiB/s + p95/p99 µs, random MiB/s,
mixed MiB/s, write/read CPU s):

| device | backend | write | fsync-hvy | read | r p95/p99 | rand | mixed | cpu |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| NVMe | sync | 418.8 | 177.6 | 4389.8 | 21/23 | 211.8 | 59.4 | 0.57/0.03 |
| NVMe | uring | 414.9 | 178.2 | 3971.5 | 23/24 | 191.8 | 57.2 | 0.58/0.02 |
| SATA | sync | 404.3 | 128.6 | 4436.2 | 20/26 | 210.2 | 57.6 | 0.57/0.03 |
| SATA | uring | 406.6 | 121.9 | 3842.6 | 25/27 | 196.4 | 56.8 | 0.58/0.03 |
| tmpfs | sync | 442.5 | 459.2 | 4367.3 | 22/23 | 214.0 | 58.8 | 0.57/0.04 |
| tmpfs | uring | 361.7 | 361.8 | 1665.1 | 50/58 | 100.0 | 47.0 | 0.68/0.07 |

Headline tally (sync / uring / tie, 13 comparisons per device):
NVMe **10/1/2**, SATA **12/0/1**, tmpfs **14/0/0** (the 10F direction
reproduces on the control). The single uring win is the SATA fsync-heavy
write (128.6 → 121.9 MiB/s).

## The gate decision: RETAIN the Sync default

`UringIo` does not win robustly on any device at this court's queue
depth. On the real NVMe lane the write paths are at parity (~±1%, both
optimizer-CPU-bound: `search`+`prepare` dominate the phase table), while
reads favor sync by ~10% (4389.8 vs 3971.5 MiB/s; p95 21 vs 23 µs) —
the same read delta 10F measured on tmpfs — and random/mixed follow the
same direction. The tmpfs control reproduces 10F and then some (uring
18% slower writes, 62% slower reads). Per the 12E.11 gate, the default
**stays `sync`**; `SyncIo` remains the semantic/crash-consistency oracle
and the io-backend parity court (`src/tests/io_backend_parity.rs`)
continues to parameterize crash/durability testing over both backends.

Scope note, recorded as-is: this court drives one stream at a time — the
small-queue-depth regime. The gate's "Sync wins small-QD, Uring wins
high-QD → investigate `auto`" branch therefore is NOT exercised by this
evidence; a multi-stream high-QD real-device sweep is the follow-up
oracle if a default change is ever reconsidered. No `auto` policy is
introduced on this evidence, and none should be without that sweep.
