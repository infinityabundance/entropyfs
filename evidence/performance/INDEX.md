# EntropyFS performance evidence index

This directory is the home of admitted performance evidence
(`docs/performance/methodology.md` §9). The absence of files here means no
project performance claim is admitted. Files here mean exactly the claims
they document are admitted — nothing more.

| Artifact | Revision | What it seals |
| --- | --- | --- |
| `campaign-1787658658-67d977a/` | `67d977a` | Store-level evidence campaign (methodology §1–§9): repeated runs, exact byte accounting, p50/p95/p99, fsync latency, CPU, device writes, GC traffic, ablation ladder, DSFB investigation, negative controls, baselines. All §8 admission rules OK. |
| `fuse-court-1787659785-027c959-head/` | `027c959` | FUSE-frontend perf court, **after** Phase 6 (current main). |
| `fuse-court-1787659914-709a710-before/` | `709a710` | FUSE-frontend perf court, **before** Phase 6 (same workloads, same workload hash `82442892…`). |

## FUSE court pair (Phase 6 before/after)

Same script, same machine, same deterministic shake_128 payload, same
bindgen workload (source hash `824428929fe76cd3c37276493c945d21d2cf86d08f15cf89694e90b2e711a106`,
bindgen 0.70.1, cargo.lock `13659abc…`).

| Workload | `709a710` (before) | `027c959` (after) |
| --- | --- | --- |
| 4K buffered writes | 0.6 MiB/s | 24.4 MiB/s |
| 1M writes (trailing fsync) | 185.1 MiB/s | 652.6 MiB/s |
| warm sequential read | 2207.1 MiB/s | 2343.8 MiB/s |
| fsync p50 | 320 µs | 1647 µs |
| 1M read p50 | 1173 µs | 1193 µs |
| bindgen build (cold target on the mount) | **FAILED** — `proc-macro2` build script SIGSEGV, `libc` build script SIGBUS (the oversized-descriptor corruption Phase 6 fixed; full log in `bindgen-build.log`) | 9.47 s, OK |

Notes:

- 4K dsync (per-op durability) is high-variance (0.6–1.5 MiB/s both
  revisions; ~2.5–6.7 ms/op) — the synchronous write-through path dominates.
- The fsync regression (320 → 1647 µs) is the measured price of deferred
  durability: pre-Phase-6 committed synchronously per write (cheap fsync,
  slow writes); post-Phase-6 batches commits and pays the full barrier at
  fsync. The evidence records both sides.
- Cache state: warm retained page cache for both courts (drop_caches needs
  root, unavailable here). Reads therefore measure materialization + page
  cache, not cold NVMe.
- Environment (identical for both): CachyOS kernel 7.2.0-1-cachyos, AMD
  Ryzen 7 9800X3D (16 threads), governor `performance`, 131 GB RAM, backing
  device `/dev/nvme1n1p1` (ext4).

## Campaign highlights (`campaign-1787658658-67d977a`)

- **DSFB search-budget investigation (5+5 repeated runs, structured
  corpus):** identical final physical representation in every run
  (79,298 bytes) with DSFB ranking enabled vs disabled; write throughput
  median 765.4 vs 334.7 MiB/s (2.3×), user CPU 0.020 vs 0.040 s. This
  supports the assigned DSFB role: candidate-search intelligence, not
  compression magic. Single-corpus, synthetic — flagged for further study.
- **Negative controls:** urandom 0.997× (RAW fallback), zstd -19 of the
  source pack 0.993× (no additional gain), shuffled temporal history
  reduces base+residual savings (H2 negative control behaves as expected).
- **Ablation ladder (structured corpus, 9 modes):** full 79,298 B vs raw
  277,382 B vs no-config 209,161 B. Caveat: the structured corpus contains
  only 4 unique 64 KiB chunks, so content-addressed object aliasing already
  absorbs most dedup; the ladder's incremental rows must be read with that
  corpus property in mind.
- **H2 versioned experiment (synthetic drift corpus):** sequential full
  2,463,484 B vs shuffled full 2,655,556 B (temporal adjacency saves 7.2%),
  but no-base 2,141,320 B — the current base+residual implementation loses
  to re-encoding on this corpus because the derived chunk index retains
  full per-content-id descriptors (~1.3 MB of index metadata). Honest
  partial/negative H2 result on this corpus; the index-descriptor cost is
  the identified structural factor.
- **GC traffic:** 54.0 MB unreachable → 53.1 MB reclaimed (98.2%),
  physical 62.8 MB → 9.6 MB, 0.013 s.
- **Baselines:** raw file (ext4) 1.000×; zstd -1 3.604×; zstd -19 5.175×;
  direct rANS (same backend) on the source pack 1.636×; btrfs/EROFS/
  SquashFS explicitly waived (require root for loop mounts).

## Admission status

The claims documented in the artifacts above are admitted. The historical
Phase-6 session numbers in the README ("4K writes 35→47 MB/s, 1M writes
601→721 MB/s, bindgen 4m14s→1m13s") are superseded by this pair as
exploratory observations; the admitted numbers are the ones in the tables
above. The synthetic 16.876× ablation fixture (`evidence/ablation-*.json`)
is retained as a Phase-4 ablation fixture only, never as a headline claim.
