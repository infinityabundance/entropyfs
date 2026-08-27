# Phase-12E.12 oracle: physical small-object packing — REJECTED

Sealed: `evidence/performance/pack-oracle-1787840345-dc3272c/`.
Oracle: `src/tests/pack_oracle.rs`. Driver:
`tools/court-pack-oracle.sh`.

## The question

The 12E.12 brief: on contemporary EntropyFS (Phases 10/11), decompose
the PHYSICAL cost of a realistic small-file tree; implement Physical
Object Packs only if the oracle demonstrates a meaningful real small-file
win. The brief is explicit that the logical representation algebra must
NOT be contaminated with a physical-placement concern (`INLINE_PACKED`
is not a representation), and that **no pack format is allowed unless
the oracle proves the win**.

## The measurement

The brief's exact corpus classes written through the normal store path
and checkpoint-settled (durability barrier):

| class | files | size range |
| --- | ---: | --- |
| tiny source (`src/*.rs`) | 30 | 0.2–1 KiB |
| headers (`include/*.h`) | 10 | 1–4 KiB |
| configs (`etc/*.conf`) | 15 | 0.5–8 KiB |
| package metadata (`meta/*.json`) | 10 | 0.5–4 KiB |
| mixed 1–16 KiB (`data/*.bin`) | 20 | 1–16 KiB |
| mixed 16–64 KiB (`big/*.dat`) | 10 | 16–64 KiB |

95 files / 6 dirs / 726 849 logical bytes (273 B min, 743 B p25, 2.6 KiB
p50, 6.3 KiB p75, 63 KiB max). The physical cost is then decomposed from
the derived object index + the GC mark (per-tag LIVE record bytes) and
the Phase-9H physical report, with the exact cross-check
`Σ live Location::total_size == physical_report.live_bytes` (held) and
`unexplained == 0` (held).

## Results (release, sealed run)

| term | bytes | share of settled physical |
| --- | ---: | ---: |
| logical payload | 726 849 | — |
| **physical before compact** | 1 124 549 | 1.55× logical |
| dead before compact (reclaimable) | 902 295 | 80% of before |
| **physical after compact (settled)** | **222 254** | **0.306× logical** |
| live total (settled) | 222 250 | 100.0% |
| ├─ Data (payload objects) | 150 248 | 67.6% |
| ├─ BtreeNode (extent+dir+inode-index trees) | 36 105 | 16.2% |
| ├─ Inode (inode records) | 20 706 | 9.3% |
| ├─ Model (rANS model objects) | 14 945 | 6.7% |
| └─ Root | 246 | 0.1% |
| record envelopes (347 live records × 58 B) | 20 126 | 9.1% of live |
| packable objects (Data+Model): 135 records | 165 193 total / 157 363 stored | — |
| **packable envelope share** | 7 830 | **4.7% of packables** |
| padding / format (settled) | 0 / 4 | 0 / ~0% |

The dominant pre-compaction term is **write-path churn**, not object
fragmentation: 902 KiB of dead (superseded tree nodes from 95
create/write transactions + the checkpointed mutation-log records) —
reclaimed by compaction to a settled store whose overhead above the live
set is exactly 4 B (the segment magic).

## The gate decision: REJECT packs

The brief's gate: pack candidates are the DATA + MODEL objects; packs
would amortize their per-record envelopes. The measured packable
envelope share is **4.7%** — far below the 20% materiality bar in the
oracle's normative rule — and the structural + packable-envelope term is
29.2% of settled physical. A perfect pack of every Data+Model object
would reclaim at most 135 × 58 = 7.8 KiB on a 222 KiB settled store
(~3.5%). That is not a "meaningful real small-file win", and the
brief's second condition (envelope fragmentation a MAJOR term) is not
met: the settled small-file tree already achieves 3.3× density with
negligible physical overhead.

**REJECT-PACKS.** No pack format is added. The physical cost that does
exist on the write path (the 4× pre-compaction churn) is tree-node and
mutation-log churn — compaction already reclaims it, and object packs
would not touch it. A pack format would add a persistent-format surface
(offsets, lengths, overlap, truncation, count limits — the hostile-media
obligation the brief lists) for a ~3.5% ceiling that the oracle shows is
not realizable on this corpus. Recorded as-is per the "accept whatever
number comes out" rule.

## What stays

The oracle (`pack_oracle.rs`) remains in the suite as the offline
measurement surface: any future physical-layout investigation (packed
segments, inline small payloads, batching the namespace transactions to
cut the tree-churn term) has its decomposition in place, and the
per-tag live census is the first of its kind for a realistic small-file
tree.
