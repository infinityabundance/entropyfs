# EntropyFS changelog

## v0.7.17 (2026-08-28)

**Phase 12C-1-3 — the mounted pressure-ENGAGEMENT court + the court-found
scheduler/integrity fixes.** The 12C-1-2 mounted court's recorded boundary
was that its corpora never saturated the pool with valuable search, so the
pressure gate's mounted differentiation never engaged. 12C-1-3 closes that
boundary with a sustained GIL-free 1 MiB per-(writer,round) structured
corpus (the phase probe measured the engagement floor: 1.0–1.5 s does NOT
engage, 2.0 s+ does) and PROVES the causal chain mounted — and the court
found FIVE real defects in the pool and the write/optimize paths.

- **The mounted engagement court** (sealed
  `pressure-mount-engagement-1787952773-e936b6d/`; driver
  `tools/court-pressure-mount-engagement.sh`): Full/Focused/Pressure ×
  FUSE writers 1/4/8/16/32 at pool-16 on build-artifacts, pool-8 at 16
  writers, and ci-cache/container-layers/generated-assets/
  scientific-outputs at 16 writers. The pressure state machine FIRES on
  every saturated cell — enter events: build-artifacts t16 152 / t32 196
  / p8 234, ci-cache 143, container-layers 155, generated-assets 151,
  scientific-outputs 138; 4 398–16 384 rANS skips and 275 MB–1 GiB of
  deferred debt per cell; the t32/p8 cells hit the **1 GiB starvation
  cap with 6 106 / 7 353 cap-engagements** (the bounded-debt invariant
  working mounted). Engagement starts at 16 writers (t1/t4/t8 enter=0 —
  the 12C-1-2 boundary reproduced at the low end). Byte identity is
  absolute (the deterministic corpus is regenerated and compared) and
  fsck is clean in every cell (4 cells' first-pass fsck raced the
  daemon teardown and failed transiently; clean on re-run, recorded).
- **The court-found pool defects** (`src/store/workers.rs`):
  1. **runtime-mutex-across-wait deadlock** — `SearchPool::submit` held
     the pool's `runtime` lock across the backpressure wait while the
     worker tasks' own `pressure_engaged` → `POOL.pressure()` calls take
     the same lock; under >capacity concurrency (16 writers × 16 chunks
     vs capacity 128) the workers blocked on the held guard, tasks never
     completed, and the mount deadlocked (16 FUSE requests pending, ring
     empty). The submit now clones the shared state under a short lock.
  2. **backpressure starvation + lost wakeup** — the notify fired only
     at the full drain and without the wait lock; fast re-submitters
     re-took capacity before the pool reached zero and a notify could
     land between a submitter's check and its sleep. The dedicated
     `backpressure_cv` notifies on EVERY decrement under the wait lock.
  3. **admission TOCTOU** — the check-then-act allowed peak in-flight to
     overshoot capacity (160 > 128); admission is now atomic with the
     check under the wait lock.
  Regression pins: `pool_admits_all_waiters_under_capacity_saturation`,
  `pool_write_path_saturation_stays_live`, and the 11E probe's
  backpressure assertion.
- **The court-found integrity defects**:
  4. **overlay-only-inode crash** — the search's base channels called
     `base_chunk_at` → `read_file` on freshly-created
     (epoch-overlay-only) inodes and crashed with
     `Invariant("inode N missing")`; no committed data now means "no
     base" (`Ok(None)`).
  5. **over-depth chains crashed the optimize and left unreadable live
     extents** — a background rewrite can deepen a chain past the decode
     cap; the search's base read of an over-depth chunk aborted the
     whole pass (`DepthExceeded`) BEFORE the end-of-pass repair sweep
     could run, and a post-mortem scan found FOUR LIVE extents (a file
     region at ~68.5 MB) whose depth-5 chains made them unreadable
     (user-visible EIO on `read_file`). Fixes: `base_chunk_at` treats an
     unreadable base as "no base"; the repair sweep runs BEFORE the
     per-extent search (the pre-pass rebase); the detection uses
     `chain_depth_uncapped`. Pinned by `src/tests/overdepth_rebase.rs`
     (fixture-staged depth-5 chains: detection, repair, idempotence,
     optimize survival, overlay-only writes on pool and semaphore
     paths).
- **The debt generation/cut** (the user-named race): `optimize_pass`
  snapshots the pending debt at pass start and subtracts ONLY that
  snapshot at completion — debt created DURING the pass survives (the
  operator is never told the store is settled when new deferrals raced
  the pass). Pinned by
  `debt_created_during_optimizer_pass_survives_completion`.
- **Pressure state-machine witnesses** (the mounted court's causal
  evidence): samples/enter/leave events, time pressured, peak debt, and
  debt-cap engagements — in the daemon's `--stats-file` dump and the
  engine metrics DTO (`schema_version` 2; the Go binding and the C-ABI
  test updated).
- **The promotion decision**: the gate rows are evaluated in
  `docs/performance/adaptive-budget.md` §12C-1-3. Byte identity, fsck,
  engagement, bounded debt, idle≈Full and low-concurrency rows are MET;
  the foreground wall/CPU are flat and p99 mixed on the 2 s write
  phase, and the settled rows are budget-limited (150 s/cell; the
  deterministic convergence authority is the 12C-1-2 direct-engine
  court, +0.00–0.08%) — so **the mount default stays `full`** and
  `--foreground pressure` remains the adopted engine-level shape. The
  recorded follow-on for the flip: a longer sustained mounted court
  with a full settle budget.

## v0.7.16 (2026-08-27)

**Phase 12C-1-2 — the pressure-aware foreground deferral, ADOPTED.** The
12C-1 class gate answered "is rANS valuable for this class?"; the
12C-1-2 pressure gate answers "even if rANS is valuable, is NOW the
right time to pay for it?" — valuable + idle runs rANS now, valuable +
pressured persists the cheap exact representation and enqueues explicit
optimization debt, and the background optimizer pays the deferred density
debt. The direct-engine court (the authority) meets EVERY gate row.

- **The mechanism** (`src/optimizer/foreground.rs` pressure parameters +
  the hysteresis `pressure_transition`; `src/store/workers.rs`
  `SearchPool::pressure` — the pool's live `in_flight / capacity`, the
  brief's "use what the engine knows, not load average"; `src/store/`
  pressure state + `foreground_pressure` + `pressure_engaged` + the
  debt accounting; `encode_guided`'s pressure mask (rANS + optional
  configurational); `optimize_pass` completion resets the debt;
  `--foreground focused|pressure` on `entropyfs mount`;
  `PressureMetrics` in `entropyfs metrics --json` — the operator's
  "compact and settled" vs "accepted writes quickly and has N bytes of
  optimization debt" distinction).
- **The direct-engine court** (sealed `pressure-deferral-probe-*/`): the
  p50c shape (rANS + configurational deferral) meets **foreground wall
  ≥2× on all 4 workloads the frontier said possible** (container-layers
  2.07×, ci-cache 2.73×, generated-assets 3.51×, scientific-outputs
  3.03×); the prepare-limited pair (build-artifacts 1.46×, source-trees
  1.28×) captures 0.78–0.91 of its measured headroom; **search CPU
  capture 0.89–0.97 (the ≥0.70 bar); settled density +0.00–0.08% (the
  +1% preferred bar; the +5% reject never approached)**; p99 improved
  0.26–0.93; RAW controls unchanged; the foreground footprint
  temporarily regresses +473–1705% (allowed and reported — the settled
  footprint is the authority). **The hysteresis kills the flap**: the
  plain p75 toggles **639×** under the 0.70/0.80 oscillation, the
  p50hyst (enter 0.80 / leave 0.60) toggles **once**. **The starvation
  bound is exact**: the 2 MiB cap bounds the debt at the cap + one
  chunk (regression-pinned); the settle converges to full's footprint.
  Condition lanes: idle ≈ Full (0 deferrals), saturated defers
  aggressively (640), pressure clears → the foreground resumes and the
  background catches up (settled identical).
- **The mounted-FUSE court** (sealed `pressure-mount-court-*/`):
  incompressible bursts — full p50 22.5 ms / 24.8 s daemon CPU vs
  focused/pressure 2.0 ms / 1.17 s (**11× latency, 95% CPU**);
  sustained distinct writes — full 105.5 s CPU vs 59.5–60 s (**43%
  cut**) with latency bounded; readback + fsck clean everywhere; the
  settled is within the mounted write-order variance (the deterministic
  direct-engine +0.00–0.08% is the convergence authority). Recorded
  boundary: the mounted corpora did not saturate the pool with
  expensive search, so the pressure gate's mounted differentiation did
  not engage measurably — a mounted expensive-search saturation lane is
  the follow-on.
- **Decision**: the pressure-aware shape is ADOPTED as `--foreground
  pressure` (hysteresis band + configurational deferral + 1 GiB debt
  cap); `ForegroundMode::Focused` is the first-class policy behind it;
  the mount default stays `full` pending the mounted pressure-engagement
  lane (the brief's "don't flip the default to satisfy a roadmap
  bullet" discipline).

## v0.7.15 (2026-08-27)

**Phase 12D-1 — the entropy-coded grammar skeleton (the "persisted
entropy" refinement the 12D-0 verdict identified), STOPPED per the
brief's gate.** The refinement is real and large: the grammar skeleton
is not literal — the sequence matcher entropy-codes it at **3.88
bits/byte** (60 059 B → 29 156 B), cutting the fully-accounted grammar
from 66 059 B to **35 156 B (341.8×, −47%)** and closing the zstd gap
from **2.2× to 1.18×** while beating EntropyFS settled 13.2×. But the
gate requires beating EVERY incumbent: **zstd-whole (29 731 B) remains
1.2× smaller**, so **the format-bit investigation is NOT justified** and
the 12D line records its boundary. The remaining 1.18× is decomposed:
context-modeling quality (the sequence matcher's order-1 vs zstd's
order-2+ on the LCG-text skeleton) + raw state encoding (17% of the
grammar cost) — the 12C/12D "contextual entropy models" direction is
the only (not-yet-justified) path to the format bit.

- **`grammar_ec_oracle`** (`src/tests/grammar_oracle.rs`, `tools/
  court-grammar-ec.sh`, sealed
  `evidence/performance/grammar-ec-oracle-1787857795-806432e/`): the
  12D-0 grammar with the skeleton stored as a normal content-addressed
  CHUNK — `grammar_chunk_cost` charges the smallest valid candidate's
  full persisted bytes (byte-rANS / sequence-rANS / the four
  configurational families / RAW, exact-cost selection, descriptor +
  model + objects + integrity). Full accounting:
  `chunk_cost(skeleton) + Σ(state + descriptor)`, state still raw
  (conservative). Generated-config (200 × 64 KiB): grammar EC 35 156 B
  (341.8×) vs the 12D-0 raw grammar 66 059 B (181.9×), EntropyFS
  settled 465 068 B (25.8×), zstd-whole 29 731 B (404.1×). Diverse
  negative control loses as expected (EC total 13 109 241 B vs
  EntropyFS 5 350 054 B; the induction finds no shared skeleton).
- **Verdict: STOP (recorded).** The entropy-coded grammar is within 18%
  of whole-pack zstd while providing per-member RANDOM ACCESS (an
  architectural property the pack lacks), but the gate is the gate — no
  format bit without beating every incumbent. The oracle stays in the
  suite as the offline measurement surface; the identified (not
  justified) continuation is an order-2+ contextual coder + rank-coded
  state, each requiring its own evidence round.

## v0.7.14 (2026-08-27)

**Phase 12C-1 — the adaptive foreground search budget: the cost–density
frontier on the sealed adoption corpora PLUS the first-class
`ForegroundMode::Focused` adaptive gate.** The question was whether
EntropyFS can preserve the 12E.13 10–20× storage wedge while spending
dramatically less foreground search CPU. The court's answer is precise:
**the wedge's search is genuine computational work — the semantic prior
itself certifies it — and the adaptive budget preserves the wedge
byte-for-byte while cutting search CPU wherever the search is waste.**

The brutal gate's rows: settled bytes ≤5% **MET (+0.000% byte-equal on
every workload)**; 10× wedge preserved **MET**; byte identity **MET**;
p99 no regression **MET**; RAW controls unchanged **MET**; search CPU
improved **MET where the class distrusts rANS (noise 10.7×, mixed
1.7×)**; put wall ≥2× **NOT MET on the wedge by the confidence gate** —
recorded with the boundary, see below.

- **12C-1-0 — the frontier** (`src/tests/adaptive_budget_probe.rs`,
  `tools/court-adaptive-budget.sh`, sealed
  `evidence/performance/adaptive-budget-probe-1787856921-98832ca/`):
  the six sealed 12E.13 corpora shared via
  `src/tests/adoption_corpus.rs` (extracted verbatim; the refactored
  adoption oracle reproduces the sealed bytes exactly), driven through
  the engine's own put protocol (content-id names, fast-dedup lookup,
  tmp-write-rename), full/cheap/raw arms. The `full` arm replays the
  sealed court to **+0.000–0.011%**. Findings: (1) the entropy probe
  has **zero headroom on the wedge** (wall 0.96–1.00×, density
  +0.000%) — every wedge corpus is structured low-entropy text; (2)
  the byte+sequence rANS sweep is **~67% of search CPU**; (3) **the
  search is density-OPTIONAL**: raw + background optimizer converges to
  **+0.000–0.618% settled on all six workloads**; (4) the deferral
  ceiling is 1.25–4.29× put wall, with **build-artifacts (1.44×) and
  source-trees (1.25×) bounded by NON-SEARCH write-path cost**
  (`prepare` ≈ 50% of put wall in the perf decomposition) — no
  search-budget policy can reach 2× there; (5) RAW controls unchanged
  (noise: byte-exact, 100% RAW, cheap 4.03×).
- **12C-1-1 — `ForegroundMode::Focused`, the adaptive budget**
  (`src/optimizer/foreground.rs`, `src/optimizer/search.rs`, the
  `dsfb_class_rans_share` / `SemanticPrior::count` accessors): the
  entropy probe PLUS a semantic class-prior rANS deferral — when the
  chunk's class has ≥16 observations and its winner distribution says
  rANS rarely wins (`P(Rans) < 0.10`), the rANS sweep is deferred to
  the background (frontier-proven density-safe); classes that win with
  rANS keep it. The gate is self-calibrating and engagement-counted:
  **0 skips on all six wedge workloads** (the prior certifies
  `P(Rans)≈1` — the wedge's search is genuine work; first-winning rank
  6.0 → 0.02), **44 skips on the distrustful mixed sparse class**
  (search −42%, wall −35%, density +0.000%). Noise: **3.73× put wall,
  10.72× search CPU, density +0.000%**. Focused is adopted as a
  first-class policy; the FUSE/engine defaults stay `Full` pending the
  prior-wiring court (the mounted write path does not yet feed semantic
  contexts).
- **The boundary (recorded honestly):** the ≥2× put-wall row is not
  met on the wedge by any confidence gate, because the wedge's rANS is
  the price of the wedge. The identified continuation (12C-1-2) is the
  pressure term of the budget function — defer the wedge's rANS sweep
  to the background under worker-pool/CPU pressure (capturing the
  2.29–3.83× available on 4/6 workloads at settled-neutral cost) plus
  the write-path overhead term for build-artifacts/source-trees. The
  frontier + the adaptive gate are its design basis; the probe is its
  measurement surface.

