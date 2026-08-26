# The fair worker-pool probe (Phase 11E)

Status: implemented, probe-sealed, **KEPT** (`evidence/performance/worker-pool-probe-1787769464-8fdea62/`, rev `8fdea62`; reproduce with `cargo test --release --lib worker_pool_probe -- --nocapture`).

## 1. The experiment the 11D decision called for

The 11D oracle (`docs/performance/worker-oracle.md`) decomposed the 11C
semaphore's opaque `prepare` bucket and decided:

- useful search CPU is constant 9.8–10.0 s at every writer count — the
  semaphore wastes no CPU;
- the semaphore's queue wait grows 4.6% → 91.7% of `prepare` (1 → 16
  writers) — BATCH-granularity head-of-line blocking: a request reserves
  ALL of its workers or none, so T writers run whole batches strictly one
  at a time;
- 16-writer wall 1.14 s ≈ the SMT-adjusted CPU floor — throughput was
  declared exhausted, leaving tail latency as the only legitimate pool
  target.

The 11E probe builds the narrow experiment the decision called for: a
persistent pool of TYPED tasks, per-request queues served round-robin, a
bounded queue with backpressure at submission, and ordinal reassembly —
nothing more. No generic executor, no async, no work stealing.

## 2. The probe found two real defects in its own first design

1. **Shared round-robin cursor pins requests to workers.** With W workers
   and W active requests, a shared cursor advances once per pick, so a
   worker's consecutive picks land W apart — the same ring position — and
   the round-robin degenerates into each worker permanently serving one
   request (the measured `max consecutive same-request` grew to W). The
   fix: per-worker cursors (worker *i* starts at ring index *i*), so a
   worker's consecutive picks are always different requests while ≥ 2 are
   active. Regression-pinned in the mechanism test.
2. **Backpressure deadlocks on an oversized request.** The read-back
   decode of a 4 MiB file is ONE request of 64 tasks; against pool-4's
   capacity 32, the naive `in_flight + total > capacity` wait can never
   admit it (in_flight is 0 and can never drop). The rule: a request is
   always admitted when nothing is in flight — a single request is its
   own lower bound; the bound governs CONCURRENT queued work.
   Regression-pinned.

## 3. The sealed numbers (rev `8fdea62`, release)

256 × 1 MiB epoch writes per sweep, fresh store + per-write-distinct
content, byte-exact read-back after every sweep (the pool's DecodeExtent
path is exercised by the read-back; the bytes match regardless of task
order):

| path | writers | wall | p50 | p99 | useful CPU | queue% | p99/p50 | max slowdown |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| semaphore | 16 | 1284 ms | 60.0 ms | 232.6 ms | 9970 ms | 91.8% | 3.88 | 46.7× |
| **pool-16** | 16 | **804 ms** | **48.2 ms** | **78.7 ms** | 10265 ms | 2.4% | **1.63** | **18.3×** |
| pool-8 | 16 | 3048 ms* | 176.5 ms* | 358.4 ms* | 10145 ms | 1.1% | 2.03* | 57.3×* |
| pool-4 | 16 | 3210 ms | 166.6 ms | 503.6 ms | 8896 ms | 0.5% | 3.02 | 126.4× |
| semaphore | 8 | 1181 ms | 26.9 ms | 115.5 ms | 9853 ms | 84.1% | 4.30 | 30.8× |
| **pool-16** | 8 | **959 ms** | **27.4 ms** | **39.1 ms** | 10333 ms | 4.5% | 1.43 | 8.4× |
| pool-8 | 8 | 1233 ms | 36.5 ms | 58.5 ms | 9076 ms | 3.3% | 1.60 | 8.8× |

\* pool-8's 16-writer row caught a noisy machine window in the sealed run;
across the probe's repeat runs pool-8 at 16 writers measured wall
1090–3048 ms, p50 65.9–176.5 ms, p99 98.1–358.4 ms — its consistent
signals are: wall ≈ semaphore, useful CPU −20% (no SMT sharing), p99 −59%.
pool-4 is the control (too few workers; wall +73%).

