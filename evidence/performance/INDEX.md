# EntropyFS performance evidence index

This directory is the home of admitted performance evidence
(`docs/performance/methodology.md` §9). The absence of files here means no
project performance claim is admitted. Files here mean exactly the claims
they document are admitted — nothing more.

| Artifact | Revision | What it seals |
| --- | --- | --- |
| `campaign-1787658658-67d977a/` | `67d977a` | Store-level evidence campaign (methodology §1–§9): repeated runs, exact byte accounting, p50/p95/p99, fsync latency, CPU, device writes, GC traffic, ablation ladder, DSFB investigation, negative controls, baselines. All §8 admission rules OK. |
| `campaign-1787665094-a6641d1/` | `a6641d1` | Same campaign with the **SequenceRans floor** (Phase 8 §4), the write-batch group-commit path, and a corrected GC reachability walk for SequenceRans objects (an earlier run under-counted reachable bytes and was withdrawn; the admission rule is that withdrawn artifacts are replaced, never kept as claims). All §8 admission rules OK. |
| `campaign-1787666036-43bf17e/` | `43bf17e` | Same campaign with the **BASE_SEQUENCE shift-aware delta** residuals (Phase 8 §5) active on base channels. H2 flips back to positive: sequential 2.752× vs shuffled 1.784× (+35.2% base savings). The shuffled control grows (1.23 MB → 2.35 MB) because copy/literal deltas exploit structural similarity between unrelated-history chunks (class-2 chunks share a period-7 skeleton), not just temporal adjacency — that confounding is the finding. All §8 admission rules OK. |
| `campaign-1787666589-e895fcf/` | `e895fcf` | Same campaign with **SPARSE_BLOCK64** (blockwise-64 enumerative sparse coding, Phase-8 §6) in the pipeline. Physical results stable (H2 +35.0%); the campaign caught and the fix sealed a ~3× write-throughput regression from missing dense-input pre-gating (structured 394 → 135 MiB/s with the bug, restored to 360 MiB/s after the k ≥ n/2 density gate). All §8 admission rules OK. |
| `fuse-court-1787659785-027c959-head/` | `027c959` | FUSE-frontend perf court, **after** Phase 6 (current main). |
| `fuse-court-1787659914-709a710-before/` | `709a710` | FUSE-frontend perf court, **before** Phase 6 (same workloads, same workload hash `82442892…`). |
| `fuse-court-1787664579-d90772c/` | `d90772c` | FUSE-frontend perf court, **Phase 8** (concurrency refactor + writeback negotiation + batch group commit + SequenceRans floor; same deterministic shake_128 payload, same bindgen workload `82442892…`). |

## FUSE court pair (Phase 6 before/after)

Same script, same machine, same deterministic shake_128 payload, same
bindgen workload (source hash `824428929fe76cd3c37276493c945d21d2cf86d08f15cf89694e90b2e711a106`,
bindgen 0.70.1, cargo.lock `13659abc…`).

| Workload | `709a710` (before) | `027c959` (after) | `d90772c` (Phase 8) |
| --- | --- | --- | --- |
| 4K buffered writes | 0.6 MiB/s | 24.4 MiB/s | **335.2 MiB/s** |
| 1M writes (trailing fsync) | 185.1 MiB/s | 652.6 MiB/s | 620.8 MiB/s |
| warm sequential read | 2207.1 MiB/s | 2343.8 MiB/s | 2256.7 MiB/s |
| fsync p50 | 320 µs | 1647 µs | 1649 µs |
| 1M read p50 | 1173 µs | 1193 µs | 1137 µs |
| bindgen build (cold target on the mount) | **FAILED** — SIGSEGV/SIGBUS | 9.47 s, OK | 10.47 s, OK |

Notes:

- The Phase-8 4K-buffered improvement (24.4 → 335.2 MiB/s) is the
  measured result of the kernel writeback cache (`FUSE_WRITEBACK_CACHE`)
  aggregating tiny writes into large `write()` requests plus the batch
  group-commit transaction path (Phase-8 M1/M2).
- 4K dsync (per-op durability) is high-variance (0.6–1.5 MiB/s both
  revisions; ~2.5–6.7 ms/op) — the synchronous write-through path dominates.
  Phase-8: 0.5 MiB/s / 7.9 ms/op on this run (incompressible payload).
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

## Campaign highlights (`campaign-1787666589-e895fcf` — + SPARSE_BLOCK64)

Same methodology, same machine, same corpora; the blockwise-64 sparse
codec (Phase-8 §6) is in the pipeline. The campaign corpora contain no
sparse-configuration chunks, so physical results are stable: H2 +35.0%
(sequential 2.745× vs shuffled 1.784×), structured 845×, src 3.452×,
urandom 0.997×.