## v0.7.13 (2026-08-27)

**Phase 12E.11–12E.24 — the completion of the adoption-engineering line:
real-device transport court, small-object packing oracle, object-store
adoption court, the stable C ABI, the Go binding, the Miri lane, the CI
matrix, the documentation deliverables, and the 21-point release gate —
ALL PASS.** Phase 12E is CLOSED; the release gates
(`tools/check-release-gates.sh`) report **21 passed, 0 failed**.

- **12E.11 — SyncIo/UringIo real-device transport court**
  (`src/tests/transport_real_court.rs`, `tools/court-transport-real.sh`,
  sealed `evidence/performance/transport-real-*/`): one fresh store per
  (device × backend) on the device itself — group-commit write, fsync-
  heavy write, sequential + random reads, mixed R/W, self-CPU deltas —
  across real NVMe (CT2000T705SSD3), SATA SSD (RBU-SC100S37256GD) and
  the tmpfs control. Sealed tally (sync/uring/tie of 13): **NVMe
  10/1/2, SATA 12/0/1, tmpfs 14/0/0** — writes at parity on real
  storage, reads favor sync ~10% (the 10F read delta reproduced on
  hardware), the 10F direction reproduced on the control. **Sync
  remains the default** (the crash-consistency oracle); the small-QD/
  high-QD `auto` branch recorded as the follow-up oracle.
- **12E.12 — physical small-object packing oracle, REJECTED**
  (`pack_oracle.rs`, `tools/court-pack-oracle.sh`, sealed): a realistic
  95-file small-file tree (726 849 logical B) decomposed per-tag with
  the exact cross-check; **settled physical 0.306× logical (3.3×
  density; overhead above the live set = 4 B)**, packable envelope
  share 4.7% (a perfect pack saves ~3.5% at most), dominant
  pre-compaction term = tree/log write-path churn (reclaimed by
  compaction, untouched by packs). No pack format; no INLINE_PACKED;
  no representation-algebra contamination.
- **12E.13 — object-store adoption court, WEDGE-CANDIDATE**
  (`adoption_oracle.rs`, `tools/court-adoption.sh`, sealed): the six
  brief workloads benchmarked + verified byte-exact through the stable
  Engine facade only, per-workload stores, raw-file baselines. Settled
  footprint vs raw: **build-artifacts 0.049× (20×; 73% dedup),
  scientific-outputs 0.055×, container-layers 0.084×, generated-assets
  0.096×** — four workloads clear the 10× bar; the wedge is a
  **storage-density wedge for versioned/structured immutable object
  populations**; recorded tradeoff: put ~14× slower than raw file
  writes (CPU-bound foreground search), get 114–691 MiB/s.
- **12E.14 — the stable C ABI** (`src/ffi`, `include/entropyfs.h`,
  `docs/api/c-abi.md`): the narrow opaque-handle facade
  (`entropyfs_engine_open/close`, `blob_put/get/read_range`,
  `contains/sync/compact/metrics_json/last_error/free/abi_version`),
  ABI v1 independent of the on-disk format, stable error classes, one
  free mechanism for callee-allocated outputs, panic containment at
  every boundary, the crate's SECOND ledger-designated unsafe file with
  exact preconditions in `docs/security/unsafe-ledger.md`. Rust FFI
  court (5 tests) + C smoke (21 checks) PASS; cdylib added to the
  build.
- **12E.15 — the Go binding over the stable C ABI** (`go/`, `docs/api/
  go.md`, `tools/go-test.sh`): thin cgo adapter (no bespoke Rust↔Go
  path), opaque handle, RWMutex lifecycle (concurrent ops + exclusive
  close), copy-then-free ownership, stable error classes with
  `errors.Is` sentinels, hostile-input validation, the 32-goroutine
  race/stress court — **`go test -race` green** — the content-store
  example (`go/examples/content-store/`), FFI-overhead benches, and the
  **enterprise gate: the 18-stage distro court now includes the Go
  binding stage (pinned upstream Go 1.24.6) — all three lanes PASS with
  zero waivers** (almalinux 10.2 / ubuntu 26.04 / leap 16.0, immutable
  digests sealed).
- **12E.16 — the no-impossible-media-claims policy** (in
  `docs/adoption/object-store.md`): adoption demos must prove their
  exact result with all reconstructive state accounted; no pre-picked
  ratios; RAW-fallback controls stay controls.
- **12E.17 — ublk adoption path** (`docs/adoption/ublk.md`):
  experimental state, kernel/root requirements, supported ops,
  durability (flush IS the 12B group barrier) and discard semantics
  documented; `ublk bench` runs kernel-free (32 MiB byte-exact).
- **12E.18 — the Miri lane** (`tools/court-miri.sh`, `docs/security/
  miri-lane.md`, sealed `evidence/security/miri-lane-*/`): the bounded
  deterministic subset (descriptor decode, representation validation,
  materialization, residual application, bounded hostile graphs — 9
  tests) passes under Miri; the doc states EXACTLY what is and is not
  covered (no "Miri verifies EntropyFS" claim).
- **12E.19 — the CI/release matrix** (`tools/ci-matrix.sh`, sealed
  `evidence/ci/ci-matrix-*/`): fmt + clippy + msrv + feature-matrix +
  audit + deny + release suite + ffi smoke + go binding — **all PASS**
  (deny policy in `deny.toml`); privileged probes recorded, never
  hidden. The matrix caught and fixed a real base-build regression
  (the 12E.10 transport classifier referenced the uring-gated variant
  unguarded); the entire tree is now **clippy- and fmt-clean** (85
  pre-existing warnings eliminated).
- **12E.20–12E.24 — docs + gates**: `docs/api/engine.md`,
  `docs/format/compatibility-policy.md`, `docs/operations/metrics.md`,
  `docs/operations/fsck-json.md`, `docs/adoption/object-store.md`,
  `docs/adoption/release-gates.md`; the 21-point release-gate checker
  (`tools/check-release-gates.sh`) — **21 passed, 0 failed**; the
  trial path (12E.10) re-verified for the 12E.24 success criterion;
  README updated to the adoption present (the phase history stays in
  CHANGELOG).

Release evidence: `evidence/performance/transport-real-*/` +
`pack-oracle-*/` + `adoption-oracle-*/`, `evidence/portability/
distro-court-*-27f3c41/` (three lanes, zero waivers, Go stage), `evidence/
security/miri-lane-*/`, `evidence/ci/ci-matrix-*/`. The C ABI, the Go
binding, the docs and the release-gate checker are the stable adoption
surface going forward.

## v0.7.12 (2026-08-27)

**Phase 12E.1–12E.10 — the adoption-engineering line: the embeddable
Engine facade, the format-v1 compatibility seal, golden stores, sealed
evidence manifests, versioned JSON surfaces, structured tracing, the
hard distribution-court release gate (Almalinux 10.2 / Ubuntu Server
26.04 / openSUSE Leap 16, Docker Hub images, OOM-limited), MSRV
verification, and the one-command trial path.**

- **12E.1+12E.3 — the stable embeddable engine facade + the format-v1
  compatibility seal** (`src/engine/`, `Store` RO support): a deliberately
  small public API (`Engine` / `EngineOpenOptions` / `BlobId` /
  `Durability` / `EngineMetrics` + a metric registry with name/unit/
  snapshot-vs-cumulative/scope/reset/authority per row) above the store,
  exposing content identity, exact bytes, range reads, durability,
  maintenance, metrics and typed errors. BlobId = BLAKE3 content id;
  IDs are stable across compaction / representation migration /
  encoder-policy changes and are physical-record-type-independent
  (documented in `docs/api/engine.md`). The blob namespace lives in a
  dedicated `.engine` directory with a write-then-rename put protocol
  (crash-safe: a torn put leaves the previous blob intact). Concurrency:
  many concurrent readers + writers; `close` drains and then takes the
  mount lock. **Format-v1 seal**: unknown `COMPAT` is ignored, unknown
  `RO_COMPAT` refuses writable open and permits read-only (the
  documented fallback — the old implementation refused ALL nonzero
  ro_compat; now resolved in favor of the documented contract), unknown
  `INCOMPAT` refuses open. Typed `CompatibilityError` carries format
  major/minor, the unknown bit, the mask, the required access mode and
  remediation; `StoreConfig.read_only` + `Store::open` RO enforcement.
  Fixed en route: rename-replay clobbered `extent_root` (silent zero
  reads after recovery) — regression-pinned in `write_race.rs`.
- **12E.2 — optional frontend/transport features** (`fuse` / `ublk` /
  `uring`; base = no defaults): the base library embeds the engine, the
  store, fsck, GC — with NO FUSE, NO ublk, and the reference SyncIo
  transport. `tools/check-feature-matrix.sh` compiles 6 combinations
  (default / base / base+uring / base+fuse / base+ublk / all) with
  check + test-no-run. No feature combination changes on-disk semantics.
- **12E.4 — historical golden-store compatibility court**: real
  historical binaries preserved under `testdata/golden/{v0.3.0,v0.5.2,
  v0.6.3}/` (never regenerated), plus `tools/make-golden-fixtures.sh`,
  per-fixture test drivers and `src/tests/golden_store.rs` — current
  EntropyFS must open / fsck / enumerate / materialize each fixture with
  byte-exact logical output, and the fixture hashes are pinned (a future
  decoder that cannot read a supported golden store fails CI).
- **12E.5 — sealed-evidence manifest versioning**: `EvidenceManifest`
  (schema 1: version, git revision, format major/minor, compat /
  ro_compat / incompat bits, universe versions, encoder-policy version,
  io backend, worker scheduler, kernel, arch, distro, compiler, host,
  digest, timestamp) written by `entropyfs evidence-manifest` — the
  machine-readable authority beside the human-readable
  `court-<ts>-<rev>/` archive names.
- **12E.6 — operator-grade JSON surfaces**: `entropyfs status --json`,
  `entropyfs fsck --json`, `entropyfs scrub --json`, and the new
  `entropyfs metrics [--json]` — external DTOs (not raw struct
  serialization), versioned schemas, typed fsck findings
  (`code`/`severity`/`object`/`observed`/`limit`). Metrics were
  refactored to `collect_engine_metrics(store)` with the full
  accounting surface (logical/reachable/backing/allocated bytes,
  live/dead/index-hidden/unindexed/torn/padding/format/unexplained
  bytes, model bytes, CAS/exact-ref savings, GC runs/scanned/copied/
  reclaimed, compaction write amplification, optimizer
  scanned/rewritten/bytes-saved, reference-depth histogram, epoch
  pending/checkpoint count, worker queue depth, latency accounting).
- **12E.7 — structured tracing**: `perf::trace` span! macro (optional
  `tracing` feature, default-on; no-op without a subscriber) on engine
  put/get/range/sync/compact, store open/create, durability barrier,
  epoch checkpoint, GC; truncated content ids; never payload bytes.
- **12E.8 — the portable distribution court — the hard release gate**:
  `tools/distro-court.sh` + `distro-court-inner.sh` run the 17-stage
  court (pristine minimal image → documented prereqs → rustup →
  `cargo build --release --locked` → tests → install → mkfs → Engine
  API smoke → SyncIo smoke → UringIo capability probe → FUSE mount
  where the runtime permits → POSIX smoke → unmount → `fsck --json` →
  reopen + hash verify → compact/GC → reopen + fsck) inside **OOM-
  limited Docker VMs** (`--memory 4g --memory-swap 6g`, the hard
  requirement) on **Docker Hub images** — `almalinux:10.2`,
  `ubuntu:26.04`, `opensuse/leap:16.0` — with immutable image digests
  recorded in the sealed evidence, capability waivers carrying the exact
  failed probe/command/error/requirement, and a separate
  `tools/docker/` vendor-artifact lane for registries requiring
  credentials. **All three lanes sealed PASS with zero waivers**
  (immutable digests + full logs under `evidence/portability/
  distro-court-*/`). Docs: `docs/portability/distro-court.md`,
  `docs/portability/support-matrix.md`.
- **12E.9 — MSRV + distro Rust policy**: `tools/check-msrv.sh`; declared
  MSRV 1.87 verified (check all-targets under default AND base feature
  sets + release builds); the distro's packaged Rust is NOT the
  project's MSRV (rustup toolchain is the documented install path);
  `docs/portability/msrv.md`.
- **12E.10 — one-command trial path**: `cargo install entropyfs --locked`
  → `entropyfs mkfs` → `entropyfs mount`; `src/cli/errors.rs` classifies
  every configuration failure (missing /dev/fuse, missing capability,
  unknown INCOMPAT bit, unsupported RO_COMPAT for RW, io_uring
  unavailable, ublk unavailable) with typed errors — no opaque EIO, no
  panics; `tools/trial-path.sh` PASSES all six classified probes.

Release evidence: the sealed distribution court (three distros, zero
waivers) is in `evidence/portability/distro-court-*/`; the feature
matrix and MSRV checks are reproducible via `tools/`. Remaining 12E
sub-phases (11–24: real-device transport court, small-object packing
oracle, object-store adoption court, C ABI, Go binding, policy gates,
docs, CI matrix) continue in v0.7.13+.

## v0.7.11 (2026-08-27)

**12D-0 — the grammar-addressed entropy OFFLINE oracle: the fully-accounted
template grammar beats EntropyFS settled 7.0× on the grammar-friendly
corpus but loses to zstd-whole 2.2× — STOP per the brief's gate, with the
"persisted entropy" refinement recorded.**

- **The offline oracle** (`src/tests/grammar_oracle.rs`, sealed
  `evidence/performance/grammar-oracle-*/`; no format change): a bounded
  template-grammar induction (longest common prefix/suffix → leading /
  trailing literals; the middle split on the longest common internal
  substring, capped at 8 slots; `Repeat`-compressed periodic segments)
  encodes a 200-member non-periodic shared-skeleton config corpus with
  FULL accounting (grammar once + Σ(state + descriptor) per member;
  state raw — the conservative bound). Incumbents: EntropyFS foreground,
  EntropyFS settled (+ `optimize_pass` + `shared_dict_pass`), zstd -19
  whole pack. Plus a diverse negative control.
- **Sealed (release):** grammar 66 059 B (181.9× logical) vs EntropyFS
  foreground 5 899 905 B (2.04×) and settled 465 068 B (25.8×) — the
  grammar beats the settled machinery 7.0×; the diverse control loses as
  expected (grammar 1.00× ≈ RAW vs EntropyFS 2.45×). But zstd-whole
  (29 731 B) is 2.2× smaller than the grammar — the grammar stores its
  irregular shared skeleton LITERALLY while zstd entropy-codes it.
- **The verdict: STOP per the brief's gate** (the fully-accounted
  grammar must beat EVERY incumbent; it beats the in-repo machinery but
  not zstd). The identified refinement is the brief's own "persisted
  entropy": the grammar object is itself data and must be
  entropy-coded — a rANS-coded skeleton would close the zstd gap, but
  the gate applies to the conservative raw accounting, so the format-bit
  investigation (12D-1) is not justified on this evidence. The oracle
  stays as the offline measurement surface. 436 lib tests green.