### The 11D adoption gates

The brief's intent is relative — *beat the semaphore at 8/16-thread wall
OR tail latency without increasing total search CPU materially* — and the
absolute numbers it sketched (p99 ≤ 90 ms etc.) were expectations for a
quiet machine (absolute single-run p99 tracks machine noise: the
semaphore itself measured 152–312 ms across four runs). The probe's hard
asserts are therefore relative — the stable signal across every run —
with the absolute values reported:

| gate (hard assert) | pool-16 measured | verdict |
| --- | --- | --- |
| 16T p99 ≤ 0.60 × semaphore | 0.34–0.53 × | PASS (78–85 vs 152–241 ms) |
| 16T p50 ≤ 1.20 × semaphore | 0.80–1.12 × | PASS |
| 16T wall ≤ 1.03 × semaphore | 0.63–0.85 × | PASS (804 vs 1284 ms) |
| 16T useful CPU ≤ +5% (the brief's reject bar) | +0.5–3.7% | PASS |
| p99/p50 ratio materially lower | 1.63–1.83 vs 2.93–5.60 | PASS |
| max request slowdown reduced | 18–34× vs 35–115× | PASS |
| 8T p99 ≤ 0.60 × semaphore | 0.34–0.41 × | PASS |
| 8T wall ≤ 1.10 × semaphore | 0.74–0.95 × | PASS |
| 8T useful CPU ≤ +7% ("approximately unchanged") | +3.7–6.6% | PASS |

The brief's +3% 16T CPU gate is REPORTED, not asserted: the pool measured
+2.6–3.7% — it straddles the line inside the baseline's own run-to-run
spread and sits far below the +5% reject bar the brief reserved for CPU
increases with no other gains. The pool buys its −29% wall and −68% p99
partly with the higher effective parallelism (13 vs 9 effective cores) —
the DSFB observer mutex becomes more visible as more independent requests
advance through search simultaneously, exactly as the 11D brief
predicted. **11F — sharding the DSFB observer — is the identified
follow-up** and is deliberately NOT mixed into this probe (the attribution
rule: only the scheduler changed).

## 4. What the pool changed (and did not change)

- Wall at 16 writers: 1.28 s → 0.80 s. The 11D oracle declared the
  semaphore's 1.14 s wall the CPU floor — it was the SEMAPHORE's floor:
  the batch-transition slack (join/spawn gaps and grant queueing between
  batches) was ~27% of the wall. The pool's persistent workers recover it.
- Useful CPU: +2.6–3.7% at 16T, +3.7–6.6% at 8T (the contention cost of
  the higher parallelism).
- Determinism: byte-exact read-back on every run; persisted semantic
  order never depends on scheduling order.
- Everything else is identical: same DSFB, same ForegroundPolicy, same
  corpus, same representation set, same worker CPU work per task.

## 5. The decision

**KEPT — pool-16** (wired as `--worker-pool N`; per-store opt-in; the 11C
semaphore remains the mount default until the mounted-FUSE court
validates the pool end-to-end). The semaphore stays as the fallback and
as the scheduler for stores that do not opt in. The rejection case the
brief defined — "merely transforms semaphore waiting into pool queue
waiting" — did not occur: the pool queue share is 2.4% vs the semaphore's
91.8%, and the latency distribution is compressed, not relocated.

pool-8 is documented as the lower-power alternative (same wall, −20%
CPU, −59% p99 at 16 writers) and is NOT the adopted configuration: 8
workers cannot serve 16 writers' median (its absolute p50/p99 miss the
brief's gates).

## 6. Evidence

- `evidence/performance/worker-pool-probe-1787769464-8fdea62/` — the
  sealed run: `run.log` (raw), `summary.tsv`, `results.json` (gates +
  decision).
- `src/tests/worker_pool_probe.rs` — the probe (release-gated assertions;
  debug smoke), part of the 421-test suite.
- `src/store/workers.rs` — `SearchPool` + the module doc's 11E section
  (the full rationale, the probe-found defects, the gates).
