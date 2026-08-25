# EntropyFS performance evidence index

This directory is the home of admitted performance evidence
(`docs/performance/methodology.md` §9). The absence of files here means no
project performance claim is admitted. Files here mean exactly the claims
they document are admitted — nothing more.

| Artifact | Revision | What it seals |
| --- | --- | --- |
| `campaign-1787658658-67d977a/` | `67d977a` | Store-level evidence campaign (methodology §1–§9): repeated runs, exact byte accounting, p50/p95/p99, fsync latency, CPU, device writes, GC traffic, ablation ladder, DSFB investigation, negative controls, baselines. All §8 admission rules OK. Its nine-row “ablation ladder” table is the leave-one-out table (protocol amendment below). |
| `campaign-1787665094-a6641d1/` | `a6641d1` | Same campaign with the **SequenceRans floor** (Phase 8 §4), the write-batch group-commit path, and a corrected GC reachability walk for SequenceRans objects (an earlier run under-counted reachable bytes and was withdrawn; the admission rule is that withdrawn artifacts are replaced, never kept as claims). All §8 admission rules OK. |
| `campaign-1787666036-43bf17e/` | `43bf17e` | Same campaign with the **BASE_SEQUENCE shift-aware delta** residuals (Phase 8 §5) active on base channels. H2 flips back to positive: sequential 2.752× vs shuffled 1.784× (+35.2% base savings). The shuffled control grows (1.23 MB → 2.35 MB) because copy/literal deltas exploit structural similarity between unrelated-history chunks (class-2 chunks share a period-7 skeleton), not just temporal adjacency — that confounding is the finding. All §8 admission rules OK. |
| `campaign-1787666589-e895fcf/` | `e895fcf` | Same campaign with **SPARSE_BLOCK64** (blockwise-64 enumerative sparse coding, Phase-8 §6) in the pipeline. Physical results stable (H2 +35.0%); the campaign caught and the fix sealed a ~3× write-throughput regression from missing dense-input pre-gating (structured 394 → 135 MiB/s with the bug, restored to 360 MiB/s after the k ≥ n/2 density gate). All §8 admission rules OK. |
| `campaign-1787668313-0a7d800/` | `0a7d800` | **Phase-8A**: the same campaign with the strict cumulative ladder A0–A8 (methodology §4, spec §43) running alongside the leave-one-out table. Immediate predecessor of `d04227f`; superseded by it (which adds the post-GC H2 footprint). All §8 admission rules OK. |
| `campaign-1787668526-d04227f/` | `d04227f` | **Phase 8A + 8B sealed**: cumulative ladder A0–A8 + leave-one-out tables; the derived chunk-index rebuild in GC (8B) evidenced by the post-GC permanent footprint in the H2 experiment. All §8 admission rules OK. |
| `campaign-1787669923-b165d60/` | `b165d60` | **Phase 8C sealed**: in-batch dedup visibility (group-commit batches now dedup against their own pending entries). The ladder's A2-dedup step drops from 115,976 B to 64,976 B on the structured corpus; A8 (background pass) densifies 54,353 → 50,528 B; full = 54,353 B (1,234.7×). The `d04227f` “dedup measures 0” finding was the pre-fix measurement and is superseded by this campaign (the controlled before/after of the fix). All §8 admission rules OK. |
| `fuse-court-1787659785-027c959-head/` | `027c959` | FUSE-frontend perf court, **after** Phase 6 (current main). |
| `fuse-court-1787659914-709a710-before/` | `709a710` | FUSE-frontend perf court, **before** Phase 6 (same workloads, same workload hash `82442892…`). |
| `fuse-court-1787664579-d90772c/` | `d90772c` | FUSE-frontend perf court, **Phase 8** (concurrency refactor + writeback negotiation + batch group commit + SequenceRans floor; same deterministic shake_128 payload, same bindgen workload `82442892…`). |
| `fs-court-1787669946-b165d60/` | `b165d60` | **Phase-8H competitive filesystem court** (`tools/fs-court.sh`): same corpora across ext4 (host), zstd -1/-3/-19, and mounted EntropyFS (FUSE). XFS/Btrfs±zstd/EROFS/SquashFS recorded as explicit waivers with the exact root-capable-VM commands (this environment has no root/loop devices; the methodology permits waivers, the goal is to clear them in a disposable root-capable VM). EntropyFS effective density 1.488× (apparent 135.7 MB / store 91.2 MB post-GC) including a 64 MB incompressible control; src write 1.7 MiB/s (tiny files) / read 1288 MiB/s; random 85/3532 MiB/s; zeros 453/4374 MiB/s; fsck clean. |

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
- **Everything else is stable:** src 3.346×, structured 845× (structural/
  configurational-dominated — dedup measures 0 on this corpus, see the
  `d04227f` highlights), urandom 0.997×, compressed 0.993×; DSFB gap and
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
  only 4 unique 64 KiB chunks, so the ratio is structural/configurational
  (ZERO/FILL/PERIODIC/rANS), not dedup — the `d04227f` campaign measures
  the dedup contribution at 0 on this corpus and corrects the earlier
  “content-addressed aliasing absorbs most dedup” speculation here.

  **Protocol amendment (Phase-8A):** this table is the *leave-one-out*
  table (one mechanism disabled at a time). It predates the two-table rule
  in `docs/performance/methodology.md` §4 and was labeled “ablation
  ladder” at the time; it is amended here as a mislabel, never rewritten.
  The strict cumulative ladder A0–A8 (each step adds one mechanism) is
  introduced by the Phase-8A campaign; both tables are kept forever.
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