## v0.7.10 (2026-08-27)

**12C-0 — the DSFB structural-semiotics oracle: the semantic prior really
reorders the search (winner rank 4.41 → 1.02), but the standalone CPU
gain is ~3% — the gate says RECORD, and the adaptive foreground budget is
the identified mechanism that turns the ordering advantage into skipped
work.**

- **The semantic machinery** (`src/dsfb/semantics.rs`): a quantized
  [`SemanticContext`] (extension / parent / basename-shape classes from
  the name; magic / printable-ratio / entropy classes from a bounded
  4 KiB byte sketch; lifecycle) and a learned per-class channel prior
  (`P(channel | semantic class)` — the class's normalized winner
  distribution, incremented at every observe). The DSFB plan scores each
  channel `historical_trust + 0.3 × prior(class, channel)`. Strictly
  advisory: ordering and budget only, never bytes (ADR-0004); the
  winning representation remains the minimum over byte-validated
  candidates (ADR-0010). Mode-gated (`None`/`Extension`/`ByteSketch`/
  `History`/`Combined`; the production default is `None` until the
  oracle's gate is met). The write path threads the context via
  `epoch_write_semantic` and `GuidedContext::semantic`; the oracle
  diagnostics (winning-channel plan rank + RAW-winner count) are
  accumulated in `encode_guided`.
- **The oracle** (`src/tests/dsfb_semantics_probe.rs`, sealed
  `evidence/performance/dsfb-semantics-probe-*/`): a heterogeneous
  corpus (source `.rs`, config `.toml`, incompressible `.bin`, zeros,
  extensionless) PLUS the brief's semantic-deception exhibits (noise
  named `.rs`, zeros named `.bin`), written twice per mode (pass 1
  learns the prior, pass 2 measures the guided search). Rows per mode:
  search CPU, candidates/chunk, winning rank, RAW fallback, density,
  byte-exactness.
- **Sealed (release):** winner rank 4.41 (S0) → 1.02 (S1 extension),
  1.52 (S3 history), 2.88 (S2/S4) — the class evidence genuinely moves
  the likely winner first. Search CPU 36.7 → 35.7 ms (S4, −2.7%): the
  plan's budget is a channel COUNT, so reordering alone does not skip
  candidate work in the current architecture. Density (1.81), RAW
  fallback (37.5%), candidates/chunk (2.89), and byte-exactness are
  IDENTICAL across every mode — including the deception exhibits, so the
  prior never overrides the byte gate.
- **The decision: RECORD, do not wire as the production default.** The
  brief's gate requires search CPU to fall SUBSTANTIALLY with density
  unchanged; the measured ~3% is not substantial, and the reason is
  structural (the budget counts channels, not costs). The prior's class
  confidence is the prerequisite for the adaptive foreground budget
  (search effort = f(system pressure, queue depth, class confidence)) —
  the identified 12C continuation — which converts the ordering
  advantage into skipped expensive-family work. The machinery stays
  wired, mode-gated, zero-risk (byte-exact, density-identical). 435 lib
  tests green.

## v0.7.9 (2026-08-27)

**12B — durability generations + group commit: concurrent fsyncs
coalesce onto one physical barrier per generation; the convoy is gone.**

- **The model** (`docs/performance/durability-generations.md`, coordinator
  `src/store/durability.rs`): `logical_seq` (the epoch's mutation-log
  sequence) and `durable_seq` (the highest sequence through a completed
  barrier) — with a SECOND coordinate, the published root's `generation`
  (bumped by every commit, epoch and direct), because direct non-epoch
  writes never advance the epoch sequence. `fsync` may return iff the
  durable state covers its requirement; a mutation acknowledged after
  the cut is chosen never inherits the barrier (the brief's
  seq-100/101 example).
- **The coordinator**: waiters park on a condition variable; the first
  waiter when idle becomes the OWNER, fixes the cut at the componentwise
  max of the current waiters, runs the UNCHANGED physical barrier (epoch
  checkpoint → commit-lock-held fdatasync → dir sync → superblock write
  → superblock fsync, same crash hooks at every step), advances the
  durable atomics to the cut on success, stores a generation-tagged
  error on failure (each waiter surfaces only ITS generation's error —
  late arrivals retry as the next owner). The physical barrier is
  byte-for-byte the pre-12B sequence; only who runs it and who waits
  changed.
- **The oracle** (`src/tests/fsync_group_probe.rs`, sealed
  `fsync-group-probe-baseline-1787792160-91cc1ba/` + `group-*`):
  concurrent write+fsync loops at 1/2/4/8/16/32 writers. Baseline:
  amplification 1.00 at every concurrency (545 physical barriers for 545
  fsyncs at 32 writers), p99 45 µs → 7.9 ms (the convoy), commit-lock
  wait 366 ms. After: **amplification 0.23 at 32 writers (127 physical
  barriers for 545 fsyncs)**, p99 7.9 → 4.1 ms (−48%), commit-lock wait
  −96% — the convoy is gone. The median shifts up at high concurrency
  (a waiter parks for the generation cycle — the brief's explicit
  trade) while the wall still drops (94 → 80 ms at 32 writers).
- **The crash court** (`src/tests/durability_group_crash.rs`): a crash
  injected at EVERY physical-barrier stage (AfterRecordAppend,
  AfterSegmentFdatasync, AfterSegmentDirFsync, AfterSuperblockWrite,
  AfterSuperblockFsync) under 8 concurrent writers; after recovery every
  RETURNED fsync's bytes read back exactly (the brief's oracle: returned
  ⇒ recoverable; unreturned ⇒ admissible) and fsck is clean. The
  unmodified `durability`, `crash_recovery`, and `io_backend_parity`
  power-loss courts stay green. 431 lib tests green.

## v0.7.8 (2026-08-27)

**12A-0 — the Hot-DAG read-cost oracle: depth does NOT predict read
latency by itself; the terminalization daemon is REJECTED on measured
evidence (the brief's explicit "record and reject" outcome).**

- **Read-cost instrumentation** (`src/store/readcost.rs`): a bounded
  [`ReadCostSample`] per materialization (family, reference depth, max
  walked path depth, DAG nodes, fanout, referenced objects, bytes
  fetched, `read_many` submissions, model-cache hit/miss delta, decode
  CPU, I/O wait, total latency, logical bytes), carried inside
  `PreparedRead` so the two-phase FUSE read completes one coherent
  sample, closed into a 4096-ring; plus an exponentially decayed
  per-chunk-id hotness tracker (`h ← h·0.9 + 1` per touch). Both
  strictly diagnostic, never persisted, never an authority. The
  model-cache hit/miss counters are lock-free store atomics (delta
  samples; exact for sequential reads).
- **The oracle** (`src/tests/dag_read_cost_probe.rs`, sealed
  `evidence/performance/dag-read-cost-probe-1787790816-ef6508b/`): six
  controlled DAG families in isolated stores — `raw` (d0),
  `exactref` (d1, fanout 8), `base-inline` (d1–4, search-natural inline
  residuals), `base-object` (d1–4, forced rANS residuals — enc + model
  objects per level), `diamond` (fanout 3 + a d2 chain), `seqdict`
  (d1 dict references) — each constructed with the REAL encoders and
  committed with §32 validation + byte-exact read-back verification, the
  committed family histograms asserted before measuring. Random 64 KiB
  reads at cold/warm/hot cache states → p50/p95/p99 + the sample
  aggregates.
- **The verdict.** Depth predicts latency ONLY through object/decode
  width: `base-object` d4/d1 p99 ~3.3× (referenced objects 3 → 12,
  decode 116 → 408 µs — the penalty IS the width), while
  `base-inline` d4/d1 ~1.35×, `exactref`/`diamond` flat in fanout, and
  cold-vs-hot barely moves the decode-dominated terms. The natural
  machinery already prices the costly shape (rebase flattens at depth 2;
  `λ_depth` penalizes depth in candidate cost), so a terminalization
  daemon keyed on depth would fix a rare ~1.35× artifact at real
  complexity. **REJECTED** — recorded in the sealed results.json; the
  instrumentation stays as the 12B/12C measurement surface. 429 lib
  tests green.

## v0.7.7 (2026-08-27)

**11F — the sharded DSFB observer: the last process-wide write-path mutex
is gone, and the oracle quantifies what the 11D brief predicted (and what
it did not).**

Phase 11 closes with this step. The pre-11F observer was per-`ChunkKey`
state behind ONE store-level mutex: `observe`/`plan`/`trust` on unrelated
files serialized because DSFB wanted to update advisory evidence. 11F
replaces it with `ShardedStorageObserver` (`src/dsfb/observer.rs`):

- **16 shards, one lock per shard** (`DSFB_SHARDS`), chosen by a stable
  FNV-1a hash of the 48 key bytes (fully specified, portable — not
  `DefaultHasher`, whose output is not guaranteed stable across Rust
  versions). `observe`/`plan`/`trust`/`forget` lock exactly one shard;
  unrelated keys never block each other.
- **Lock-free aggregate statistics**: `tracked` (exact live count — every
  insert/remove updates it under the same shard lock that mutates the
  map), `steps`, `drift_events`, `slew_events`, `narrowed_searches`. The
  store holds the observer directly — no outer mutex.
- **Cap gate without a global mutex**: `Store::dsfb_observe` reads the
  exact atomic count and, past `DSFB_MAX_CHUNKS`, evicts from the shard
  that just grew (targeted, deterministic given the key; total stays ≤
  cap + 1). Eviction is approximate and correctness-neutral (ADR-0004).
- **Probe instrumentation** (diagnostics, never behaviors): `dsfb_plan` /
  `dsfb_trust` / `dsfb_observe` global perf rows around the observer
  calls (`perf::time`: global-only, so the request-envelope
  reconciliation partition is untouched), and a `candidates_evaluated`
  atomic summed at the two `encode_guided` call sites.

**The 11F oracle** (`src/tests/dsfb_shard_probe.rs`, sealed
`evidence/performance/dsfb-shard-probe-mutex-1787789207-f103248/` and
`dsfb-shard-probe-sharded-*`): the SAME probe binary-shape at the 11F-0
commit (single-mutex observer) and at this release (sharded), pool-16,
writers 1/8/16 x 64 files plus a 4× scale probe (16 writers x 256 files =
16k chunks, 1 GiB), per-write-distinct content, byte-exact read-back +
checkpoint + logical-identity + reachable-bytes + family histogram.

| 16-writer row | mutex | sharded |
| --- | ---: | ---: |
| wall | 769–781 ms | 775–779 ms |
| p50 / p99 | 45.6–47.7 / 74.5–78.1 ms | 46.6–47.0 / 75.5–79.8 ms |
| useful CPU | 10.42–10.47 s | 10.47–10.48 s |
| dsfb plan call wall | 3.3–20.1 ms | 2.1–2.2 ms |
| stress (4×) dsfb plan | 34–38 ms | 10.5–11.5 ms |
| stress (4×) wall | 3156–3162 ms | 3144–3151 ms |

Byte identity exact, logical committed bytes == logical input exactly,
reachable bytes identical, candidates identical (4096 / 16384),
representation families identical (RAW only — the LCG corpus), on every
run of both sides.

**The verdict is a recorded falsification with an adoption.** The 11D
brief predicted the observer mutex would become visible as independent
requests advanced through search simultaneously under the 11E pool. The
oracle shows the prediction was true IN THE OBSERVER ROWS but not
end-to-end: the plan call (the largest critical section, a 9-element
sort under the lock) lost ~66% of its wall under 16-way concurrency
(34–38 ms → 10.5–11.5 ms at the 4× scale), but all observer calls
together are ~1 µs each — 0.1% of `prepare` even at 4× scale — so wall,
p50/p99, and useful CPU are unchanged within run-to-run noise (±1%).

The shard is adopted anyway, on grounds the oracle does not contradict:
(1) architecture — the observer's state is per-key, so its locking
should be per-key; the write path is now synchronization-free end to end
except the commit coordinator and per-inode locks (real shared state,
not advisory evidence); (2) future-proofing — Phase-12C (DSFB structural
semiotics) deepens the per-call work, which would make a single mutex
matter; (3) zero measured regression in any row. 428 lib tests green
(11F-0 probe + 8 observer tests incl. an exact-count-under-concurrency
test and a shard-spread test; the `court_repro` 24-thread stress test
still shows its known load flake — passes alone, noted in the test
ledger).

Also: the DSFB call timing rows and candidate counter land as permanent
write-path diagnostics (the 12C oracle will read them), and the
mounted-court 11E1 data-loss regression pins (`src/tests/write_race.rs`)
remain green.

## v0.7.6 (2026-08-27)

**11E1 — the mounted-FUSE court sealed the worker pool as the MOUNT
DEFAULT, and the court exposed a real write-path data-loss bug that is
now fixed.**

- **The mounted-FUSE 11E court** (`tools/court-worker-pool-mount.sh`,
  sealed `evidence/performance/worker-pool-mount-court-1787786369-*`):
  semaphore / pool-8 / pool-16 at FUSE session threads 1/4/8/16 against a
  13-workload battery (serial cp/dd controls, parallel writes/reads,
  per-op latency drivers, namespace ops, tree copies, untar, make -j,
  the bindgen cargo build, mixed readers+writers, fsync-heavy), with
  byte-exact readback + fsck per cell. At 16 FUSE threads, pool-16 vs
  the semaphore: parallel write +14%, latency-battery wall −26%, p95
  −39%, p99 −48%, CPU +2.8%, serial controls neutral — and crash/fsck/
  readback CLEAN at every cell. The brief's five gates all pass
  (parallel neutral-or-better, p95/p99 materially improved, serial not
  materially regressed, CPU bounded, cleanliness clean). **The FUSE
  mount now enables the pool by default** (`available_parallelism()`
  workers; `--worker-pool N` sizes it explicitly; `--no-worker-pool`
  restores the 11C semaphore as the fallback).
- **The data-loss bug the court found (and why the court exists).** The
  untar workload's readback failed: parallel tar extractions lost
  ~10-45% of small files' EXTENTS — the inode size survived but the
  committed extent tree was empty (silent zero reads; fsck-clean
  because the binding was internally consistent). Root cause, in three
  parts, each fixed and regression-pinned in
  `src/tests/write_race.rs` (new, +2 lib tests):
  1. **Stale-root checkpoint commit.** The epoch never rebuilds
     extent/directory trees — the checkpoint does, and only for the
     files/dirs whose extents/entries are in ITS frozen snapshot. A
     pending inode re-staged by a concurrent op (a write's block-B
     re-read, a setattr) carries a stale (usually ZERO) root and could
     survive the compare-and-remove; the next checkpoint committed it,
     orphaning the committed tree. Fixed in `epoch_checkpoint` step
     3.5: never commit a data root this checkpoint did not rebuild —
     resolve it from the committed inode.
  2. **Replay applied log-staged inodes wholesale.** The recovery
     replay's `Setattr` arm (and the `Unlink` child path) put the
     log-staged inode into the tree verbatim — including its stale
     ZERO root — wiping the tree a preceding write-replay had just
     built (tar's fchmod/futimens after each write made this nearly
     deterministic). Fixed: replay applies the attribute fields to the
     tx's current inode, preserving the committed data root, exactly
     like the `Write` arm already did.
  3. **The getxattr checkpoint storm (the amplifier).** `get_xattr` /
     `list_xattr` flushed the epoch on every call; the kernel probes
     security.capability / ACL xattrs on every file creation, so a
     parallel untar fired hundreds of full checkpoints, each widening
     the race window (and costing real throughput). Fixed: xattr reads
     are committed-side reads (xattrs are committed immediately) with
     an overlay existence check only.
- The store-level write path was already correct: a direct concurrent
  `epoch_write` + reopen+replay round-trip is byte-exact (the corruption
  required the checkpoint/replay interplay the court's untar workload
  exercises). 423 lib tests green; the mount court's readback+fsck
  cleanliness now passes at every cell.

## v0.7.5 (2026-08-26)

**Ultra-verbose commentary doctrine — applied repository-wide.** The
implementation is research evidence; the commentary standard
(`docs/architecture/commentary-standard.md`) is now enforced across the
entire source tree (~59 400 lines, every module):

- Every substantial module carries the full template — PURPOSE, BOUNDARY,
  MODEL, PERSISTENT AUTHORITY, CORRECTNESS INVARIANTS, CONCURRENCY,
  DURABILITY, RESOURCE BOUNDS, PERFORMANCE, FAILURE MODES, HISTORY /
  EVIDENCE — and every architecturally significant function answers
  what / why / how / guarantees (inputs-and-authority, algorithm,
  invariants, concurrency, durability, resource bounds, failure behavior,
  evidence).
- Long systems functions are stage-numbered so a future engineer can
  navigate a 500-line state machine without reverse-engineering it.
- Every evidence-sensitive choice in the doctrine's §7 list now carries
  its causal history and sealed measurement in place: the writeback-cache
  removal (Phase-10G), mutation-log sequence monotonicity, checkpoint
  compare-and-remove (not `mem::take`), staged-payload resolution, the
  exclusive read-window bound and conditional predecessor inclusion
  (Phase-11C regression), physical-scan occupancy (Phase-9H 2.66 MB
  dead-BtreeNode finding), chunk-index `bulk_load`, DSFB zero decoding
  authority, anchor survival through GC reference closure, longest-path
  reference depth (Phase-10E), rANS model-cost-in-selection (Phase-9G0
  277.6→74.3 KB), random-data→RAW convergence, the background optimizer's
  CAS-against-incumbent gate, io_uring unsafe isolation, the durability
  barrier's commit-lock hold (the 11B/11C fsync convoy), decode-validates-
  before-allocation (Phase-11A layering gap), and semantic hostile-media
  mutation recomputing CRCs so hostile payloads reach the deep parsers.
- The last three files were completed in this pass: `core/materialize.rs`
  (the bounded materializer — validate-before-allocation + the
  budget/depth/allocation counters in place), `optimizer/search.rs` (the
  guided search — the Phase-10B/10C/11C/11E seams and the DSFB authority
  separation), and `tests/hostile_media/graph_court.rs` (the
  materialization-graph court — the no-CRC contrast with the store court
  and the Phase-10E/10G graph-bomb history).
- Stale comments that contradicted the code were corrected to describe
  the code that exists now (doctrine §9) while their historical rationale
  stays in the changelog/evidence.

Comment-only pass: `cargo check --all-targets` clean (pre-existing
warnings only), full 421-test lib suite green, no measurement or on-disk
behavior changed — no evidence updates. Released as 0.7.5 (patch;
docs-only, no behavior change).

## v0.7.4 (2026-08-26)

**11E — the persistent fair worker pool (probe-sealed, KEPT).** The 11D
decision called for a narrow latency-fairness experiment; this is exactly
that, and the probe sealed it as KEPT (`src/store/workers.rs` `SearchPool`,
probe `src/tests/worker_pool_probe.rs`, evidence
`evidence/performance/worker-pool-probe-1787769464-8fdea62/`, doc
`docs/performance/worker-pool-probe.md`):

- **Typed tasks only** — `EncodeChunk` / `DecodeExtent` carrying
  `(request_id, ordinal)` and their owned payloads; no generic executor,
  no async, no work stealing. Results reassemble strictly by ordinal:
  execution order may vary, persisted semantic order may not (byte-exact
  read-back verified on every probe run).
- **Per-request queues, round-robin, one task per pick.** The probe found
  the shared-cursor round-robin silently pins each request to one worker
  when workers == active requests; per-worker cursors (worker i starts at
  ring index i) keep consecutive picks on different requests.
- **Bounded queue with backpressure at submission** (8 x workers). The
  probe found the naive wait deadlocks when an oversized request (a
  64-extent read decode) meets an idle pool — a request is always
  admitted when nothing is in flight.
- **Per-store opt-in** (`enable_worker_pool` + `--worker-pool N`): only
  opted-in stores take the pool path; the FUSE daemon keeps the 11C
  semaphore by default until the mounted-FUSE court validates the pool
  end-to-end.
- **Sealed gates (release, 16 writers, pool-16 vs semaphore):** wall
  0.79–0.80 vs 1.08–1.28 s (−29% — the batch-transition slack the 11D
  floor analysis missed), p50 47–48 vs 49–60 ms, p99 78–85 vs 152–241 ms
  (−68%), p99/p50 1.63 vs 3.88, max request slowdown 18× vs 47× — at
  +2.6–3.7% useful CPU (straddles the +3% gate, below the +5% reject
  bar; the DSFB-mutex visibility the 11D brief predicted — the 11F
  observer shard is the identified follow-up). 8 writers: wall −34%,
  p99 −69%, CPU +3.7–6.6%. pool-8: same wall, −20% CPU, −59% p99 (the
  lower-power alternative); pool-4: control. Two real probe-found
  defects (the cursor pinning and the backpressure deadlock) are fixed
  and regression-pinned in the mechanism test.

Also: **documentation role separation** — the README's phase table moved
into this changelog as the *Development phase ledger* (below); README now
is the stable front door (current state + links). The commentary doctrine
is codified at `docs/architecture/commentary-standard.md`. 421 lib tests
green.

## v0.7.3 (2026-08-26)

**11D — the worker-pool decision oracle.** The 11C semaphore's `prepare`
bucket is bounded but opaque; before building a fair worker pool, the
oracle (`src/tests/worker_oracle.rs`, sealed at
`evidence/performance/worker-oracle-1787765041-052bc46/`;
`docs/performance/worker-oracle.md`) decomposes it at 1/2/4/8/16 writers
(the search/decode workers now report true thread-CPU time via
`CLOCK_THREAD_CPUTIME`):

- `worker_queue_wait` — the grant acquisition (Gate A: semaphore queue).
- `worker_scope_wall` — the scoped-thread scope duration (Gate B).
- `worker_useful_cpu` — per-worker thread-CPU time via
  `CLOCK_THREAD_CPUTIME` (rustix `time` feature; wall fallback), summed
  across parallel workers (Gate C).
- Workload-validity probes in the search (dedup-hit fraction, decisive
  early-exit fraction) that the test asserts are zero.

The first oracle run caught its own methodology bug: one store across
 the sweep let a mid-run checkpoint feed the committed chunk index, and
 the 16-thread row measured an EXACT_REF dedup cache (search 11.2 s →
 0.21 s, `dedup_hit_frac=1.0`) instead of search CPU. Fixed with fresh
 stores + per-write-distinct content per thread count (the 11C court's
 “never share a page” rule).

Sealed result: **search CPU is constant 9.8–10.0 s at every thread
count** (the semaphore wastes no CPU), **queue wait grows 4.6% → 91.7%
of `prepare`** (Gate A fires — the batch-granularity head-of-line
blocking the 11D brief predicted), **16-thread wall 1.14 s ≈ the
SMT-adjusted CPU floor** (9.8 s / 8 physical cores), and **p50 5.3 →
52.4 ms / p99 9.5 → 177.6 ms** (tail latency is the only real pool
headroom). Decision: the pool is justified ONLY as a latency-fairness
probe — it must beat p50/p99 at 8/16 T without increasing search CPU —
and is rejected if it merely reproduces the 1.14 s floor with more
code. 419 lib tests green.

## v0.7.2 (2026-08-26)

**11C — the three 11B levers, attacked.** The reconciliation court
identified the write plateau's components; 11C implements all three and
re-seals with the same identity (residual ≤ 3.2% mounted, ≤ 1.8%
direct-store; `docs/performance/reconciliation.md` §3.4, sealed court
`evidence/performance/recon-court-1787762195-49f1a55/`):

1. **The remaining epoch-guard holds (60–81% of 8–16-thread request
time).** The chunk prefill is split into a guard-held PREPARE half
extent collection, dependency enumeration, and the batched object fetch
with the reference closure's nested descriptors captured into the
prepared read) and a pure-CPU DECODE half that runs with the epoch guard
RELEASED — both `epoch_write` and the FUSE read handler are two-phase, so
no materialization runs under the epoch mutex. The per-write
checkpoint-threshold check reads a lock-free pending-op mirror
(`epoch_pending`, maintained under the guard, read without it), removing
the `epoch_wait` acquisition. Direct-store: `epoch_lock_wait + epoch_wait`
80.8% → ~0.3% at 16 threads; walls 1.22/1.12/1.13/1.13/1.59 s — the
plateau is a flat line at the CPU-bound floor.

