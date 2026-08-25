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
| `campaign-1787669923-b165d60/` | `b165d60` | **Phase 8C sealed**: in-batch dedup visibility (group-commit batches now dedup against their own pending entries). The ladder's A2-dedup step drops from 115,976 B to 64,976 B on the structured corpus; A8 (background pass) densifies 54,353 → 50,528 B; full = 54,353 B (1,234.7×). The `d04227f` “dedup measures 0” finding was the pre-fix measurement and is superseded by this campaign (the controlled before/after of the fix). All §8 admission rules OK. NOTE (amended by `923df7b`): this campaign's dedup rows measure the EXACT_REF *representation* only; content-addressed object sharing is a store invariant separately accounted from `923df7b`, and its A1-rans / “direct rANS” rows included SequenceRans (the gates were not yet split). |
| `campaign-1787671040-923df7b/` | `923df7b` | **Attribution correction + transaction-local CAS canonicalization** (Phase-8C v2). Split gates: A1 is pure byte rANS; SequenceRans is the post-registration E1 step; `allow_exact_ref` gates only the alias representation (CAS sharing is an invariant). Physical fix: duplicate payload/B-tree/model records are never re-appended (one record per content id per transaction). Structured: E1 = 50,528 B (1,328×); **post-GC total backing 55,921 B (1,200×), allocated blocks 61,440 B (1,092×)** vs the 5.1 MB pre-GC backing of earlier campaigns. zstd-per-64KiB diagnostic: SequenceRans 3.556× within 5% of zstd-per-64K 3.739× (whole-file zstd -1 4.420×) ⇒ the gap is cross-chunk context → SequenceDict direction. All §8 admission rules OK. |
| `campaign-1787674068-4892644/` | `4892644` | **Phase 9A sealed**: transaction-local COW-intermediate pruning. The incompressible physical floor collapses to ~1.00× (urandom reachable 33,652,515 B / total backing 33,658,070 B / allocated 33,665,024 B); `unreachable_bytes_by_record_tag` evidence proves the post-GC gap was B-tree intermediates. All §8 admission rules OK. |
| `campaign-1787676607-8250f6b/` | `8250f6b` | **Phase 9B sealed (SequenceDict)**: cross-chunk dictionary match coding (tag 0x0F, feature bit 12). src corpus 4.070× (up from 3.51×) — EntropyFS full now beats standalone SequenceRans (3.627×) and zstd-per-64KiB -1 (3.848×); the whole-file zstd gap (-1 4.636× / -19 6.787×) is now genuinely cross-64K-window context. E2 ladder step present; leave-one-out 13 rows; ladder 11 rows. urandom 0.997× reachable / 1.00× backing (negative control holds). H2 temporal signal preserved (sequential 3.013× vs shuffled 1.788×, +40.6%); post-GC the base chain still costs more than no-base (1,265,786 vs 1,165,681 B) — recorded as-is per the “accept whatever number comes out” rule. GC traffic: optimizer scanned 512, rewrote 0 — the foreground SequenceDict write path already densifies sequential edits (regression test updated accordingly). All §8 admission rules OK. |
| `campaign-1787679299-8d6e147/` | `8d6e147` | **Phase 9C sealed (SequenceSharedDict)**: shared amortized dictionary match coding (tag 0x10, feature bit 13). The tree court (real-tree corpus, one inode per file under its real directory structure) seals the 9C evidence gate: 279/282 files are single-chunk, so the previous-chunk dictionary gets almost no opportunity on a real tree (SEQUENCE_DICT used 3×), and the packed-stream density (src 4.09×) is mostly cross-FILE structure. zstd baselines on the tree: whole 4.978× / per-file 3.541× / per-64KiB 3.991× (-1). EntropyFS per-file writes: **2.182× → 2.328× post-GC after the shared-dict pass** (102 extents rewritten, ~85.2 KiB saved) — a real, attributable cross-file gain on ordinary source text, with the mechanism proven by the synthetic family fixtures (random-looking shared headers → large wins). E3 ladder step present; leave-one-out 14 rows; ladder 12 rows. The two intermediate runs (flat-placed tree: 0 rewrites; then real dirs with RAW-scored anchors: 27 rewrites) were unadmitted measurement iterations — their tree courts are amended in the note below, never silently kept. All §8 admission rules OK. |
| `fuse-court-1787659785-027c959-head/` | `027c959` | FUSE-frontend perf court, **after** Phase 6 (current main). |
| `fuse-court-1787659914-709a710-before/` | `709a710` | FUSE-frontend perf court, **before** Phase 6 (same workloads, same workload hash `82442892…`). |
| `fuse-court-1787664579-d90772c/` | `d90772c` | FUSE-frontend perf court, **Phase 8** (concurrency refactor + writeback negotiation + batch group commit + SequenceRans floor; same deterministic shake_128 payload, same bindgen workload `82442892…`). |
| `fs-court-1787669946-b165d60/` | `b165d60` | **Phase-8H competitive filesystem court v1** (`tools/fs-court.sh`): ext4/zstd/mounted EntropyFS on the same corpora; XFS/Btrfs±zstd/EROFS/SquashFS waived (no root/loop in that environment). EntropyFS density 1.488×. Superseded by `fs-court-1787674397-4f58334`. |
| `fs-court-1787674397-4f58334/` | `4f58334` | **Phase-8H court v2 — ZERO WAIVERS** (`tools/run-court-docker.sh` in a disposable privileged docker VM): loop-mounted XFS, Btrfs raw + zstd:1, EROFS-lz4hc, SquashFS-zstd, FUSE EntropyFS, standalone zstd, symmetric buffered/durable writes + warm/cold reads + allocated-block accounting. EntropyFS post-GC backing 73.9 MB < Btrfs+zstd image 78.7 MB for the same corpus set (1.836× vs 1.65×); the throughput gaps are recorded honestly (src tiny-file writes 8.5 vs 87–394 MiB/s; 64 MiB buffered random 79.6 vs 3,752–6,015 MiB/s). fsck clean. |

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
  (54,353 B), write median 773.9 vs 717.1 MiB/s (~8% — the RANS-era
  2.3× gap collapsed as the SequenceRans floor simplified the search
  landscape; CPU 0.08 s both modes).