The campaign's engineering value here: it caught a ~3× write-throughput
regression (structured 394 → 135 MiB/s, urandom 96 → 51 MiB/s) from the
encoder building doomed streams for near-dense chunks (urandom has
k ≈ 0.996n). A k ≥ n/2 density pre-gate restores 360 MiB/s structured /
95 MiB/s urandom. The regression + fix are sealed in this campaign pair
(the intermediate artifacts were withdrawn; the fix is regression-tested
in `src/entropy/sparse64.rs`).

## Campaign highlights (`campaign-1787666036-43bf17e` — + BASE_SEQUENCE deltas)

Same methodology, same machine, same corpora; the shift-aware copy/literal
delta residual (Phase-8 §5, residual kind 0x04) is active on every base
channel.

- **H2 flips back to positive:** sequential full 2.752× vs shuffled full
  1.784× — base+residual savings +35.2% (827,375 B). The sequential
  physical state is byte-identical to the pre-delta campaign (1,524,135 B:
  aligned XOR mutations were already cheap); what changed is the shuffled
  control, which grew from 1,229,568 B to 2,351,510 B because 34 of 64
  shuffled chunks now keep base chains. The delta family finds matches
  between unrelated-history chunks that share structure (all class-2
  chunks are period-7 skeletons with different mutations), so the
  shuffled control no longer isolates *temporal* causality — copy/literal
  deltas capture *structural* similarity too. That confounding is the
  finding: BASE_SEQUENCE gains are not purely temporal.
- **Everything else is stable:** src 3.346×, structured 845× (dedup-
  dominated, labeled), urandom 0.997×, compressed 0.993×; DSFB gap and
  ablation ladder unchanged in shape.
- **Baselines:** raw ext4 1.000×; zstd -1 3.832×; zstd -19 5.502×;
  direct rANS 3.344×; btrfs/EROFS waived (root loop mounts).

## Campaign highlights (`campaign-1787665094-a6641d1` — SequenceRans floor)

Same methodology, same machine, same corpora as `67d977a`, with the
SequenceRans general compression floor (Phase-8 §4) active in the write
path and the versioned experiment now completing.

**Withdrawal note:** an earlier run of this campaign (withdrawn) measured
`src` at 124.8× — the store GC reachability walk did not yet mark
SEQUENCE_RANS model/enc objects, so reachable bytes were under-counted
and every SequenceRans extent inflated the ratio. The walk is fixed
(regression-tested in `tests/enospc.rs`); the numbers below are the
corrected run.

- **src corpus (source-tree pack): 1.636× → 3.344×** physical density
  (439,989 B for 1,471,135 logical). SequenceRans wins all 23 chunks and
  lands exactly on the direct-rANS baseline — the current foreground
  matcher (greedy, chain-depth 16, 131-byte copy cap) adds little over
  per-chunk entropy coding on this pack, and zstd -1 (3.832×) / -19
  (5.502×) still beat it. This is the measured state of the floor, not a
  claim: a deeper matcher is the obvious next step (Phase-8 §4
  background search).
- **H2 sign flip (honest negative finding):** with the cheaper floor,
  fresh re-encoding of a mutated chunk now beats keeping a base+residual
  chain on the drift corpus — sequential full 2.752× vs no-base 3.471×
  vs shuffled 3.411× (−24% base savings). Each chain layer's descriptor
  and the retained per-content-id chunk-index entries stay reachable, so
  chain accumulation now costs more than re-encoding. The `67d977a`
  campaign's +7.2% H2 result was conditional on the weaker RANS-era
  floor; the two campaigns together are a controlled comparison of the
  mechanism's value vs. the compression floor. **Superseded for the full
  pipeline by `campaign-1787666036-43bf17e`**: with the BASE_SEQUENCE
  shift-aware delta, H2 is positive again (+35.2%). The −24% here
  measures the *positional-residual-only* base machinery against the
  SequenceRans floor — the controlled value of the delta upgrade.
- **DSFB investigation (repeated 5+5, structured):** physical byte-
  identical across modes (19,844 B); write median 387.9 vs 379.6 MiB/s
  (DSFB on vs off) — a smaller gap than the RANS-era 765 vs 335,
  consistent with DSFB ordering cheaper candidates now that the floor
  handles the heavy lifting. Single synthetic corpus; under further
  study.
- **Negative controls hold:** urandom 0.997×, compressed pack 0.993×;
  shuffled history still removes temporal gains.
- **GC traffic:** 50.9 MB unreachable → 47.9 MB reclaimed, physical
  59.7 MB → 11.7 MB, 0.009 s.
- **Baselines:** raw file (ext4) 1.000×; zstd -1 3.832×; zstd -19 5.502×;
  direct rANS on the source pack 3.344× (== EntropyFS src ratio, as
  measured); btrfs/EROFS/SquashFS explicitly waived (root loop mounts).

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