2. **Worker oversubscription.** A process-wide worker SEMAPHORE
(`src/store/workers.rs`) caps total search/decode threads at
`available_parallelism()`. The non-blocking “grant 0 → run inline”
fallback was measured and REJECTED: the unlucky requests' serial searches
thrashed the workers' cores (search wall-sum grew ~5× at 16 threads);
the semaphore parks requesting threads instead, so the search CPU is
bounded at every thread count.

3. **The fsync convoy is contract-inherent** — the barrier's
`[fdatasync → superblock fsync]` window must hold the commit lock or a
mid-barrier commit would ack after the fsync started but before its cut
(write→fsync durability linearizability, pinned by the crash courts). It
shrank indirectly as writes stopped stalling behind it (mounted
`commit_lock_wait` 34.7% → 16.4% at 16 threads).

**Two read-window defects the instrumentation exposed** (regression-tested):
the pending-extent range used an inclusive upper bound (collecting the
adjacent pending extent at the window's end → a spurious multi-extent
decode per prefill read of a rewritten file), and the
pending-predecessor scan-start extension pulled in the previous chunk
for chunk-aligned reads. The bound is now exclusive; the extension is
gated on the predecessor actually covering the read offset.

The mounted court at 16 threads: epoch locks 4.3% → 0.2%, `read_decode`
1.6% → 0.7%, `commit_lock_wait` 34.7% → 16.4%. Full 417-test suite
(crash courts, hostile-media court, concurrency) green.

## v0.7.1 (2026-08-26)

**11B — write-path request reconciliation.** The performance equivalent of
Phase 9H's physical byte reconciliation, applied to latency: every write
and fsync request is partitioned into exclusive phases and the identity
`request latency == Σ phases + residual` is asserted per thread count
(spec in `docs/performance/reconciliation.md`, court in
`src/tests/perf_reconciled.rs`, sealed mounted court under
`evidence/performance/recon-court-1787757073-e5b0592/` via
`tools/recon-court.sh`).

**The finding:** the 4→16-thread write plateau is the EPOCH MUTEX convoy,
not the commit coordinator. `commit_lock_wait` is ~zero at every thread
count; `epoch_lock_wait + epoch_wait` reached 94% of request time at 16
threads, because `epoch_write` held the epoch guard across candidate
preparation. This is the measured answer to the 10G question (parallel
writes flatten 375.8 → 543.1 → 558.1 MB/s) — the write-side
serialization resource is the epoch mutex, found by accounting, not
inferred.

**The fix:** `epoch_write` releases the epoch guard across `prepare`
(candidate search is pure CPU + committed reads; its inputs are the
pre-filled overlay bytes) and re-acquires it only for the overlay prefill
and the staging, with the file size re-read at staging as a monotonicity
guard. Same-inode writers were already serialized by the per-inode
mutation lock, and a checkpoint can only merge this thread's own earlier
pending writes. Direct-store A/B (release, 256×1 MiB epoch writes): the
guard convoy collapses from ~50–75% of request time at 2–4 threads to
~1–29%; wall 4T 1.28 → 0.94 s. The full 415-test suite — crash courts,
hostile-media court, concurrency suites — stays green.

**Instrumentation:** `src/perf/mod.rs` gains the request ledger
(`Timings::request` envelope, `Timings::time_request` exclusive leaf
rows, `Timings::detach` for internal helper reads, `reconcile`/
`render_reconciled` stacked table with the explicit residual and an
OVERLAP flag when a row double-counts). The daemon's `--stats-file` dump
carries the table automatically. The reconciliation holds at every thread
count: residual ≤ 2.4% direct-store, ≤ 4.0% mounted.

