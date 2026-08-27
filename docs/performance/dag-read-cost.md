# Phase-12A oracle: does reference-DAG depth predict read latency?

Sealed: `evidence/performance/dag-read-cost-probe-1787790816-ef6508b/`.
Probe: `src/tests/dag_read_cost_probe.rs`. Instrumentation:
`src/store/readcost.rs` (the `ReadCostSample` ring + the hotness tracker).

## The question

The 12A brief: **for a reference DAG that is legal and compact, when does
its repeated read/materialization cost exceed the storage savings it
provides?** EntropyFS already bounds depth (`max_reference_depth` = 4) and
rebases over-depth chains on write (`REBASE_DEPTH_THRESHOLD` = 2); 12A
must not duplicate that machinery. It must measure whether depth, fanout,
and cache state actually drive read latency — the "depth != latency"
distinction: a depth-4 chain whose dependencies are hot in memory may be
cheaper than a depth-1 representation requiring a large cold fetch.

## The measurement

Six controlled DAG families, each in its own store (isolated caches and
sample ring), constructed with the REAL encoders and committed through the
normal commit path, with the committed family histogram verified before
any measurement:

| family | shape |
| --- | --- |
| `raw` | depth 0: 8 incompressible 64 KiB chunks (RAW objects) |
| `exactref` | depth 1: 8 chunks aliasing ONE shared RAW object (fanout 8) |
| `base-inline` | depth 1–4: BaseResidual chains, search-natural inline residuals (8 files/depth) |
| `base-object` | depth 1–4: BaseResidual chains, FORCED rANS residuals — enc + model objects per level (8 files/depth) |
| `diamond` | one base, 3 residual consumers (fanout 3), one depth-2 chain on a sibling |
| `seqdict` | depth 1: SEQUENCE_DICT / SEQUENCE_SHARED_DICT references (the real 9B/9C encoders) |

The depth-1..4 chains are committed directly because the foreground write
path rebases at depth ≥ 2 — the natural path cannot produce the deep
chains the oracle must measure. Every update was byte-validated (§32)
before commit and every file read back byte-exactly.

Each family was read in seeded random 64 KiB chunk order at **cold** (0
prior passes), **warm** (1), and **hot** (8), with per-read wall latency →
p50/p95/p99 and the per-materialization `ReadCostSample` fields averaged
per depth (nodes, referenced objects, bytes fetched, I/O wait, decode
CPU, total latency).

## Results (release, sealed run)

p99, µs, cold / hot (hot in parentheses):

| family | depth 1 | depth 2 | depth 3 | depth 4 | d4/d1 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `base-inline` | 31.7 (27.8) | 33.8 (29.7) | 40.5 (33.8) | 41.8 (37.5) | **1.32 (1.36)** |
| `base-object` | 130.8 (184.2) | 229.7 (229.4) | 333.8 (373.6) | 433.2 (427.9) | **3.49 (2.32)** |

Sample witnesses (cold, `base-object`): referenced objects 3 → 12 across
depth 1 → 4 (three objects per level: enc + model + the residual's
reference closure), decode CPU 116 µs → 408 µs, bytes fetched 28 612 →
29 053 (the objects are small; the COST is the decode, not the fetch).

Other families: `exactref` ~21 µs flat regardless of fanout (the shared
target is one object; consumer count does not change per-read cost),
`diamond` 22–29 µs flat, `seqdict` depth-0 5.5 µs vs depth-1 11 µs hot
(the dictionary reference doubles a tiny read). Cold vs hot moves the
fetch terms (2–6 µs) but not the decode terms.

## The gate decision: REJECT the terminalization daemon

The brief's gate: *"Only if legal-depth DAGs demonstrate a meaningful
latency penalty should Phase 12A implement terminalization… If depth
itself fails to predict latency once cache state and representation type
are controlled, record that and reject the daemon."*

The oracle shows:

1. **Depth predicts latency only through object/decode width.** The
   object-backed chains show a strong penalty (d4/d1 ~3.3×), and the
   sample fields prove the penalty IS the width: referenced objects and
   decode CPU scale linearly with depth while bytes fetched barely move.
   The search-natural chains (inline residuals) show only ~1.35× — a walk
   step, not a cost explosion.
2. **Fanout does not predict per-read latency** (`exactref`/`diamond`
   flat), and cache state does not move the decode-dominated terms.
3. **The natural machinery already prevents the costly shape.** Rebase
   flattens chains at depth ≥ 2 on write; candidate cost already penalizes
   depth (`λ_depth` in `Policy`); the optimizer commits only strictly
   cheaper rewrites. Deep object-backed chains are essentially never
   committed in real operation.

So a terminalization daemon — "hot costly DAG → materialize exact bytes
once → search TERMINAL representations only" — is **rejected** on the
measured evidence: it would key on depth to fix a ~1.35× artifact that
the existing machinery prevents, while the real cost (object width) is
already priced by `λ_depth`. `depth > N => RAW` remains explicitly not a
candidate (the brief's rejection): it would destroy density without
measuring anything.

## What stays

The `ReadCostSample` instrumentation (`src/store/readcost.rs`) and the
hotness tracker remain as the measurement surface: the 12B durability
oracle and 12C structural semiotics will read them, and any future
measured-cost representation policy (should the write path ever produce
wide chains) has its oracle in place. The probe remains in the suite as a
regression witness for the read path's cost accounting (429 lib tests
green).