## Campaign highlights (`campaign-1787671040-923df7b` — attribution + CAS canonicalization)

Same methodology, same machine, same corpora. This campaign seals the
Phase-8 review corrections:

**The two dedup layers are now separate, and the ladder means what it
says.** Content-addressed object sharing (identical payload → one
`ChunkId` → one physical record) is a store invariant, never a gate;
EXACT_REF is the gated alias representation. Per-run accounting now
reports `cas_shared_bytes_saved` (Σ (refcount−1) × size) and
`exact_ref_bytes_saved`. A1 is pure byte rANS again (the original
methodology); SequenceRans is the post-registration E1 step:

```text
A0-raw             319,070 B   210.3×   ← RAW descriptors + CAS sharing only
A1-byte-rans       267,181 B   251.2×   ← pure byte rANS (no match finder)
A2-exact-ref       251,881 B   266.4×   ← + EXACT_REF aliasing
A4-config          112,301 B   597.6×
A8-background      112,301 B   597.6×
E1-sequence-rans    50,528 B  1328.2×   ← + SequenceRans floor (production)
```

Earlier A1-rans rows (115,976 B, 578×) and the pre-split “direct rANS”
baseline included SequenceRans; they are amended here, never rewritten.

**Transaction-local CAS canonicalization is the physical fix.** The
structured corpus — the case that made the 988× vs 13× distinction
visible — now persists: reachable 50,528 B / **total backing 55,921 B
(1,200×) / allocated blocks 61,440 B (1,092×)** after GC. Earlier
campaigns wrote up to 5.1 MB of backing for the same corpus because
identical object records were appended repeatedly and the derived object
index kept only the last location (the duplicates vanished into
“allocator overhead”). Duplicate records are no longer appended at all.
Write throughput for the batch path jumps to ~1,120 MiB/s on this corpus
(CPU 0.05 s). urandom remains honest: backing 37.2 MB vs 33.6 MB logical
(0.90× — the real cost of records + index for incompressible data).

**The zstd-per-64KiB diagnostic answered the floor question.** On the src
pack (2,030,799 B):

```text
direct byte rANS           1.633×
SequenceRans standalone    3.556×  (= EntropyFS full: 3.556×)
zstd -1 per 64 KiB         3.739×
zstd -1 whole file         4.420×
zstd -19 whole file        6.432×
```

SequenceRans is within 5% of zstd-per-64K; the remaining ~19% gap to
whole-file zstd is cross-chunk dictionary context, not matcher quality.
Per the review's decision rule, the indicated direction is a
**SequenceDict** cross-chunk dictionary (local history + previous file
chunk + previous-version base as bounded dictionaries), NOT a deeper
SequenceRans matcher. Full == standalone SequenceRans on this pack: the
other families add nothing to unique source text.

**H2:** sequential 1,392,236 B (3.013×) vs shuffled 2,345,529 B (1.788×)
= +40.6% temporal savings (up from +35.0% — marginal costing helped).
Post-GC the base chain still costs more than no-base (1,265,786 vs
1,165,681 B): the index artifact is gone, the base-chain cost is real.

**DSFB:** physical identical (50,528 B); write median 1,120.8 vs 1,106.1
MiB/s (~1.3%), CPU 0.05 s both modes — the search landscape is simpler
under the SequenceRans floor; DSFB stays out of the spotlight with its
counters deferred. The DSFB series across the three eras (RANS-era 765.4
vs 334.7 = 2.29× → SequenceRans-era 773.9 vs 717.1 = ~8% → CAS-era
1,120.8 vs 1,106.1 = ~1.3%), all with byte-identical physical
representations, is the controlled record of a search-budget lever whose
marginal benefit shrank as the floor improved.

## Campaign highlights (`campaign-1787676607-8250f6b` — Phase 9B SequenceDict)

Same methodology, same machine, same corpora (the src pack grew to
2,323,661 B with the new code). This campaign seals the cross-chunk
dictionary family:

**The cross-chunk context gap is now attributable, not speculative.** On
the src pack:

```text
direct byte rANS           1.633×
SequenceRans standalone    3.627×
EntropyFS full             4.070×   ← > standalone SequenceRans: the
                                      dictionary adds cross-chunk context
zstd -1 per 64 KiB         3.848×   ← EntropyFS full now beats per-64K
zstd -1 whole file         4.636×
zstd -19 whole file        6.787×
```

The 923df7b diagnostic predicted exactly this: SequenceRans was within 5%
of zstd-per-64K, and the remaining gap was cross-chunk context. With
SequenceDict (previous same-file chunk), EntropyFS full (4.070×) beats
both standalone SequenceRans (3.627×) and zstd-per-64K -1 (3.848×). The
remaining ~12% to whole-file zstd -1 is the packed-stream caveat the
reviewer flagged: `source_tree_pack` concatenates files, so whole-file
zstd benefits from matches crossing *original file* boundaries, which a
previous-chunk-of-same-real-file dictionary cannot reach; the fs-court
mount-level corpus is the place to test whether that gap persists on a
real tree.

**E2 ladder step and leave-one-out row added** (13 leave-one-out rows,
11 ladder rows): on the structured corpus both measure 0 (the corpus is
one single-version batch of deduped/configurational chunks — there is no
cross-chunk dictionary leverage to find). The `no-sequence-dict` row and
E2 step exist so the mechanism's contribution is always visible.

**H2 temporal signal preserved; the base-chain cost is recorded as-is.**
Sequential 3.013× vs shuffled 1.788× (+40.6% temporal savings); post-GC
sequential full 1,265,786 B vs no-base 1,165,681 B — base coding still
costs more than re-encoding on the versioned corpus. This is the
“accept whatever number comes out” outcome: the index artifact was
removed in 8B, and the base-chain cost is real. SequenceDict does not
change it (the versioned corpus chunks are dict-correlated too, but the
final version's chunks are already deduped against each other).

**Background optimizer: scanned 512, rewrote 0.** The foreground
SequenceDict write path now densifies sequential edits at write time, so
the pass has nothing left to do on this corpus (the regression test
`background_pass_densifies_sequential_edits` was updated to assert the
foreground densification + pass byte-exactness instead of demanding a
pass rewrite). The pass still rebases RAW→SequenceDict where the
foreground was gated off (tested in `tests/seqdict.rs`).

**Three latent defects surfaced by SequenceDict chains, fixed and
sealed in this revision:**

1. `flatten_if_deep` validated flattened updates through the bare store,
   which failed with `MissingObject` for object-backed families (the
   update's own staged objects were invisible). It now resolves them.
2. `current_persisted_bytes` counted only RAW/RANS object ids, so an
   object-backed incumbent (SEQUENCE_RANS/SEQUENCE_DICT/SPARSE_BLOCK64)
   looked nearly free and every densification was refused. It now
   accounts every referenced object.
3. Background candidate ordering used marginal bytes, making an
   incumbent whose objects already exist immune to replacement. The
   background search now orders by full persisted bytes (the foreground
   keeps marginal bytes so reuse wins).

**Everything else stable:** urandom 0.997× reachable / 1.00× backing;
compressed-z19 0.99×; structured 1,328×; GC traffic 5,537,444 B
unreachable → reclaimed, physical 39.4 → 35.9 MB, 0.014 s; DSFB physical
identical (50,528 B), write median 1,266.6 vs 1,256.7 MiB/s (~0.8%).

## Admission status

The claims documented in the artifacts above are admitted. The historical
Phase-6 session numbers in the README ("4K writes 35→47 MB/s, 1M writes
601→721 MB/s, bindgen 4m14s→1m13s") are superseded by this pair as
exploratory observations; the admitted numbers are the ones in the tables
above. The synthetic 16.876× ablation fixture (`evidence/ablation-*.json`)
is retained as a Phase-4 ablation fixture only, never as a headline claim.

## Phase-9C tree-court amendment

Two intermediate tree-court measurements were produced during Phase 9C
development and are superseded by the sealed `8d6e147` campaign row
above. They were never admitted; this note records why, so the fixed
measurement is never mistaken for a tuned one:

1. The first tree court wrote every file flat under the root directory.
   274 heterogeneous files formed one group, no single anchor could
   capture directory-local structure, and the pass rewrote 0 extents —
   the shared dictionary looked useless. The defect was the *measurement*
   (flat placement), not the mechanism.
2. The second tree court mirrored real directories but scored anchor
   candidates against RAW bytes rather than the extents' incumbent
   representations. It rewrote 27 extents and saved ~6.7 KiB — an
   under-measurement of the actual objective by ~12×.

Both defects were fixed before sealing: `write_tree` mirrors the real
tree structure, and `select_anchor` maximizes savings against member
incumbents (the strict-cheaper rewrite objective). The sealed tree court:
EntropyFS per-file writes **2.182× → 2.328× post-GC** after the shared-
dict pass (102 extents, ~85.2 KiB saved) on the real source tree.