**Next terms (11C levers), named by the accounting:** the remaining
eventual guard holds at 8–16 threads (60–81% `epoch_lock_wait` +
`epoch_wait`); per-request `available_parallelism()` worker oversub-
scription (`read_decode` 5–22%, inflating exactly where the plateau
flattens); and the mounted durability-path fsync convoy (`commit_lock_wait`
29.8% at 16 threads, `cp`'s trailing fsyncs queueing on the commit lock).

## v0.7.0 (2026-08-26)

**11A — hostile-media court.** The security documentation claimed fuzz
assurance that the repository did not implement; this release closes the
gap with the persistent-data adversarial suite (`src/tests/hostile_media/`,
spec in `docs/security/hostile-media-court.md`, sealed evidence under
`evidence/hostile-media/court-1787750784-a2983dc/`). The backing store is
treated as untrusted/corrupt input; the oracle is uniform — bounded-valid
result OR typed rejection, never panic/OOM/unbounded CPU, never bytes
inconsistent with the authenticated content identity:

1. **Descriptor court** (`descriptor_court.rs`): every bounded byte
   string through `format::descriptor::decode` under deliberately tight
   limits AND the defaults. decode-OK implies `validate` OK, encoded size
   within the descriptor cap, and a byte-exact canonical re-encode.
   Corpus: one real descriptor of every representation family (all 17 +
   every residual kind) truncated at every byte boundary, the 8192/8193
   descriptor-cap boundary, and every rank/count/ordering violation the
   format defines.

2. **Materialization-graph court** (`graph_court.rs`): a fuzz-defined
   descriptor table + object table + entry descriptor materialized
   through an in-memory hostile resolver that mirrors the store's
   `DecoderContext`. Valid seeds pin the exact materialized bytes;
   structural bombs (self-reference, cycles, depth bombs, chains at
   exactly 4/5, diamonds, shared-dict double branches, invalid dictionary
   chains, corrupted models, hostile command streams) must terminate
   boundedly — the budget/depth/allocation counters are what this court
   proves.

3. **Store court** (`store_court.rs`): the CRC-aware distinction —
   physical corruption (broken envelope → integrity rejection) vs
   semantic adversarial mutation (envelope CRC + content id recomputed so
   the hostile payload reaches the deep parsers), over real tiny stores
   with a whole-store mutator (flip / truncate / splice / duplicate /
   reorder / alter lengths / replace tags / replace payloads / recompute
   CRC selectively) driving open/fsck/materialize. Dedicated exhibits:
   B-tree fanout exactly 4096/4097, unsorted and duplicate keys, a
   valid-CRC envelope containing a malicious (self-referential)
   descriptor, and mutation-log duplicate / non-monotonic sequences.

**Layering fix the court required:** `format::descriptor::decode` now
 takes the full `&Limits` and passes every decoded representation through
 `Representation::validate` before returning — the read path never hands
 an unvalidated descriptor to the materializer, matching the write path's
 gate (`put_chunk_in_tx`). The on-disk format is unchanged; the parser is
 stricter about accepting, never about encoding. This is the invariant
 the security documentation always described, now actually enforced and
 fuzz-proven.

Also: `docs/security/resource-bounds.md` §6 is corrected to describe the
implemented court (it previously claimed fuzz targets that did not
 exist); `docs/security/threat-model.md` documents the two-court
 CRC-aware distinction; ADR-0016's fuzzing section records the decision
 to run the hostile-media court as an in-package proptest-driven harness
 rather than a `fuzz/` Cargo package (ADR-0001: one package);
 `tools/hostile-court.sh` is the evidence-sealing runner.

Sealed: the court runs 200k descriptor cases + 200k graph cases + 30k
store-mutator cases per proptest target in release mode, plus the full
lib suite (428 tests), all green; evidence receipt at the revision above.

## v0.6.3 (2026-08-26)

**10G — parallel-workload hardening.** The writeback-native architecture
(10D epochs + 10E range reads + 10F io_uring transport) re-run under
GENUINELY PARALLEL workloads (`tools/court-threads-parallel.sh`: concurrent
`cp`/`cmp`, multi-thread namespace loops, `make -j` with the target on the
mount). The sweep exposed five real bugs — all fixed, all regression-pinned
on both io backends (385 lib tests green):

1. **Epoch envelope sequence monotonicity.** `Epoch::envelope()` assigned
   each op's `MutationLog` sequence at STAGE time, and the checkpoint
   `mem::take`'d the epoch and restarted the counter at 0 — a post-
   checkpoint op could receive a small seq <= an earlier `log_seq` and be
   silently DROPPED at recovery (its overlay was never checkpointed), and
   two epochs could emit envelopes sharing a sequence (the recovery
   "duplicate mutation log sequence" invariant). The sequence counter is
   now globally monotonic (never reset); the inode high-water mark is
   carried the same way (a reset could hand out a duplicate ino — two
   files, one ino).

2. **Checkpoint snapshot redesign.** The checkpoint SNAPSHOTS the overlay
   under the commit lock (the live epoch keeps its state, so concurrent
   epoch ops never see the old empty-overlay + stale-committed gap that
   produced spurious "inode missing" EIOs), merges the snapshot, and
   compare-and-removes exactly the snapshot's entries only after a
   successful commit (a failed commit leaves the overlay intact). An
   overlapping checkpoint can no longer merge a stale snapshot onto a
   newer tree (the observed corruption: a file's committed size regressed
   while its tail extents remained — fsck "extent ends beyond file size").
   `log_seq` is `max`'d with the pre-commit root for monotonicity.

3. **Overlay read-path staleness.** `read_file_epoch` used the PENDING
   inode's `extent_root` — stale once a checkpoint published a newer tree
   — so reads missed newly-committed extents (holes at chunk boundaries,
   cached by the kernel). The read now walks the CURRENT committed
   inode's `extent_root` (overlaid with the pending extents) and extends
   the scan window to the PENDING predecessor. `dir_lookup_epoch` /
   `read_dir_epoch` got the same committed-`dir_root` fix (the parallel
   namespace court hit "no such entry" on epoch-merged entries).

4. **In-flight staged objects.** An overlay read resolving a PENDING
   descriptor could fetch objects that were not yet appended (they live in
   the op's local records until `epoch_append`), producing silent zeros
   via the decode. The epoch now retains the staged object PAYLOADS
   (`staged_payloads`, cleared at the checkpoint) and the read paths
   resolve through them.

5. **FUSE writeback-cache removal.** In writeback mode the kernel flushes
   dirty pages asynchronously and can interleave READ requests between a
   file's write requests; the epoch overlay is only complete once every
   write is staged, so such reads returned partial extents the kernel then
   cached (mount corruption at chunk boundaries). The daemon no longer
   negotiates `FUSE_WRITEBACK_CACHE` — write-through makes each `write()`
   wait for the daemon's ack, so reads always observe fully-staged
   writes; aggregation is preserved via `max_write`. The FUSE read
   handler also serializes with the file's in-flight writes.

Also: the epoch-write chunk-index SELF-ALIAS guard (the dedup path's
`EXACT_REF{target: cid}` must never be registered as the pending entry
for its own cid — it would let the checkpoint clobber the retained
terminal with a self-loop, `DepthExceeded` on concurrent identical-content
writes), and the parallel-workload sweep tool + regression tests
(`epoch_self_alias`, `epoch_seq_monotonic`, `court_repro`,
`partial_window_read`, `split_write`, `namespace_repro`).

Sealed: the full court (`court-threads-parallel-1787745479/`) runs clean
at 1/2/4/8/16 threads; the FUSE max-request-concurrency column is
GENUINELY >1 for the parallel workloads (reads 351→1044 MB/s, namespace
ops 200→3200 ops/s, writes ~350→550 MB/s) — the Phase-10A serial-`cp`
concurrency ceiling (~1) is falsified for parallel workloads; `make -j`
is flat (~11–12 s, compile-bound).

## v0.6.2 (2026-08-26)

**10F — io_uring storage transport (`IoBackend`, ADR-0021):** all file
mutations and payload reads now go through a transport seam below
`Store` / transactions / epoch checkpoint:

```text
Store / transactions / epoch checkpoint
                 │
                 ▼
              IoBackend
             /         \
        SyncIo           UringIo
     reference path    performance path
```

- **`SyncIo`** is the pre-10F synchronous engine, preserved byte-for-byte
  as the crash-consistency oracle (the default; `--io-backend sync`).
- **`UringIo`** implements the exact same record format and durability
  ordering with the syscalls issued through one io_uring ring
  (`--io-backend uring`, `--io-uring-entries N`; the `io-uring` crate was
  already a transitive dependency — no new dependency tree). It is the
  crate's ONE `unsafe` file-adjacent module: the SQE push lives in
  `platform/io_uring.rs` with a ledger entry
  (`docs/security/unsafe-ledger.md`) and a walk-the-src enforcement test;
  every other module keeps `#![forbid(unsafe_code)]`.

**Crash-court parity is the acceptance test:** the full crash matrix and a
full-workload sequence run against BOTH backends, and the parity harness
(`tests/io_backend_parity.rs`) asserts the store directories are
canonically byte-identical at every injection point (inode wall-clock
times are canonicalized; every other byte — record structure, order,
lengths, superblock, layout — is compared verbatim). The crash and
durability courts are parameterized over both backends. A `UringIo`
implementation that produced a different recoverable state at any crash
point would fail the suite.

**`read_many`** — the architectural payoff: a materialization's
model/stream/dictionary dependencies are enumerated statically from the
extent descriptors (nested base/target/dictionary refs resolved through
the chunk index, depth-capped) and fetched in ONE submission queue, then
decoded in parallel (scoped threads). The extent scan became a LEVEL-ORDER
batched walk (one `read_many` per tree level, sibling node fetches batch)
and the transaction prune walk batches per level too — the tree-node
fetches the ring would otherwise do one at a time.

The write-path hunt surfaced a real algorithmic bug (both backends):
`apply_sorted_batch` recursed into every child slot and fetched every
node even for a tiny batch, so the epoch checkpoint's 1-2-entry chunk
patch walked the ENTIRE chunk index — O(tree) per commit, the dominant
write-path floor. Fixed with an empty-batch short-circuit + empty-slice
skip: `cp_chunk_apply` p50 1262 → 22 µs (sync) and 2281 → 30 µs (uring),
and the mounted 4K-dsync court improved on both backends.

**Sealed 10F court pair (tmpfs-backed; relative comparison):**

| metric | 10F-sync | 10F-uring |
|---|---|---|
| 4K dsync writes | 1.9 MiB/s | 1.8 MiB/s (−5%) |
| 4K buffered writes | 189.5 MiB/s | 139.0 MiB/s (−27%) |
| 1M writes | 301.1 MiB/s | 244.4 MiB/s (−19%) |
| warm sequential read | 2219.8 MiB/s | 1959.7 MiB/s (−12%) |
| 1M read latency p50 | 1108 µs | 1190 µs (−7%) |

Measured ring economics: ~2.3 µs per submit-and-wait cycle (the kernel's
submit+wait+wake floor on this system) vs ~0.1 µs per `pread` on tmpfs,
amortizing to ~0.34 µs/op at a 32-op batch. The batching closes most of
the read gap; the residual write gap is the ring floor on sub-µs tmpfs
I/O. The default stays `sync` (the oracle) until real-device (NVMe,
queue-depth) evidence flips it; the seam is where that evidence will
land.

Also: `tools/perf-court.sh` gained `--io-backend`; `capabilities` probes
and reports the io_uring transport; the durability barrier and the epoch
checkpoint gained sub-phase timings (`barrier_*`, `cp_*`) for the
write-path diagnostics.

## v0.6.1 (2026-08-26)

**10E1 — lock-free fd-cache reads:** the 10E `segment_payload` held the
`segment_fds` map mutex across the whole `pread` loop, so concurrent
object reads serialized on the map lock even though `pread` has no shared
file position. The cache now stores `Arc<File>`: acquire the map lock,
clone the `Arc`, drop the lock, then `pread` — object reads execute
concurrently. Sealed by the 10E1 mounted-court A/B pair
(`evidence/performance/fuse-court-*-10e1-before` / `fuse-court-*-10e1-after`,
tmpfs-backed): the court did not move — 1M-read latency p50/p95/p99
1062/1197/1277 → 1085/1200/1301 µs, warm seq read 2238 → 2226 MiB/s,
both within noise — falsifying a serial-workload win cheaply, exactly as
predicted. The lock removal is retained regardless: it takes a real
serialization point out of the read hot path with no measured regression,
and the `Arc<File>` shape is the one 10F's `read_many` parallel decode
needs, where concurrent `pread`s are the entire point.

## v0.6.0 (2026-08-26)

**10A — performance instrumentation + court thread sweep:** `src/perf`
phase timings (`Timings`, FUSE op stats, write-size buckets, in-flight
counters) and a mountable court thread sweep (`--threads 1/2/4/8/16`
runs with full context capture). Finding: `cp` exposed a maximum
request concurrency of ~1, so additional FUSE worker threads were
useless then — the measurement predates the 10D namespace-latency
collapse and is being re-run with parallel workloads (make -j, parallel
untar, Git checkout) after 10E/10F.

**10B — ruthless foreground selection:** `ForegroundPolicy` (full/cheap/raw-
only) + `--foreground` mount flag. The high-entropy probe (anti-aliased
min-over-three-strides sampled entropy) skips the LZ/entropy families for
obviously-incompressible chunks. Direct store: random 64 MiB 39.8 → 852
MiB/s (21×), daemon CPU −37% through FUSE, settled density unchanged at
1.994× (the background optimizer recovers everything the cheap foreground
defers).

**10C — parallel chunk preparation:** a multi-chunk write's candidate
search runs concurrently (scoped threads; single-chunk writes inline),
with byte-identical results to the serial path (synthetic in-batch
dictionary validation + a serial real-state backstop). Mounted court:
random full 66.5 → 148.8 MiB/s buffered (2.2×), compressed.tgz durable
4.7 → 26.9 (5.7×); settled density byte-for-byte 1.994× in all four runs.

**10D — metadata writeback epochs:** namespace/writeback ops accumulate in
an ACTIVE EPOCH — each op appends its staged objects + a `MUTATION_LOG`
envelope (the recoverable dirty state) and acks after the page-cache
flush; checkpoints merge the frozen overlay into the trees ONCE
(bulk-load for per-directory trees, `apply_sorted_batch` bulk COW for the
global indexes) with one root publication. Recovery replays envelopes
with `seq > root.log_seq`. Mounted src-workload: create p50 2.5 ms →
8.5 µs, setattr 2.4 ms → 4.1 µs (~300× per namespace op); the 135-file
source-tree copy drops from ~0.7 s to 0.045 s. On-disk: format v1
retained; a new incompat feature bit (15, MUTATION_LOG), a new record
tag (0x07), and a trailing `root.log_seq` field (absent in pre-epoch
roots) are additive extensions an old implementation must refuse.

**10E — segment read-fd cache + range-traversal read paths (no format
change, no new feature bit):** the read path used to open the segment
file afresh for every object fetch and descend the extent tree
chunk-by-chunk per read. Now: one read-only fd per segment (append-only
while mounted, so a cached fd never goes stale) with offset-based
`pread` (no shared seek position; thread-safe), and one `scan_range`
traversal per file read (`read_file` and the epoch overlay path) instead
of a per-chunk `covering` descent.

**Reference-depth invariant fixes (correctness; surfaced by the real-tree
tests, not by the read-path change):**

- The depth walks (`chain_depth` / `chain_depth_uncapped`) undercounted
diamond-shaped reference DAGs — a SEQUENCE_SHARED_DICT whose dictionary
chain and shared chain converge — because a first-reached-wins visited
set blocked deeper paths through an already-visited chunk. The depth gate
could then admit a descriptor whose true chain exceeds
`max_reference_depth`, leaving the file unreadable (`DepthExceeded`).
The walks now record the DEEPEST depth at which each node was explored.
- A background pass could replace a chunk-index entry (content id →
descriptor) with a DEEPER descriptor after an earlier extent had already
been validated against the shallower entry — references resolve through
the index at materialize time, so the earlier extent became unreadable.
A post-pass convergence sweep (`Store::rebase_overdepth_extents`, run at
the end of `optimize_pass` / `shared_dict_pass` / `model_bundle_pass`)
rebases any extent whose chain now exceeds the decode cap to a depth-0
encoding, §32-gated (relaxed decode budget recovers the bytes).

Regression-tested (`chain_depth_reports_deepest_path_through_a_diamond`);
full suite 365 lib + 1 bin tests green; real-tree convergence, remount,
fsck and crash courts all green. The mounted FUSE court for 10E was
pending at release and is sealed as 10E1 in v0.6.1.

## Unreleased

## v0.5.2 (2026-08-25)

**9H — physical convergence (GC + compaction; no format change, no new
feature bit):**

- **Physical scanner** (`store/physical.rs`): reconciles every segment
  byte independent of the derived object index — live / dead-indexed /
  index-hidden / unindexed / torn / padding / format — with the invariant
  `file_bytes = Σ categories` (unexplained must be 0).
- **Root cause found by measurement**: on the real-tree court the
  post-GC dead bytes (2.66 MB) were `BtreeNode` records staged by the GC
  chunk-index REBUILD, which rebuilt the tree with repeated COW inserts
  — every intermediate path version was physically written and indexed.
  The index-hidden-duplicate hypothesis was falsified (index-hidden = 0).
- **GC victim selection** now uses the scanned PHYSICAL occupancy
  (`physical_ratios`): segments whose disk bytes are dominated by garbage
  are compacted even when the index's one-location view calls them live.
- **`index::bulk_load`**: the chunk-index rebuild bulk-loads the tree
  bottom-up from sorted reachable entries, staging each FINAL node
  exactly once (no COW intermediates).
- **`entropyfs gc --compact`** (`compact_full`): every segment is a
  victim; backing converges to reachable + bounded format overhead;
  idempotent. The current root record is not re-copied (snapshot roots
  are Root records too and still are).
- Sealed (`campaign-1787688017-0a03ece/`, 7/7 admission): tree court
  backing **9,129,988 → 1,100,161 B**; post-GC reconciliation = reachable
  1,100,157 B + 0 B dead + 0 B index-hidden + 0 B unindexed + 4 B format
  overhead; full compaction = reachable + 4 B (0.00% of logical); second
  compaction reclaims 0. GC-traffic H2 store: unreachable-after
  2,274,864 → 201,033 B. The 2.88× representation win is now a real
  ~2.9× filesystem capacity win. Regressions: real-tree convergence +
  idempotence + snapshot-root preservation; crash courts + fsck green.

## v0.5.1 (2026-08-25)

**9G0 + 9G — amortized entropy models (codec fix + background pass; no
format change, no new feature bit):**

- **9G0 — model-cost-aware stream selection** (codec fix, sealed
  `campaign-1787684918-80e36c8/`): the stream-level RAW/rANS gate now
  includes the persisted model bytes (`enc + model < raw`), so a stream
  whose rANS gain is smaller than its model is stored RAW. The biggest
  single measured win since 9F: on the real source tree the sequence
  families' model objects drop 277.6 KB → 74.3 KB — tree court per-extent
  overhead 26.5% → 11.1% of footprint; tree court 2.388× → 2.775× post
  shared-dict; src corpus 4.327×.
- **Model-sharing oracle** (diagnostic, `tests/model_oracle.rs`):
  exhaustive intra-extent set-partition model sharing is FALSIFIED
  (−125 KB); one aggregate model per stream type per directory cohort is
  validated (+49.6 KB); bundle pools of 2/4 lose to the single aggregate.
  The oracle initially decoded SequenceRans offsets with `off_per_copy =
  1` instead of 2 — a diagnostic bug corrected before any implementation
  decision (sealed campaign numbers do not use the oracle).
- **9G — `model_bundle_pass`** (sealed `campaign-1787685723-60ecaf2/`):
  the background pass trains one aggregate model per stream type per
  directory cohort and re-encodes each member's streams against it
  (per-stream RAW fallback), rewriting only when the cohort's total
  persisted bytes strictly fall, through the same CAS + byte-exact (§32)
  gate as every other background pass. **No format change**: model objects
  are content-addressed; a descriptor already references them by ChunkId;
  CAS amortizes one cohort model object across N extents. Tree court:
  shared-dict 2.813× → **2.881×** (65 rewrites; 7,486 B cohort-accounted;
  the real post-GC reachable saving is 25.7 KiB because GC reclaims the
  superseded per-extent models). The win is better-trained aggregate
  models (enc side); unique model bytes stay flat — recorded as-is.
  Accounting invariant: a member's incumbent pinned bytes are descriptor +
  enc object only (models are amortized and never claimed as removable by
  one member). Correctness battery: byte-exact, idempotent, remount+fsck
  clean, noise cohort never rewritten.
- Wired into the tree court (`efs_model_*` fields), `entropyfs optimize`,
  and the background idle worker.

**Earlier in this release — Phase-9F evidence (no codec change, no format
change):** the gap decomposition is sealed into the tree court. Measured
on the real source tree (280 files, per-file writes):

- zstd -1 per-file 3.57×; zstd -1 per-file **with the same per-directory
  anchor** (`-D`, self-matches excluded) 3.91× — a mature coder extracts
  only ~8.5% from the shared-dictionary concept, less than EntropyFS's
  pool + deep pass already recovers (2.22× → 2.39×). **The anchor policy
  is not the cap.**
- The residual gap to per-file zstd is **~2/3 per-extent persistence
  overhead** — per-chunk multi-stream rANS model objects + descriptors on
  small files: 309.7 KB = 26.5% of the EntropyFS footprint (11.1% of
  logical; models alone 275.9 KB) — and **~1/3 coder quality**.
- Falsified: `scale_bits` does not shrink model objects (the model
  encoding is symbol-count-dominated: sb14 367 B vs sb8 295 B on a 3 KB
  file).

This refocuses the engineering target: **9G = amortized/shared entropy
models** (model objects are content-addressed and immutable, so N extents
can reference ONE persisted model object with no decoding chain), not
more anchor tuning or parser deepening.

## v0.5.0 (2026-08-25)

**Format note:** v0.5.0 retains **format version v1** (no format-version
bump) and adds one new **incompat representation feature** as an explicit
on-disk feature bit — `SEQUENCE_DEEP` (bit 14). An implementation that
does not understand that bit must refuse the store. Additive,
feature-gated, same pattern as v0.2.0 (bits 10/11), v0.3.0 (bit 12), and
v0.4.0 (bit 13). **Correction:** the superblock feature-bit tracker now
also records `SEQUENCE_SHARED_DICT` (bit 13) — the v0.4.0 write path
committed the descriptors but the tracker missed the bit, so an old
tool could open such a store and only fail at descriptor decode instead
of at the incompat gate. The tracker is now exhaustive over the
sequence families and regression-tested (`tests/shared_dict.rs`,
`tests/seqdeep.rs`).

Milestone content (Phase 9D + 9E):

- Phase 9D — the anchor POOL: `shared_dict_pass` now selects up to four
  per-directory anchors greedily by marginal savings against member
  incumbents, and each extent picks its best pool anchor during the
  rewrite. Heterogeneous directories (mixed styles/content classes) get
  per-file dictionary choice; the pool beats the single anchor by ~2×
  savings on a two-cluster directory fixture.
- Phase 9E — `SEQUENCE_DEEP` (tag 0x11, feature bit 14): repcodes
  (REP0/REP1 copies carry no offset symbol) + extended length codes (one
  XCOPY/XLIT plus a u16 extra instead of runs of 131-byte continuation
  commands) fed by a deep background matcher (hash chains to depth 256,
  lazy parsing with a minimum-gain threshold, rep-distance priority).
  Background-only; terminal (depth 0).
- Ablation: `allow_sequence_rans_deep` gate, `no-deep` leave-one-out
  mode, cumulative-ladder step E4 (post-registration extension),
  `raw_sequence_deep()` standalone baseline (foreground write + a
  background pass, since the family is background-only).
- Evidence (sealed campaign, 7/7 admission): tree court on the real
  source tree — EntropyFS per-file writes **2.194× → 2.354× post-GC**
  after the shared-dict pool + deep background pass (151 extents, ~93.3
  KiB saved); standalone deep floor **3.786× vs the fast floor 3.736×**
  on the src pack (deep wins all chunks); ladder E4 densifies the
  structured corpus 50,528 → 50,238 B. Archived under
  `evidence/performance/` (`INDEX.md` is authoritative).

## v0.4.0 (2026-08-25)

**Format note:** v0.4.0 retains **format version v1** (no format-version
bump) and adds one new **incompat representation feature** as an explicit
on-disk feature bit — `SEQUENCE_SHARED_DICT` (bit 13). An implementation
that does not understand that bit must refuse the store. Additive,
feature-gated, same pattern as v0.2.0 (bits 10/11) and v0.3.0 (bit 12).

Milestone content (Phase 9C):

- Phase 9C — the shared amortized dictionary: `SEQUENCE_SHARED_DICT` (tag
  0x10, feature bit 13). Local history + optional previous same-file chunk
  + a *shared cross-file dictionary* in one stream, with a third
  copy-source symbol (`SRC_SHARED`, absolute offset). The background
  `shared_dict_pass` selects a per-directory anchor — an existing terminal
  chunk that maximizes savings against member incumbents (not against raw
  bytes, which under-measured the fix by 12×) — and rewrites extents
  strictly-cheaper with the same CAS-gated, byte-validated commit path as
  `optimize_pass`. Anchors are terminal (v1), so rewritten extents carry
  depth ≤ 1; GC pins the anchor chunk through the reference closure even
  after its owning file is deleted (regression-tested).
- The 9C evidence gate is sealed in the campaign's **tree court**: 279/282
  real-tree files are single-chunk, so the previous-chunk dictionary gets
  almost no opportunity on a real tree, and the packed-stream density is
  mostly cross-FILE structure. Per-file zstd baselines: whole 4.978× /
  per-file 3.541× / per-64KiB 3.991× (-1). EntropyFS per-file writes
  **2.182× → 2.328× post-GC** after the shared-dict pass (102 extents,
  ~85.2 KiB saved). The mechanism is proven by synthetic fixtures
  (random-looking shared headers → large wins); the modest real-tree gain
  is recorded as-is.
- Ablation: `allow_shared_dict` gate, `no-shared-dict` leave-one-out mode,
  cumulative-ladder step E3 (post-registration extension), DSFB channel
  P8 (`shared_dict`).
- Evidence: the sealed Phase-9C campaign `campaign-1787679299-8d6e147`
  (7/7 admission) under `evidence/performance/` (`INDEX.md` is
  authoritative); the two intermediate tree-court measurements (flat
  placement; RAW-scored anchors) are amended in `INDEX.md`, never
  silently kept.

## v0.3.0 (2026-08-25)

**Format note:** v0.3.0 retains **format version v1** (no format-version
bump) and adds one new **incompat representation feature** as an explicit
on-disk feature bit — `SEQUENCE_DICT` (bit 12). An implementation that
does not understand that bit must refuse the store (the superblock's
feature-bit gate, `docs/format/compatibility.md`). This follows the same
additive, feature-gated pattern as the v0.2.0 correction
(`SEQUENCE_RANS` bit 10, `SPARSE_BLOCK64` bit 11).

Milestone content (Phase 9A + 9B):

- Phase 9A — the incompressible physical floor: `Tx::commit_deferred`
  prunes transaction-local COW intermediates (B-tree nodes and inode
  objects unreachable from the final root) before append; ENOSPC guard on
  the pruned footprint; `unreachable_bytes_by_record_tag` evidence.
  urandom reaches 0.997× reachable / 1.00× total backing / 1.00×
  allocated blocks.
- Phase 9B — `SEQUENCE_DICT` (tag 0x0F, feature bit 12): cross-chunk
  dictionary match coding. The previous same-file chunk is used as an
  external ≤64 KiB dictionary alongside local history: a fourth
  *copy-source* stream says whether each u16 is a LOCAL backward distance
  (byte-progressive) or a DICT absolute offset; a DICT match longer than
  131 bytes advances the offset across continuation commands. Reference
  depth is accounted like a base chain (`dictionary chain + 1 ≤
  max_reference_depth`), so cross-chunk dictionary references can never
  defeat bounded random access; periodic terminal anchors emerge
  automatically at the depth cap.
- Write-path integration: the batch overlay provides the previous chunk's
  bytes nearly free (`PendingBatch.depths` registers in-batch reference
  depths); the background optimizer re-encodes RAW extents to
  SEQUENCE_DICT via the committed previous chunk.
- Correctness fixes surfaced by SequenceDict:
  - `flatten_if_deep` now validates the flattened update through a
    resolver that sees the update's own staged objects (previously
    materializing through the bare store failed on object-backed
    families with `MissingObject`).
  - `current_persisted_bytes` now accounts every object a descriptor
    references (RAW/RANS were the only families counted; object-backed
    incumbents looked nearly free and blocked densification).
  - Background candidate ordering now uses FULL persisted bytes (the
    foreground keeps marginal bytes so reuse wins); a chunk whose
    incumbent's objects already exist is no longer immune to
    replacement.
- Ablation: `allow_sequence_dict` gate, `no-sequence-dict` leave-one-out
  mode, cumulative-ladder step E2 (post-registration extension).
- Evidence: `campaign-1787674068-4892644` (9A floor) and
  `campaign-1787676607-8250f6b` (9B, 7/7 admission — src corpus 4.070×
  with SequenceDict vs standalone SequenceRans 3.627× and
  zstd-per-64KiB -1 3.848×). Archived under `evidence/performance/`
  (`INDEX.md` is authoritative).

## v0.2.0 (2026-08-25)

**Format note (correction to the release commit's wording):** the v0.2.0
release commit stated "no on-disk format changes." That was imprecise:
v0.2.0 retains **format version v1** (no format-version bump) but adds new
**incompat representation features** as explicit on-disk feature bits —
`SEQUENCE_RANS` (bit 10) and `SPARSE_BLOCK64` (bit 11). An implementation
that does not understand those bits must refuse the store (the
superblock's feature-bit gate, `docs/format/compatibility.md`). This is an
additive, feature-gated on-disk format *extension*, not a layout rewrite;
the correction is recorded here rather than rewriting the commit.

Milestone content (Phase 8, M1–M5 + 8A/8B/8C):

- Concurrency refactor: interior-mutability `Store` (root/superblock
  behind `RwLock`, 64-shard object index, per-inode lock table, short
  commit coordinator); reads traverse root snapshots without the global
  writer lock; FUSE writeback-cache negotiation.
- Write aggregation: `write_region_batch` group commit with in-batch
  overlay; deferred durability with fsync barrier; in-batch dedup
  visibility; transaction-local CAS canonicalization (one physical record
  per content id per transaction); marginal-cost candidate selection
  (existing objects cost zero).
- `SEQUENCE_RANS` (tag 0x0D, feature bit 10): the local-match +
  entropy compression floor (LZ77-style hash-chain matcher with three
  rANS-coded or raw streams).
- `BASE_SEQUENCE` residual (kind 0x04 inside `BASE_RESIDUAL`): shift-aware
  copy/literal deltas for versioned data.
- `SPARSE_BLOCK64` (tag 0x0E, feature bit 11): blockwise-64 enumerative
  sparse coding.
- Derived chunk-index rebuild in GC: the chunk index is pruned to the
  reachable set (live extents + transitive reference closure) so
  overwritten unsnapshotted content cannot grow it permanently.
- Evidence: strict cumulative ablation ladder A0–A8 (+ post-registration
  E1 SequenceRans step) beside the leave-one-out table; attribution split
  into CAS object sharing (store invariant) vs EXACT_REF aliasing (gated
  representation); per-corpus post-GC physical footprint (reachable /
  total backing / allocated blocks); the zstd-per-64KiB diagnostic.
  Archived under `evidence/performance/` (`INDEX.md` is authoritative).

## v0.1.0 (2026-08-25)

Initial publication: single-crate native-Rust crash-consistent FUSE
filesystem (`entropyfs`). Format v1, representation set 0x01–0x0C
(ZERO/FILL/RAW/RANS/EXACT_REF/BASE_RESIDUAL/SPARSE/PALETTE/PERIODIC/
ENTROPY_REF/INLINE/PERMUTATION), dual-superblock commit protocol,
reachability GC, snapshots, fsck, crash courts.
---

# Development phase ledger

This ledger records the complete research and implementation progression of
EntropyFS. It intentionally preserves failed experiments, superseded
measurements, protocol corrections, and falsified hypotheses rather than
rewriting history after later improvements. The release entries above tell
what changed and when; this table is the phase-by-phase map with each
phase's scope, status, and sealed evidence. **README describes the present
system; CHANGELOG preserves its temporal history; evidence proves measured
claims; ADRs explain architectural decisions.**

| Phase | Scope | Status |
|-------|-------|--------|
| 0 | Research, ADRs, information-theory boundary, format v1, crash protocol | ✅ sealed (`docs/`) |
| 1 | In-memory representation engine: RAW/ZERO/FILL/INLINE/RANS/EXACT_REF/BASE_RESIDUAL/SPARSE/PALETTE/PERIODIC/PERMUTATION/ENTROPY_REF, cost accounting, round trips | ✅ implemented (tests green) |
| 2 | Persistent immutable store: segments, dual superblocks, records, descriptor codec, feature bits, fsck, crash courts, ENOSPC | ✅ sealed (fsck-verified, crash-court matrix) |
| 3 | Mountable FUSE filesystem: mkfs/mount/unmount, full POSIX battery (cp/diff/rsync/git clone/cargo build/untar/truncate/rename/hardlink/symlink/xattr/fsync), kernel-cache invalidation, fsck-clean | ✅ sealed (live-mount verified) |
| 4 | Entropy-native optimization: DSFB-guided search (P0-P5 channels, trust-ordered budget), exact dedup, base+residual with rebase-on-write, background optimizer (CAS-protected, resumable) + idle daemon worker, ablation benchmarks | ✅ implemented (ablation fixture + campaign `evidence/performance/campaign-1787658658-67d977a/`) |
| 5 | Snapshots, GC, robustness: snapshot create/list/delete/restore (live verified), GC pins snapshot roots, chunk-index reachability fix (deleted data reclaimable), near-full GC recovery from the emergency reserve, shrink-write extent fix, snapshot crash-court matrix | ✅ implemented (live + fsck verified) |
| 6 | Performance: deferred durability (logical commit + fsync barrier), search fast path, oversized-descriptor validation fix (SIGBUS root cause), multi-threaded FUSE. Sealed before/after FUSE court pair (`evidence/performance/INDEX.md`): 1M writes 185→653 MiB/s, 4K buffered 0.6→24.4 MiB/s, bindgen cold build FAILED (SIGSEGV/SIGBUS) → 9.5 s; fsync p50 320→1647 µs (deferred-durability tradeoff, reported honestly) | ✅ implemented (evidence-sealed) |
| 7 | Experimental ublk frontend: `src/ublk/` over the same engine — BlockStore adapter (4K blocks, read/write/flush/discard via the entropy engine, device = hidden store file), libublk target glue + `ublk run` CLI (root + `ublk_drv` required), `ublk bench` (kernel-free), unit tests, ADR-0020 | ✅ implemented (adapter live-verified; kernel binding needs root) |
| 8 (M1) | Concurrency refactor: `Store` interior mutability (root/superblock behind `RwLock`, 64-shard object index, per-inode lock table, short commit coordinator), reads traverse root snapshots without the global writer lock; FUSE writeback-cache negotiation (`FUSE_WRITEBACK_CACHE | ASYNC_READ | PARALLEL_DIROPS | BIG_WRITES`, 1 MiB max_write, background queues) | ✅ implemented (`d90772c`, 264→278 tests) |
| 8 (M2) | Write aggregation: `write_region_batch` group commit (one transaction + generation per batch, in-batch overlay for overlapping partial chunks), deferred durability, live 4K writes 24.4 → 319 MiB/s (13×), 1M writes 653 MiB/s, reads 2212 MiB/s | ✅ implemented (live court) |
| 8 (M3) | **SequenceRans** — the general-purpose compression floor: bounded LZ77 hash-chain matcher + three rANS-coded (or raw) streams over `ryg-rans-rs` (tag 0x0D, feature bit 10). Fixes two real defects found by the H2 campaign: encoder tail-remainder bug (`0x7F` corruption for 1–3-byte copy tails) and the flatten-on-write §32 validation gap; also fixed the store GC reachability walk (it under-counted SequenceRans objects — a withdrawn campaign caught it). src corpus: pure byte rANS 1.633× → SequenceRans **3.556×** (zstd -1 per-64KiB 3.739× — the per-extent floor is within 5%, the gap to whole-file zstd is cross-chunk context); urandom still 0.997× | ✅ implemented (evidence-sealed `campaign-1787671040-923df7b/`; the earlier “at parity with direct rANS” description conflated the pre-split gate and is amended in `INDEX.md`) |
| 8 (M4) | **BaseSequence** — shift-aware copy/literal delta residuals (residual kind 0x04 inside BASE_RESIDUAL): `COPY(base_offset, len)` / `LITERAL(run)` commands, three-stream rANS/raw codec shared with SequenceRans. Inserted/deleted regions cost only their own bytes. H2 flips back to **+35.2%** (sequential 2.752× vs shuffled 1.784×); the shuffled control grows because deltas also capture structural similarity — recorded as the finding | ✅ implemented (evidence-sealed `campaign-1787666036-43bf17e/`) |
| 8 (M5) | **SparseBlock64** — blockwise-64 enumerative sparse coding (tag 0x0E, feature bit 11): per-word popcount + `C(64,k)` rank (fits u64) + literals, three-stream rANS/raw codec. Removes the plain-SPARSE `u128` cliff (`10 ≤ k ≤ n−10` at 64 KiB). The campaign caught a 3× write-throughput regression from missing dense-input pre-gating; a `k ≥ n/2` density gate fixed it (regression-tested) | ✅ implemented (evidence-sealed `campaign-1787666589-e895fcf/`) |
| 8 (8A) | Evidence-protocol correction: the strict cumulative ladder A0–A8 (each step adds one mechanism, A8 = +background pass) now runs beside the leave-one-out table (spec §43, methodology §4); both are kept forever. The first campaign's nine-row table is amended as the leave-one-out table (protocol note, never rewritten) | ✅ implemented + evidence-sealed (`campaign-1787668526-d04227f/`) |
| 8 (8B) | Derived chunk-index rebuild: GC rebuilds the chunk index to exactly the reachable set (live extents + transitive reference closure), so overwritten unsnapshotted content cannot grow it permanently. H2 post-GC permanent footprint: sequential full 1,528,175 → **1,366,816 B** (10.6% pruned); regression-tested invariant `chunk_index_entries ≤ reachable + closure`, repeated GC never regrows the index, remount + fsck clean | ✅ implemented + evidence-sealed (`campaign-1787668526-d04227f/`) |
| 8 (8C) | Attribution correction + transaction-local CAS canonicalization: `allow_exact_ref` gates only the EXACT_REF alias representation (content-addressed object sharing is a store invariant, separately accounted: `cas_shared_bytes_saved` vs `exact_ref_bytes_saved`); `allow_rans` split into byte rANS (A1, pure again) + SequenceRans (E1, post-registration); duplicate records are never re-appended (one record per content id per transaction); duplicate chunks short-circuit to the canonical descriptor or alias, marginally cheapest (existing objects cost zero); post-GC footprint evidence (reachable/total backing/allocated blocks). Structured: E1 50,528 B (1,328×), post-GC allocated 61,440 B = **1,092×** (was 5.1 MB pre-GC backing); zstd-per-64K diagnostic: SequenceRans within 5% of zstd-per-64K ⇒ cross-chunk context is the next lever (SequenceDict) | ✅ implemented + evidence-sealed (`campaign-1787671040-923df7b/`) |
| 8 (8H) | Competitive filesystem court: `tools/fs-court.sh` measures the same corpora across ext4, zstd -1/-3/-19, and mounted EntropyFS; XFS/Btrfs±zstd/EROFS/SquashFS recorded as explicit waivers with the exact root-capable-VM commands. First run `fs-court-1787669946-b165d60`: EntropyFS effective density 1.488× incl. a 64 MiB incompressible control; zeros 453/4374 MiB/s write/read, random 85/3532 MiB/s, fsck clean | ✅ tooling + first run sealed (VM run clears the loop-mount waivers) |
| 9 (9A) | Physical floor: transaction-local COW-intermediate pruning — the incompressible backing floor collapses to ~1.00× (urandom reachable 33,652,515 / total backing 33,658,070 / allocated 33,665,024 B); `unreachable_bytes_by_record_tag` evidence identifies the pruned record class; ENOSPC guard on the pruned footprint | ✅ implemented + evidence-sealed (`campaign-1787674068-4892644/`) |
| 9 (9B) | **SequenceDict** — cross-chunk dictionary match coding (tag 0x0F, feature bit 12): the previous same-file chunk as an external ≤64 KiB dictionary beside local history, with a fourth copy-source stream (LOCAL backward distance vs DICT absolute offset; DICT continuation advances the offset). Reference depth accounted like a base chain (`dictionary chain + 1 ≤ max_reference_depth`), so cross-chunk references can never defeat bounded random access; terminal anchors emerge automatically at the depth cap. src corpus **4.070×** — beats standalone SequenceRans (3.627×) and zstd-per-64KiB -1 (3.848×). Also fixed three latent defects it surfaced: `flatten_if_deep` staged-object resolution (`MissingObject`), `current_persisted_bytes` object accounting (object-backed incumbents looked free), background full-byte candidate ordering | ✅ implemented + evidence-sealed (`campaign-1787676607-8250f6b/`) |
| 9 (9C) | **SequenceSharedDict** — shared amortized dictionary match coding (tag 0x10, feature bit 13): local history + optional previous same-file chunk + a **shared cross-file dictionary** in one stream (third copy-source symbol `SRC_SHARED`). The background `shared_dict_pass` picks a per-directory anchor — an existing terminal chunk that maximizes savings against member incumbents — and rewrites strictly-cheaper extents through the same CAS-gated, byte-validated commit path. GC pins the anchor through the reference closure (survives owner deletion). Sealed by the campaign's **tree court**: 279/282 real-tree files are single-chunk (previous-chunk dictionaries get ~no opportunity on a real tree — the packed-stream density is cross-FILE structure); per-file writes **2.182× → 2.328× post-GC** (102 extents, ~85.2 KiB saved) vs zstd per-file 3.541× / per-64KiB 3.991× (-1). The modest real-text gain and the strong synthetic-family mechanism are both recorded as-is | ✅ implemented + evidence-sealed (`campaign-1787679299-8d6e147/`) |
| 9 (9D) | **Anchor pool**: `shared_dict_pass` selects up to four per-directory anchors greedily by marginal savings against member incumbents; each extent picks its best pool anchor during the rewrite. Heterogeneous directories get per-file dictionary choice (pool saves ~2× the single-anchor on a two-cluster fixture) | ✅ implemented (sealed with 9E) |
| 9 (9E) | **SequenceDeep** — deep-match family (tag 0x11, feature bit 14): repcodes (REP0/REP1 carry no offset symbol) + extended length codes (one XCOPY/XLIT + u16 extra instead of 131-byte continuation runs), fed by a deep background matcher (chain 256, lazy parse with a minimum-gain threshold, rep-distance priority). Background-only; terminal. Standalone deep floor **3.786× vs fast 3.736×** on the src pack (deep wins all chunks); ladder E4 densifies structured 50,528 → 50,238 B | ✅ implemented + evidence-sealed (`campaign-1787681660-9be6bd3/`) |
| 9 (9F) | **Gap decomposition sealed** — the remaining gap to per-file zstd is measured, not asserted: zstd with the same per-directory anchor (`-D`, self-excluded) gains only ~8.5% (the anchor policy is NOT the cap); the residual gap is ~2/3 **per-extent persistence overhead** (multi-stream rANS models + descriptors on small files, ~26.5% of footprint) and ~1/3 coder quality. Also falsified: `scale_bits` does not shrink models (symbol-count-dominated encoding). Sets the 9G direction: amortized model sharing | ✅ sealed (`campaign-1787683904-da26c75/`) |
| 9 (9G0) | **Model-cost-aware stream selection** — the stream-level RAW/rANS gate now includes the persisted model bytes (`enc + model < raw`), so a stream whose rANS gain is smaller than its model is stored RAW. The biggest single measured win since 9F: sequence model objects on the real tree 277.6 KB → 74.3 KB (per-extent overhead 26.5% → 11.1% of footprint); tree court 2.388× → 2.775× post shared-dict; src corpus 4.327×. Plus the model-sharing **oracle** (diagnostic): intra-extent partition sharing falsified (−125 KB), directory aggregate bundle validated (+49.6 KB), pools lose to the single aggregate | ✅ implemented + evidence-sealed (`campaign-1787684918-80e36c8/`) |
| 9 (9G) | **Amortized entropy models** — `model_bundle_pass` (background): one aggregate model per stream type per directory cohort, trained on the cohort's summed histograms; each member's streams are re-encoded against it (per-stream RAW fallback) and rewritten only when the cohort's total persisted bytes strictly fall. **No format change**: the model object is content-addressed, the descriptor references it by ChunkId, CAS amortizes it. The oracle's S2 is implemented; S1 (intra-extent bundle format) and S3/S4 (pools) are rejected on measured evidence. Tree court: shared-dict 2.813× → **2.881×** (25.7 KiB real post-GC reachable reduction); byte-exact, idempotent, fsck-clean, noise control never rewritten | ✅ implemented + evidence-sealed (`campaign-1787685723-60ecaf2/`) |
| 9 (9H) | **Physical convergence** — the derived index can diverge from what is actually on disk. The physical scanner reconciles every segment byte (`live / dead-indexed / index-hidden / unindexed / torn / padding / format`), GC victim selection uses scanned physical occupancy, the chunk-index rebuild bulk-loads the tree staging each final node exactly once (the old COW rebuild physically wrote every intermediate — 2.66 MB of dead BtreeNodes on the real tree), and `entropyfs gc --compact` converges the backing to reachable + bounded overhead (idempotent). Tree court: backing **9.13 MB → 1.10 MB**; post-GC = reachable + 0 B dead + 4 B format; full compact = reachable + 4 B (0.00% of logical). The 2.88× representation win is now a real **~2.9× filesystem capacity win** | ✅ implemented + evidence-sealed (`campaign-1787688017-0a03ece/`) |
| 10 (10A) | Performance instrumentation (`src/perf` phase timings + FUSE op stats) + mountable court thread sweep | ✅ implemented + evidence-sealed (`38f5d40`) |
| 10 (10B) | **ForegroundPolicy** — ruthless cheap foreground selection: high-entropy probe skips the LZ/entropy families for incompressible chunks; the background optimizer recovers everything the cheap foreground defers (random direct-store writes 39.8 → 852 MiB/s, 21×) | ✅ implemented + evidence-sealed (`8062f2d` / `d38f73f`) |
| 10 (10C) | **Parallel chunk preparation** — a multi-chunk write's candidate search runs concurrently (scoped threads; single-chunk writes inline), byte-identical to the serial path; mounted court random full 66.5 → 148.8 MiB/s, compressed.tgz 4.7 → 26.9 MiB/s | ✅ implemented + evidence-sealed (`3ca9d93` / `5a5f2f3`) |
| 10 (10D) | **Metadata writeback epochs** — namespace/writeback ops accumulate in an ACTIVE EPOCH (each op appends staged objects + a `MUTATION_LOG` envelope and acks after the page-cache flush; checkpoints merge the frozen overlay once with a single root publication; recovery replays `seq > root.log_seq`). Mounted src-workload create p50 2.5 ms → 8.5 µs, setattr 2.4 ms → 4.1 µs (~300× per namespace op) | ✅ implemented + evidence-sealed (`b345640` / `d2fe894`) |
| 10 (10E) | **Segment read-fd cache + range-traversal read paths** — one read-only fd per segment with offset-based `pread` (no shared seek position) and a single `scan_range` traversal per read instead of per-chunk descents. Plus the reference-depth invariant fixes the real-tree tests surfaced: the depth walks now follow the DEEPEST path through diamond-shaped reference DAGs (dict chain + shared chain converging), and a post-pass convergence sweep rebases any extent whose chain a chunk-index replacement pushed past the decode cap (unreadable files became possible) | ✅ implemented (tests green: 365 lib + 1 bin; mounted court sealed as 10E1) |
| 10 (10E1) | **Lock-free fd-cache reads** — the 10E `segment_fds` map mutex was held across the whole `pread` loop, serializing concurrent object reads; the cache now stores `Arc<File>`, so the lock is dropped before any `pread` (object reads execute concurrently). Mounted-court A/B (tmpfs): read latencies did not move (1M-read p50 1062 → 1085 µs — noise), falsifying a serial-workload win cheaply; retained because it removes a real serialization point with no regression and is the shape 10F `read_many` needs | ✅ implemented + A/B sealed (`fuse-court-*-10e1-before/after`) |
| 10 (10F) | **io_uring storage transport (`IoBackend`, ADR-0021)** — all file mutations and payload reads go through a transport seam: `SyncIo` (the reference engine, byte-for-byte the crash-consistency oracle, default) or `UringIo` (the same record format + durability ordering over one io_uring ring, opt-in `--io-backend uring`). The crash courts run against BOTH backends; the parity harness asserts the store directories are canonically byte-identical at every crash point. `read_many` fetches a materialization's model/stream/dictionary dependencies in ONE submission with parallel decode; the extent scan and the transaction prune walk batch per tree level. The write-path hunt surfaced a real algorithmic bug: `apply_sorted_batch` walked the ENTIRE tree per tiny batch (O(tree) per checkpoint) — fixed (empty-batch short-circuit + empty-slice skip), a 50×+ win on both backends. The 10F-sync/10F-uring court pair (tmpfs): uring trails by 5–27% (reads −7–12%) — the measured ring floor (~2.3 µs/cycle, ~0.34 µs/op at depth 32) on sub-µs tmpfs I/O; the default stays sync until real-device evidence flips it | ✅ implemented + crash-court parity sealed (`io_backend_parity`; `fuse-court-*-10f-sync/uring`) |
| 10 (10G) | **Parallel-workload hardening** — the writeback-native architecture (10D epochs + 10E reads + 10F transport) re-run under genuinely PARALLEL workloads (`tools/court-threads-parallel.sh`: concurrent cp/cmp, multi-thread namespace loops, make -j on the mount). The sweep exposed five real bugs, all fixed and regression-pinned: (1) the epoch envelope sequence counter was not globally monotonic across checkpoints (post-checkpoint ops could be silently dropped at recovery or share sequences — the recovery duplicate invariant); (2) the checkpoint `mem::take`'d the overlay and merged for a while, so concurrent epoch ops saw an EMPTY overlay + STALE committed root mid-commit (spurious "inode missing" EIOs), and overlapping checkpoints could merge a stale snapshot onto a newer tree (size regressed while tail extents stayed — fsck "extent ends beyond file size"); (3) the inode high-water mark reset at checkpoint could hand out a duplicate ino (two files, one ino); (4) overlay reads used the PENDING inode's extent_root/dir_root (stale once a checkpoint published a newer tree — holes at chunk boundaries and "no such entry") and the scan window missed the pending predecessor; pending objects were unfetchable before their op's append landed; (5) the FUSE writeback cache let the kernel interleave reads between a file's write requests, returning partial extents the kernel then cached (write-through mode restores read-your-writes). The parallel court now runs clean 1/2/4/8/16 threads and the FUSE max-request-concurrency is genuinely >1 for parallel workloads (reads 351→1044 MB/s, namespace ops 200→3200 ops/s) — the Phase-10A serial-cp concurrency ceiling is falsified | ✅ implemented + evidence-sealed (`court-threads-parallel-1787745479/`; 385 lib tests green, both io backends) |
| 11 (11A) | **Hostile-media court** — the persistent-data adversarial suite (`src/tests/hostile_media/`, spec `docs/security/hostile-media-court.md`): the backing store is treated as untrusted input, and the court proves the oracle (bounded-valid result OR typed rejection; never panic/OOM/unbounded CPU/wrong bytes) across (1) the **descriptor codec** (every bounded byte string under tight + default limits; decode-OK implies validate-OK and a byte-exact canonical re-encode — `decode` now takes `&Limits` and validates internally, closing the read-path layering gap), (2) the **materialization graph** (fuzz-defined descriptor table + object table + entry descriptor through an in-memory hostile resolver; valid seeds pin exact bytes; self-reference/cycles/depth bombs/diamonds must terminate boundedly), and (3) the **store** with the CRC-aware distinction — physical corruption (broken envelope → integrity rejection) vs semantic adversarial mutation (envelope CRC + content id recomputed so hostile payloads reach the deep parsers) — plus a whole-store mutator (flip/truncate/splice/duplicate/reorder/alter lengths/replace tags/replace payloads/recompute CRC selectively) driving open/fsck/materialize, and the authenticated-bytes clause checked through the opened store's own view. Dedicated exhibits: B-tree fanout 4096/4097, unsorted/duplicate keys, a valid-CRC envelope containing a malicious descriptor, mutation-log duplicate/non-monotonic sequences | ✅ implemented + evidence-sealed (`evidence/hostile-media/court-1787750784-a2983dc/`; 200k/200k/30k fuzz cases + full suite green; 428 lib tests) |
| 11 (11B) | **Write-path request reconciliation** — the performance equivalent of 9H's byte reconciliation applied to latency (`docs/performance/reconciliation.md`, `tools/recon-court.sh`): every write/fsync request is partitioned into exclusive phases and the identity `request latency == Σ phases + residual` is asserted per thread count (no overlap, residual ≤ 4% at 1/2/4/8/16 threads, both direct-store and mounted). **Finding:** the 4→16-thread write plateau is the EPOCH MUTEX convoy, not the commit coordinator (`commit_lock_wait ≈ 0` everywhere; `epoch_lock_wait + epoch_wait` 94% at 16 threads). **Fix:** `epoch_write` releases the epoch guard across candidate preparation (pure CPU + committed reads; inputs are the pre-filled overlay bytes) and re-acquires only for the overlay prefill and the staging, with the size re-read at staging; the guard convoy collapses ~50–75% → ~1–29% at 2–4 threads (direct-store A/B, release) and the full 415-test suite stays green. Next terms named by the accounting: the remaining guard holds at 8–16 threads, per-request `available_parallelism()` worker oversubscription, and the mounted durability-path fsync convoy (`commit_lock_wait` 29.8% at 16 threads) | ✅ implemented + evidence-sealed (`recon-court-1787757073-e5b0592/`; 415 lib tests green) |
| 11 (11C) | **The three 11B levers** (`docs/performance/reconciliation.md` §3.4): (1) the prefill is two-phase — guard-held PREPARE (extent collection + dependency enumeration + batched object fetch, nested descriptors captured) + pure-CPU DECODE with the epoch guard released (`epoch_write` AND the FUSE read handler); the checkpoint-threshold check reads a lock-free pending-op mirror — direct-store `epoch_lock_wait + epoch_wait` 80.8% → ~0.3% at 16 threads, walls flat at the CPU-bound floor. (2) a process-wide worker SEMAPHORE caps search/decode threads at `available_parallelism()` (the non-blocking inline fallback was measured and rejected: search wall-sum grew ~5× at 16 threads). (3) the fsync convoy is contract-inherent (write→fsync durability linearizability) and shrank indirectly — mounted `commit_lock_wait` 34.7% → 16.4% at 16 threads. Two read-window defects the instrumentation exposed are fixed with a regression test (inclusive pending-range bound; unconditional predecessor scan-start extension) | ✅ implemented + evidence-sealed (`recon-court-1787762195-49f1a55/`; 417 lib tests green) |
| 11 (11D) | **Worker-pool decision oracle** (`docs/performance/worker-oracle.md`) — diagnostic, not a release: decomposes the 11C semaphore's opaque `prepare` bucket at 1/2/4/8/16 writers into `worker_queue_wait` (Gate A), `worker_scope_wall` (Gate B), and `worker_useful_cpu` (per-worker thread-CPU, Gate C), with workload-validity probes (dedup-hit / decisive-exit fractions, asserted zero). First run caught its own methodology bug (one store across the sweep → a mid-run checkpoint fed the EXACT_REF dedup cache and the 16 T row measured the cache, not search); fixed with fresh stores + per-write-distinct content. Sealed: search CPU constant 9.8–10.0 s at every thread count (semaphore wastes no CPU); queue wait 4.6% → 91.7% of `prepare` (Gate A fires — batch head-of-line blocking); 16 T wall 1.14 s ≈ the SMT-adjusted CPU floor (throughput exhausted); p50 5.3 → 52.4 ms / p99 9.5 → 177.6 ms (tail latency is the only real pool headroom). Decision: a fair pool is justified only as a latency-fairness probe (bar: beat p50/p99 at 8/16 T without more search CPU), rejected if it merely reproduces the 1.14 s floor | ✅ diagnostic, sealed (`worker-oracle-1787765041-052bc46/`; 419 lib tests green) |
| 11 (11E) | **Persistent fair worker pool** (`docs/performance/worker-pool-probe.md`, sealed `evidence/performance/worker-pool-probe-1787769464-8fdea62/`) — the 11D decision's experiment, probe-sealed and KEPT: persistent workers with TYPED tasks only (`EncodeChunk`/`DecodeExtent`, `(request_id, ordinal)`), per-request queues served round-robin one task per pick (per-worker cursors — the probe found the shared cursor silently pins each request to one worker when workers == active requests), results reassembled strictly by ordinal ("execution order may vary; persisted semantic order may not", byte-exact read-back verified), bounded queue with backpressure at submission (the probe found the naive wait deadlocks on an oversized request meeting an idle pool), per-store opt-in (the FUSE daemon keeps the semaphore unless `--worker-pool N`). Sealed at 16 writers, pool-16 vs semaphore: wall 0.79–0.80 vs 1.08–1.28 s (−29%, the batch-transition slack the 11D floor analysis missed), p50 47–48 vs 49–60 ms, p99 78–85 vs 152–241 ms (−68%), useful CPU +2.6–3.7% (straddles the +3% gate, below the +5% reject bar — the DSFB-mutex visibility the 11D brief predicted; 11F sharding is the follow-up), p99/p50 1.63 vs 3.88, max request slowdown 18× vs 47×. 8 writers: wall −34%, p99 −69%, CPU +3.7–6.6%. pool-8: same wall, −20% CPU, −59% p99 (the lower-power alternative); pool-4: control, too few workers. The semaphore remains the mount default pending the mounted-FUSE court | ✅ probe-sealed, KEPT (`worker-pool-probe-1787769464-8fdea62/`; 421 lib tests green) |
| 11 (11E1) | **Mounted-FUSE court + the data-loss bug it found** (`tools/court-worker-pool-mount.sh`, sealed `evidence/performance/worker-pool-mount-court-1787786369-*`; CHANGELOG v0.7.6) — semaphore/pool-8/pool-16 × FUSE threads 1/4/8/16 against a 13-workload battery with byte-exact readback + fsck per cell. pool-16 passes ALL five gates (parallel write +14%, latency p95 −39%, p99 −48%, wall −26%, CPU +2.8%, serial neutral, cleanliness clean) → **the FUSE mount now runs the pool by default** (`available_parallelism()` workers; `--no-worker-pool` restores the 11C semaphore). The court exposed a REAL write-path data-loss bug (parallel untar lost ~10–45% of small files' extents; silent zero reads; fsck-clean because internally consistent): the checkpoint committed stale pending data roots, replay applied log-staged inodes wholesale, and getxattr probed xattrs flushed the epoch on every call — three fixes, regression-pinned in `src/tests/write_race.rs` | ✅ implemented + evidence-sealed (`worker-pool-mount-court-1787786369-b756a7c/`; 423 lib tests green) |
| 11 (11F) | **Sharded DSFB observer** (`src/dsfb/observer.rs` `ShardedStorageObserver`, probe `src/tests/dsfb_shard_probe.rs`, sealed `evidence/performance/dsfb-shard-probe-mutex-1787789207-f103248/` + `dsfb-shard-probe-sharded-*`; CHANGELOG v0.7.7) — the last process-wide write-path mutex, removed: 16 per-key shard locks (stable FNV-1a over the key bytes) + lock-free aggregate stats + exact atomic live count; the cap gate evicts from the shard that just grew (total ≤ cap + 1). The oracle (same probe binary-shape at both commits, pool-16, 1/8/16 writers + a 4× scale run) RECORDED A FALSIFICATION WITH AN ADOPTION: the 11D-predicted mutex visibility was real in the observer rows themselves (the plan call — a 9-element sort under the lock — lost ~66% of its wall under 16-way concurrency: 34–38 → 10.5–11.5 ms at 4× scale) but all observer calls together are ~1 µs each, 0.1% of `prepare`, so wall/p50/p99/useful CPU are unchanged within ±1% noise; byte identity, logical bytes, candidates, and families identical on every run of both sides. Adopted for architecture (per-key state ⇒ per-key locking; the write path is now synchronization-free except the commit coordinator and per-inode locks), future-proofing (12C deepens per-call work), and zero measured regression. The DSFB timing rows + candidate counter land as permanent write-path diagnostics for 12C | ✅ implemented + evidence-sealed (`dsfb-shard-probe-*`; 428 lib tests green) |

**Phase 11 is CLOSED** — 11A hostile persistent input → 11B write-latency
reconciliation → 11C synchronization/oversubscription removal → 11D
worker oracle → 11E fair worker pool (probe + mounted court, the mount
default) → 11F observer shard. The remaining foreground bottleneck is
useful search CPU itself; the next research sequence is Phase 12: 12A
Hot-DAG terminalization oracle, 12B durability generations / group
commit, 12C DSFB structural semiotics, 12D grammar-addressed entropy
(offline oracle first).

**Phase 12 progress:** 12A-0 (the Hot-DAG read-cost oracle, v0.7.8)
REJECTED the terminalization daemon on measured evidence. 12B (durability
generations / group commit, v0.7.9) sealed the fsync coalescing
(amplification 0.23 at 32 writers, crash courts green at every stage).
12C-0 (DSFB structural semiotics, v0.7.10) RECORDED that the semantic
prior reorders the search (winner rank 4.41 → 1.02) with byte-exact,
density-identical correctness, but the standalone CPU gain is ~3% — the
adaptive foreground budget is the identified continuation. 12D-0 (the
grammar-addressed entropy OFFLINE oracle, v0.7.11) STOPPED per the
brief's gate: the fully-accounted template grammar beats EntropyFS
settled 7.0× on the grammar-friendly corpus and loses as expected on the
diverse control, but zstd-whole beats it 2.2× (the grammar's skeleton
must itself be entropy-coded — the brief's "persisted entropy" — before
any format-bit investigation).
