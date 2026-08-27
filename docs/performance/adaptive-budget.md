# Phase-12C-1 court: the adaptive foreground search budget

Sealed: `evidence/performance/adaptive-budget-probe-1787856921-98832ca/`.
Oracle: `src/tests/adaptive_budget_probe.rs`. Driver:
`tools/court-adaptive-budget.sh`. Production change:
`ForegroundMode::Focused` (`src/optimizer/foreground.rs`,
`src/optimizer/search.rs`, `src/dsfb/`).

## The question

Phase 12E.13 found the adoption wedge — a storage-DENSITY wedge
(build-artifacts 0.049× raw ≈ 20×; four workloads clear 10×) — at the
cost of put throughput ~14× slower than raw file writes, because the
foreground search prices every chunk with the full candidate sweep.
The 12C-1 question:

> Can EntropyFS preserve most of the 10–20× storage wedge while spending
> dramatically less foreground search CPU?

The user's target architecture:

```text
search_budget = f(semantic confidence, historical winner confidence,
                  worker queue pressure, current CPU saturation,
                  expected marginal storage gain, object size,
                  foreground latency target)

high confidence + high pressure -> stop early
low confidence  + low pressure  -> search broadly
background optimizer            -> recover density later
```

and the brutal gate:

```text
on the adoption-wedge workloads:
    put wall        >= 2x (ideally much more)
    search CPU      materially improved
    settled bytes   regression <= 5%
    byte identity   absolute
    p99             no material regression
    raw controls    unchanged
```

## The measurement