## Campaign highlights (`campaign-1787668526-d04227f` — Phase 8A ladder + 8B index rebuild)

Same methodology, same machine, same corpora. This campaign seals the two
Phase-8 protocol/architecture corrections:

**8A — the strict cumulative ladder A0–A8 now runs beside the leave-one-out
table (both kept forever):**

```text
A0-raw             319,070 B  210.3×
A1-rans            115,976 B  578.6×   ← rANS floor: 2.75× over RAW
A2-dedup           115,976 B  578.6×   ← dedup: 0 on this corpus (below)
A3-base-residual   115,976 B  578.6×   ← no P0 base available on a fresh
                                           single-batch write
A4-config           67,868 B  988.8×   ← configurational: 1.71× over A3
A5-temporal-bases    67,868 B  988.8×
A6-universe          67,868 B  988.8×   ← negative control: 0 (correct)
A7-dsfb              67,868 B  988.8×   ← budget changes cost, not bytes
A8-full+background   67,868 B  988.8×   ← background pass: nothing to gain
                                           on a cold single-batch corpus
```

**Dedup measures 0 on the structured corpus, and the earlier “dedup-
dominated” label is corrected.** The corpus is one 64 MiB version written
as a single group-commit batch; the dedup lookup reads the committed chunk
index, so the batch's own pending entries are invisible to it (an
identified Phase-8C write-aggregation item), and the uniform zones are
already structurally cheap (ZERO/FILL/PERIODIC). The 845×–989× structured
ratio is structural/configurational, not dedup — the old INDEX/README
labels are amended here. The versioned corpus's cross-version dedup is
separately verified (drift chunks that repeat exactly across versions
still alias via EXACT_REF).

**8B — the derived chunk-index rebuild is evidenced by the post-GC
(permanent) H2 footprint.** The chunk index is a derived structure (§34);
GC now rebuilds it to exactly the reachable set (live extents + transitive
reference closure), so overwritten unsnapshotted content cannot grow it
permanently:

```text
H2 pre-GC reachable:   sequential full 1,528,175 B (2.745×)
                       no-base        1,214,754 B (3.453×)
                       shuffled       2,351,510 B (1.784×)
H2 post-GC reachable:  sequential full 1,366,816 B (3.069×)   ← −161,359 B pruned
                       no-base        1,165,681 B (3.598×)
                       shuffled       2,287,928 B (1.833×)
base+residual savings vs shuffled (pre-GC): 823,335 B (35.0%)
```