The oracle drives the REAL store through the engine's own put protocol
(content-id file names, the fast-dedup acknowledged-blob lookup,
tmp-write-rename — `Engine::put_blob`'s exact operations) so the `full`
arm is byte-comparable to the sealed 12E.13 rows. The corpus is the
sealed 12E.13 generators verbatim (`src/tests/adoption_corpus.rs`,
extracted so both courts share the identical bytes) plus two controls:
`noise-control` (deterministic random bytes — the RAW-control gate) and
`mixed-control` (sparse / noise / text classes — the adaptivity
demonstration). Four arms, one change each:

```text
full     ForegroundMode::Full      the sealed 12E.13 replay (anchor)
cheap    ForegroundMode::Cheap     the Phase-10B entropy-probe skip
focused  ForegroundMode::Focused   the 12C-1 adaptive budget: entropy
                                   probe + semantic class-prior rANS
                                   deferral (the one arm that enables
                                   the Phase-12C prior — its input)
raw      ForegroundMode::RawOnly   the no-search control (CPU floor)
```

Per arm per workload: put wall, useful search CPU (the perf `search`
row + the phase decomposition), candidates/chunk, first-winning rank,
RAW fallback %, put p50/p95/p99, the GC-only settled footprint (the
sealed 12E.13 measurement), the post-background-optimizer settled
footprint (the "background recovers density later" state), and
byte-exact read-back (asserted in every arm).

## Results (release, sealed run)

### The replay anchor is exact

The `full` arm's GC-only footprint vs the sealed 12E.13 rows:
**+0.000–0.011%** (build-artifacts 908 419 vs 908 411; ci-cache 402 652
vs 402 651; container-layers 552 521 vs 552 512; generated-assets
78 519 vs 78 510; scientific-outputs 441 798 vs 441 787; source-trees
1 737 896 vs 1 737 891 — single-digit byte deltas). The regression
curves are anchored to the sealed court.

### The frontier (12C-1-0)

| workload | full search ms | cheap search ms | raw search ms | raw wall × | raw settled reg | full gc-foot |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| build-artifacts | 53.5 | 56.4 | 7.9 | 1.44× | +0.081% | 0.049 |
| scientific-outputs | 66.9 | 67.0 | 3.7 | 3.72× | +0.618% | 0.055 |
| container-layers | 28.5 | 29.7 | 2.5 | 2.29× | +0.034% | 0.084 |
| generated-assets | 6.0 | 6.2 | 0.3 | 3.83× | +0.000% | 0.096 |
| ci-cache | 28.0 | 28.9 | 2.8 | 2.86× | +0.053% | 0.105 |
| source-trees | 103.7 | 109.5 | 19.4 | 1.25× | +0.018% | 0.191 |
| noise-control | 51.7 | 4.9 | 3.9 | 4.20× | +0.000% | 1.018 |
| mixed-control | 75.6 | 45.2 | 4.4 | 4.29× | +0.002% | 0.370 |

Findings:

1. **The entropy probe (cheap) has zero headroom on the wedge**
   (wall 0.96–1.00×, search 0.93–1.00×, density +0.000%) — every wedge
   corpus is structured low-entropy text, so the 10B skip never fires.
   It IS the right lever on incompressible content (noise: 4.03× wall,
   10.5× search, density +0.000%).
2. **The search is density-OPTIONAL on the wedge**: the raw arm
   (dedup + ZERO/FILL + RAW only) plus the background optimizer
   converges to **+0.000–0.618%** of full's settled footprint on all
   six workloads — the "background recovers density later" architecture
   is quantitatively proven on the adoption corpora.
3. **The addressable search CPU is real and rANS-dominated**: the perf
   phase decomposition on build-artifacts shows the byte-rANS +
   sequence-rANS sweep is ~67% of the `search` row (configurational
   ~13%); raw cuts useful search CPU 4.7–17.9× and put wall 1.25–4.29×.
4. **The ≥2× wall gate is reachable on 4/6 workloads** by the raw
   control (container-layers 2.29×, ci-cache 2.86×, generated-assets
   3.83×, scientific-outputs 3.72×); **build-artifacts (1.44×) and
   source-trees (1.25×) are bounded by NON-SEARCH write-path cost**
   (prepare/prefill ≈ 50% of put wall — the perf decomposition shows
   `prepare` 56 ms of the 114 ms wall), which no search-budget policy
   can remove.
5. **RAW controls unchanged**: noise-control is byte-exact in every
   arm, 100% RAW winners, footprint 1.018 identical; cheap = 4.03×.

### The adaptive budget (12C-1-1): `ForegroundMode::Focused`

The focused policy adds the semantic class prior as a budget input:
when the chunk's class has earned `focused_min_observations` (16)
observations AND the class's winner distribution says rANS rarely wins
(`P(Rans) < focused_rans_skip_share` = 0.10), the rANS sweep is deferred
to the background optimizer (which the frontier proved density-safe);
classes that genuinely win with rANS keep the full sweep. The gate is
self-calibrating and engagement-counted (`focused_rans_skips`).

| workload | focused wall × | focused search × | settled reg | p99 ratio | rans skips | win-rank full→focused |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| build-artifacts | 0.97 | 0.95 | +0.000% | 1.02 | **0** | 6.00 → 0.02 |
| scientific-outputs | 0.97 | 0.97 | +0.000% | 1.00 | **0** | 4.64 → 0.58 |
| container-layers | 0.95 | 0.95 | +0.000% | 1.02 | **0** | 6.00 → 0.02 |
| generated-assets | 0.96 | 0.97 | +0.000% | 1.00 | **0** | 6.00 → 0.12 |
| ci-cache | 0.94 | 0.93 | +0.000% | 1.05 | **0** | 6.00 → 0.03 |
| source-trees | 0.98 | 0.96 | +0.000% | 1.05 | **0** | 6.00 → 0.00 |
| noise-control | **3.73×** | **10.72×** | +0.000% | 0.17 | 0* | 7.00 → 1.26 |
| mixed-control | **1.54×** | **1.72×** | +0.000% | 0.93 | **44** | 6.75 → 1.25 |

\* noise's entropy probe skips before the class gate is consulted.

Findings:

1. **The gate never starves a winning family**: on all six wedge
   workloads the prior's `P(Rans)` ≈ 1.0 (the class genuinely wins with
   rANS — the winner sits at plan position ~0 once the prior learns,
   win-rank 6.0 → 0.02), so the deferral stays OFF (0 skips) and the
   settled footprint is **+0.000% — byte-equal to full on every wedge
   workload**. The prior itself certifies the wedge's search is genuine
   computational work.
2. **The gate fires exactly where the class distrusts rANS**: on
   mixed-control the sparse class accumulates 16 observations, then 44
   rANS deferrals engage — search CPU −42%, wall −35%, density
   +0.000% (SPARSE still wins; only the wasted rANS sweep was cut).
3. **The prior's reordering is dramatic**: first-winning rank collapses
   from ~6.0 to 0.00–0.58 on the wedge and 6.75 → 1.25 on mixed — the
   winner is found at the plan's head, which is exactly the ordering a
   pressure-tightened budget needs (the 12C-1-2 wiring).
4. **RAW controls unchanged**: noise-control byte-exact, 100% RAW,
   footprint identical, 3.73× wall / 10.72× search (the entropy probe +
   the class both point the same way).

## The gate decision

```text
settled bytes <= 5%        MET  (+0.000% byte-equal on every workload)
10x wedge preserved        MET  (footprints byte-equal to full's)
byte identity              MET  (asserted in every arm)
p99 no regression          MET  (0.93-1.05 on the wedge; controls better)
raw controls unchanged     MET  (noise: 100% RAW, identical footprint)
search CPU improved        MET where the class distrusts rANS (noise
                           10.7x, mixed 1.7x); NOT on the wedge (0.93-0.97x)
put wall >= 2x             NOT MET on the wedge by the confidence gate
                           (0.94-0.98x); MET by deferral on 4/6 (raw)
```

**The adaptive budget is adopted as a first-class policy** —
`ForegroundMode::Focused` is density-exact everywhere measured (settled
+0.000%), never worse in CPU than full on the wedge, 3.7× faster on
incompressible content, and self-calibrating (its engagement counter
proves it fires only where the class distrusts the expensive families).
It replaces nothing by default in this release: the FUSE daemon keeps
`Full` (its write path does not yet feed semantic contexts — the
Phase-12C prior wiring is a follow-on), and the engine's create-time
policy is unchanged pending a follow-on court.

**The brutal ≥2× put-wall row is NOT met on the wedge workloads**, and
the phase's evidence explains exactly why:

1. **The wedge's search is genuine work.** The semantic prior itself
   certifies it: `P(Rans) ≈ 1.0` for every wedge class — the rANS sweep
   is what produces the 10–20× wedge, and a confidence gate that cut it
   would be destroying the density the phase is meant to preserve. This
   is the 12C thesis confirmed at the margin: the remaining foreground
   search is no longer wasted CPU to be scheduled away — it is the price
   of the wedge.
2. **The only cut available is deferral, and it is settled-density-
   neutral but wall-bounded.** The raw arm + background optimizer
   recovers +0.000–0.618% — the frontier's decisive number — at
   1.25–4.29× put wall. On 2/6 workloads (build-artifacts 1.44×,
   source-trees 1.25×) the wall is bounded by NON-SEARCH write-path cost
   (`prepare`/prefill ≈ 50% of put wall in the perf decomposition), so
   **no search-budget policy, however aggressive, can reach 2× there** —
   that term is a write-path optimization (batch puts, leaner prefill),
   not a search-budget one.

**Identified continuation (12C-1-2): the pressure term.** The user's
budget function's `f(worker queue pressure, CPU saturation)` is the
missing input that would defer the wedge's rANS sweep to the background
when the store is busy (capturing the 2.29–3.83× available on 4/6
workloads at settled-density-neutral cost), plus the write-path
overhead term for build-artifacts/source-trees. The frontier + the
adaptive gate are its design basis; the probe is its measurement
surface.

## What stays

- `ForegroundMode::Focused` (production, first-class, probe-measured).
- The engagement counter (`focused_rans_skips`) and the semantic prior
  accessors (`SemanticPrior::count`, `dsfb_class_rans_share`) as the
  12C-1-2 instrumentation.
- The oracle and driver in the suite (480 lib tests green); the sealed
  corpora stay shared via `adoption_corpus.rs` so every future court
  replays the identical bytes.
- The frontier's numbers — including the failed rows (cheap: no wedge
  headroom; the ≥2× wall: not met; build-artifacts/source-trees:
  non-search-bounded) — are the phase's evidence, preserved verbatim.

---

# Phase-12C-1-2 court: the pressure-aware foreground deferral

Sealed: `evidence/performance/pressure-deferral-probe-1787860601-8d41f18/`
(direct-engine) and `evidence/performance/pressure-mount-court-1787869688-8d41f18/`
(mounted-FUSE). Oracle: `src/tests/pressure_deferral_probe.rs`.
Drivers: `tools/court-pressure-deferral.sh`,
`tools/court-pressure-mount.sh`. Production change:
`ForegroundMode::Focused` gains the pressure dimension
(`src/optimizer/foreground.rs`, `src/optimizer/search.rs`,
`src/store/workers.rs` `SearchPool::pressure`, `src/store/mod.rs`
pressure state + debt accounting, `--foreground focused|pressure` on
`entropyfs mount`, `PressureMetrics` in the engine metrics DTO).

## The question

12C-1 answered "is rANS valuable for this class?" and adopted the class
gate. 12C-1-2 answers the complementary question:

> Even if rANS is valuable, is NOW the right time to pay for it?

The 12C-1-0 frontier gave the empirical permission: raw foreground +
background optimization converges to +0.000–0.618% of full settled
density on the adoption corpora, so some foreground search is DEFERRABLE
work, not mandatory write-path work. The policy under test:

```text
valuable + idle       -> run rANS now
valuable + pressured  -> persist the cheap exact representation,
                         enqueue explicit optimization debt
low-value             -> the class gate skips regardless of pressure
background            -> pay the deferred density debt
```