The temporal signal is unchanged (sequential ≪ shuffled); what 8B removes
is the permanent index metadata for overwritten history: 10.6% of the
sequential full footprint was historical descriptor entries that GC now
reclaims. Post-GC, the remaining full-vs-no-base gap (1,366,816 vs
1,165,681 B) is the actual base-chain cost, not index bloat — the honest
“accept whatever number comes out” outcome the reviewer prescribed: the
next cost is real. The regression test
(`gc_rebuilds_derived_chunk_index_without_history_growth`) asserts the
invariant `chunk_index_entries ≤ reachable logical content + reference
closure` and that repeated GC never regrows the index.

**DSFB investigation (5+5, structured):** physical byte-identical across
modes (67,868 B); write median 345.5 vs 339.4 MiB/s, CPU 0.180 s both —
the RANS-era 2.3× gap (765 vs 335 MiB/s) has converged as the
SequenceRans floor handles the heavy lifting; DSFB's measured role
remains search-budget intelligence, not bytes.

**Everything else stable:** src 3.455× (== direct rANS 3.455×; zstd -1
4.090×, zstd -19 5.924× — the matcher floor is still the identified gap),
urandom 0.997×, compressed 0.994×; GC traffic 59.5 MB unreachable →
48.0 MB reclaimed, physical 93.5 MB → 47.6 MB, 0.015 s; btrfs/EROFS waived
as before.

## Campaign highlights (`campaign-1787669923-b165d60` — Phase 8C in-batch dedup)

Same methodology, same machine, same corpora. This campaign seals the
write-aggregation density fix and supersedes the `d04227f` “dedup measures
0” finding (which was the controlled pre-fix measurement):

```text
leave-one-out:  no-dedup 67,868 B  vs  full 54,353 B   (dedup: −13,515 B, 1.25×)
cumulative:     A1-rans 115,976 B
                A2-dedup  64,976 B   ← in-batch dedup now hits (1,032.8×)
                A4-config 54,353 B
                A8-full+background 50,528 B   ← background pass densifies
                                                 the dedup structure (1,328.2×)
```

- The structured corpus is one group-commit batch; before 8C its pending
  chunk-index entries were invisible to the dedup lookup, so A2 == A1 and
  the 845×–989× ratio was structural/configurational only. With 8C the
  batch dedups against itself: A2 drops 51,000 B, and the leave-one-out
  no-dedup row now isolates dedup's marginal contribution (1.25×). The
  fix is regression-tested (`group_commit_batch_dedups_within_the_batch`,
  `group_commit_batch_dedup_survives_in_batch_overwrite`).
- A8 (background re-optimization) is now a genuine densifier on this
  corpus (54,353 → 50,528 B, −3,825 B): with aliases present, the pass
  rewrites the owner structure; the earlier A8 == A7 flatness was the
  absence of alias structure, not pass uselessness. The background pass
  never grows reachable bytes (gate: strictly cheaper, CAS-checked).
- The ablation CLI (`benchmark --ablation*`) now writes one group-commit
  batch, runs the A8 pass when requested, GCs, and reports REACHABLE
  bytes — the old per-chunk-transaction `physical_used` reporting made
  RAW look like a 0.944× loss and has been corrected to match the
  campaign methodology.
- H2 unchanged (2.745× vs 1.784×, +35.0%; post-GC permanent 3.069×) —
  the versioned corpus writes per-version batches, where cross-version
  dedup already worked; the 8B index rebuild evidence stands.
- Everything else stable: src 3.514× (new source pack at this revision;
  zstd -1 4.09×, zstd -19 5.92×), urandom 0.997×, compressed 0.994×,
  GC 59.5 MB → 48.0 MB reclaimed in 0.015 s; DSFB physical identical
  (54,353 B), write 773.9 vs 339.9 MiB/s (DSFB still halves search CPU
  budget work; bytes unchanged).

## Admission status

The claims documented in the artifacts above are admitted. The historical
Phase-6 session numbers in the README ("4K writes 35→47 MB/s, 1M writes
601→721 MB/s, bindgen 4m14s→1m13s") are superseded by this pair as
exploratory observations; the admitted numbers are the ones in the tables
above. The synthetic 16.876× ablation fixture (`evidence/ablation-*.json`)
is retained as a Phase-4 ablation fixture only, never as a headline claim.