with the pressure scalar measured from the STORAGE ENGINE ITSELF (the
worker pool's `in_flight / capacity` — the brief's "do not use load
average" rule), a hysteresis band (enter 0.80 / leave 0.60) against
search/skip flapping, and a hard starvation bound
(`pressure_max_deferred_bytes`).

## The mechanism (production)

- `SearchPool::pressure()`: the pool's live `in_flight / capacity`
  (lock-free; the queue-depth term of the brief's scalar).
- `Store::foreground_pressure()`: the probe's deterministic override
  when set, else the pool's live signal when the store uses the pool,
  else 0.
- `Store::pressure_engaged(&fg)`: sample + hysteresis transition
  (`ForegroundPolicy::pressure_transition`: idle→pressured at
  `pressure_enter`, pressured→idle below `pressure_leave`), per-store
  lock-free state.
- `encode_guided` (Focused mode): the rANS sweep is deferred when the
  class gate fires (low-value) OR the pressure gate is engaged AND the
  debt is under the cap. The pressure mask also covers the
  configurational families when `pressure_defer_configurational` is set
  (the p50c shape). The CHEAP exact families (dedup, ZERO/FILL,
  dictionaries, bases, RAW) always stay.
- Debt accounting: `deferred_extents` / `deferred_logical_bytes` /
  `deferred_since_ns` — the pressure-deferred work since the last
  COMPLETED background pass (which re-searches every extent and resets
  the debt). Explicitly non-persistent (the brief's decision).
- Operator surface: `PressureMetrics` in `entropyfs metrics --json`
  (`pressure.pressured/rans_skips/deferred_extents/deferred_logical_bytes/
  deferred_age_ms`, registry-defined) — the "compact and settled" vs
  "accepted writes quickly and has N bytes of optimization debt"
  distinction.
- The mounted daemon: `--foreground full|cheap|focused|pressure|raw`;
  `pressure` = Focused + enter 0.80 / leave 0.60 + configurational
  deferral + the 1 GiB debt cap.

## The direct-engine court (the authority)

The sealed 12E.13 corpora + the shared noise control, driven through the
engine's put protocol under a deterministic pressure matrix (the probe
override — the pool signal is validated separately by the pool test).

### The matrix (sustained P = 0.9), p50c arm (rANS + configurational)

| workload | wall gain | ceiling | wall capture | search capture | settled reg | p99 ratio |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| build-artifacts | 1.46× | 1.52× | 0.91 | 0.95 | +0.08% | 0.61 |
| source-trees | 1.28× | 1.40× | 0.78 | 0.93 | +0.02% | 0.93 |
| container-layers | **2.07×** | 2.25× | 0.93 | 0.95 | +0.03% | 0.64 |
| generated-assets | **3.51×** | 3.94× | 0.96 | 0.97 | +0.00% | 0.32 |
| ci-cache | **2.73×** | 2.98× | 0.95 | 0.97 | +0.05% | 0.26 |
| scientific-outputs | **3.03×** | 4.16× | 0.88 | 0.89 | +0.00% | 0.37 |
| noise-control | 3.70× | 4.15× | 0.96 | 0.98 | +0.00% | 0.17 |

- **Foreground wall ≥2× on all 4 workloads the frontier said possible**
  (container-layers 2.07×, ci-cache 2.73×, generated-assets 3.51×,
  scientific-outputs 3.03×); the prepare-limited pair (build-artifacts
  1.46×, source-trees 1.28×) captures 0.78–0.91 of its measured
  available headroom — the search portion is captured (0.93–0.95) and
  the non-search `prepare` term bounds the wall, exactly as the 12C-1-0
  frontier predicted. The rANS-only p50 arm missed the container-layers
  2× (1.85×); the configurational deferral (p50c) closed it — the
  evidence picks the p50c shape.
- **Settled density +0.00% to +0.08%** everywhere (the +1% preferred
  bar; the +5% hard reject never approached). The foreground footprint
  temporarily regresses +473% to +1705% (allowed and reported — the
  brief's "foreground footprint vs eventual settled footprint"
  distinction).
- **Search CPU capture 0.89–0.97** — the ≥70% bar met on every
  workload. **p99 improved everywhere** (0.26–0.93).
- **RAW controls unchanged**: noise-control byte-exact, 100% RAW,
  footprint identical, 3.7× wall, zero debt (the entropy probe handles
  it before the pressure gate is consulted).

### The condition lanes (p50hyst: enter 0.80 / leave 0.60)

| lane | build-artifacts | scientific-outputs | noise-control |
| --- | ---: | ---: | ---: |
| idle | 0 deferrals | 0 deferrals | 0 |
| pressured (P=0.9) | 640 deferrals | 234 deferrals | 0 |
| oscillating (0.70/0.80) | **1 transition** | 1 | 0 |
| clearing | 400 deferrals, 2 transitions | 117, 2 | 0 |
| settled (idle == pressured) | 0.049 == 0.049 | 0.055 == 0.055 | 1.018 |

- **Idle behaves close to Full** (zero deferrals; the brief's "idle ≈
  Full" row).
- **Saturated defers aggressively**; **pressure clears → the foreground
  resumes and the background catches up** (the settled footprint is
  identical idle vs pressured).
- **The hysteresis kills the flap**: the plain p75 (0.75/0.75) under the
  0.70/0.80 oscillation toggles **639 times**; the hysteresis p50hyst
  toggles **once**. The brief's exact bad case, measured and fixed.

### The starvation lane

Sustained pressure (P=0.9) with a 2 MiB debt cap on build-artifacts:
**capped debt 2,106,689 B (the cap + one chunk — the bound is exact;
regression-pinned) vs uncapped 4,945,040 B**; the foreground resumes the
search at the cap; the settled footprint converges to 0.049 (== full).
The "continuous pressure cannot defer optimization forever" invariant
holds.

## The mounted-FUSE court

`full` vs `focused` vs `pressure` (`--foreground`) at 8 FUSE threads
against the brief's battery (parallel write, tree copy, untar, make -j,
mixed R/W, bursty writers, continuous saturation of DISTINCT content,
structured bursts). Readback + fsck clean in every cell.

| workload | full | focused | pressure |
| --- | ---: | ---: | ---: |
| bursty writers p50 / CPU | 22.5 ms / 24.8 s | 2.09 ms / 1.18 s | 2.00 ms / 1.17 s |
| continuous distinct p50 / CPU | 114 ms / 105.5 s | 107 ms / 59.5 s | 105 ms / 60.0 s |
| structured burst p50 | 1.69 ms | 1.39 ms | 1.38 ms |

- **The 12C-1 adaptive gate, mounted**: incompressible bursts — full p50
  22.5 ms → 2.0 ms (**11× lower latency**) and daemon CPU 24.8 s →
  1.17 s (**95% cut**).
- **Sustained distinct writes**: full burns 105.5 s of daemon CPU vs
  59.5–60.0 s for focused/pressure (**~43% cut**) with latency bounded
  (~105–114 ms p50 — the write-path-dominated regime; no unbounded
  growth under sustained writes).
- **Settled within run variance**: the full-vs-focused delta on cells
  whose policies are behaviorally identical under FUSE (no semantic
  input → the class gate is dormant) is the ±5% noise floor of the
  mounted court's write-order variance; the DETERMINISTIC convergence
  authority is the direct-engine court (+0.00–0.08%).
- **Recorded boundary**: the mounted corpora did not saturate the pool
  with expensive search work (the saturation content is probe-skipped;
  the structured content searches in ~0.3 ms/chunk), so the pressure
  gate's mounted DIFFERENTIATION did not engage measurably — the
  deterministic direct-engine court is the pressure gate's authority, and
  a mounted expensive-search saturation lane is the recorded follow-on.

## The gate decision (direct-engine authority)

```text
byte exactness        MET (asserted in every arm)
settled density       MET (+0.00% to +0.08%; the +1% preferred bar)
10x wedge             MET (settled byte-equal to full everywhere)
foreground wall       MET (>=2x on all 4 workloads the frontier said
                       possible; the prepare-limited pair captures
                       0.78-0.91 of its measured headroom)
search CPU            MET (0.89-0.97 capture vs the >=0.70 bar)
p99                   MET (improved 0.26-0.93)
background convergence MET (settled == full; the debt resets at pass
                       completion; background rewrites identical)
starvation            MET (debt bounded at the cap + one chunk,
                       regression-pinned)
raw controls          MET (noise unchanged, zero debt)
write amplification   measured (bg_rewrites/bg_saved identical across
                       the pressure arms and raw — the debt pays once)
```

**The pressure-aware deferral is ADOPTED** as the `--foreground
pressure` shape (the hysteresis band + the configurational deferral +
1 GiB debt cap) and `ForegroundMode::Focused` is the first-class policy
behind it. The mount default stays `full` (the flip needs the mounted
pressure-engagement lane — the brief's own "don't flip the default to
satisfy a roadmap bullet" discipline; the opt-in `--foreground
pressure` is available). The architectural story the phase seals:

```text
12C      context tells us what is probably valuable
12C-1    confidence tells us what can be skipped permanently
12C-1-2  pressure tells us what valuable work can be postponed
background optimizer pays the deferred density debt
```
