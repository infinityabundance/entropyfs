# EntropyFS

de Beer, R. (2026). *EntropyFS: Entropy-Native Configurational Storage as a Filesystem Substrate - Broad Prior-Art Technical Disclosure and Research Architecture* (Version v1.0). Zenodo. https://doi.org/10.5281/zenodo.22092869

**Persist irreducible state. Materialize structure. Preserve exact bytes.
Measure everything.**

EntropyFS is a native-Rust, mountable Linux filesystem whose central research
premise is:

> Logical bytes are an interface presented to applications. They do not have
> to be the primary persisted representation. EntropyFS persists the minimum
> exact reversible entropy/configuration state necessary to reproduce those
> bytes.

The defining equation:

```text
X = Materialize(D)
```

`X` is the exact logical byte sequence; `D` is the persisted representation
descriptor. A more general family:

```text
X = T(E(U, S, P)) ⊕ R
```

`U` = versioned entropy universe · `S` = seed/state · `P` = rank/coordinate ·
`T` = bounded reversible transform · `R` = exact residual · `E` =
deterministic materialization.

EntropyFS does **not** claim to evade information theory: the SSD still
stores physical bits. Its innovation is to make *entropy state, mathematical
rank/configuration, immutable references, deterministic generators,
transforms, and irreducible residuals* first-class storage representations
instead of assuming logical byte blocks must themselves exist at rest. For
random/encrypted/incompressible data it converges gracefully toward ordinary
physical storage (RAW fallback) — that is a success condition, not a failure.

## Current status

EntropyFS is under active research and development.

For the complete implementation history — the phase-by-phase ledger with its
historical caveats, methodology corrections, rejected hypotheses, measured
results, and release-by-release changes — see **[CHANGELOG.md](CHANGELOG.md)**.

For sealed performance and experimental evidence, see
**[evidence/performance/INDEX.md](evidence/performance/INDEX.md)**.

### Current development focus

- Latest completed phase: **12A-0 — the Hot-DAG read-cost oracle**
  (`docs/performance/dag-read-cost.md`, sealed
  `evidence/performance/dag-read-cost-probe-1787790816-ef6508b/`):
  per-materialization `ReadCostSample` instrumentation + a hotness
  tracker, and a controlled-DAG oracle (raw / exactref / base-inline /
  base-object / diamond / seqdict at depths 0–4, cold/warm/hot reads)
  that **REJECTED the terminalization daemon** on measured evidence —
  depth predicts read latency only through object/decode width (~3.3× at
  d4 for object-backed chains, ~1.35× for the search-natural inline
  chains; fanout flat; rebase-at-depth-2 + `λ_depth` already price the
  costly shape). The instrumentation stays as the 12B/12C measurement
  surface. **Phase 11 is closed** (11A hostile input → 11B
  reconciliation → 11C synchronization removal → 11D oracle → 11E fair
  pool, mount default → 11F observer shard); the 11E/11F results are in
  `docs/performance/worker-pool-probe.md` and CHANGELOG v0.7.4–0.7.7.
- Current decision: the pool is the mount default
  (`available_parallelism()` workers; `--no-worker-pool` restores the 11C
  semaphore as the fallback). The next research steps are 12B durability
  generations / group commit over the existing MutationLog, 12C DSFB
  structural semiotics, and 12D grammar-addressed entropy (offline
  oracle first).
- Persistent format: explicit, versioned, incompat-feature-gated.
- Correctness: crash courts + hostile-media court + fsck, byte-exact
  read-back under every scheduler.
- Evidence: all material performance claims point to sealed artifacts.

## Measured results

All performance and storage-density claims are governed by
`docs/performance/methodology.md`; admitted results live in
`evidence/performance/` (see `evidence/performance/INDEX.md`).

**FUSE-frontend before/after pair (Phase 6, same workloads, same machine):**

| Workload | `709a710` (before) | `027c959` (after) |
| --- | --- | --- |
| 1M writes | 185 MiB/s | 653 MiB/s |
| 4K buffered writes | 0.6 MiB/s | 24.4 MiB/s |
| bindgen cold build (target on mount) | FAILED (SIGSEGV/SIGBUS) | 9.5 s |
| fsync p50 | 320 µs | 1647 µs |

The fsync regression is the measured cost of deferred durability and is
reported honestly; the before half reproduces the crashes the
oversized-descriptor fix (Phase 6) eliminated.

**What EntropyFS does and does not claim:**

- The synthetic ablation fixture (`evidence/ablation-2026-08-25.json`) is an
  *ablation fixture*, never a headline: on a corpus containing four unique
  64 KiB chunks its 16.876× is dominated by content-addressed dedup. The
  campaign's structured-corpus ratios (up to 1,328×) are *structural +
  EXACT_REF aliasing + CAS object sharing* — attribution is now
  measured, not labeled: per-run accounting separates `cas_shared_bytes_saved`
  (a store invariant) from `exact_ref_bytes_saved` (the gated alias
  representation), and A1 is pure byte rANS with SequenceRans as the
  post-registration E1 step (`campaign-1787671040-923df7b/`). The earlier
  “dedup = 0” and “dedup-dominated” statements conflated the two layers
  and are amended in `evidence/performance/INDEX.md`, never rewritten.
- The campaign's ablation evidence is two tables, both kept forever: the
  **strict cumulative ladder A0–A8** (each step adds one mechanism) and the
  **leave-one-out table** (one mechanism disabled at a time). The first
  campaign's nine-row table is the leave-one-out table; it predates the
  two-table rule and is amended as such in `evidence/performance/INDEX.md`
  (protocol note, never rewritten).
- The campaign's DSFB investigation is a sealed three-era series, all
  with byte-identical physical representations: RANS-era 765.4 vs 334.7
  MiB/s (2.29×, user CPU halved — `67d977a`), SequenceRans-era 773.9 vs
  717.1 MiB/s (~8%, `b165d60`), and CAS-era 1,120.8 vs 1,106.1 MiB/s
  (~1.3%, `923df7b`). DSFB's marginal benefit collapsed as the
  SequenceRans floor simplified the search landscape — evidence for its
  assigned role as candidate-search budget intelligence, not compression;
  its marginal value now is small on this tiny synthetic corpus and its
  proper counters are deferred. Historical numbers are preserved in
  `evidence/performance/INDEX.md`; the 2.29× is not a current claim.
- The source-corpus progression is now sealed across eras: `923df7b` pure
  byte rANS 1.633× / standalone SequenceRans 3.556× (with
  zstd-per-64KiB -1 at 3.739× — the per-extent floor was within 5%, and
  the gap to whole-file zstd was cross-chunk context), `8250f6b`
  **EntropyFS full 4.070× with SequenceDict** on the packed stream, and
  the Phase-9C/9D/9E tree court answering the open mount-level question:
  on a real tree of separately-written files, per-file zstd -1 is ~3.5×
  and EntropyFS per-file writes are **2.194×, rising to 2.354× post-GC
  after the shared-dict pool + deep background pass** — so most of the
  packed-stream density was indeed cross-FILE structure, and the shared
  dictionary (plus the deep matcher, which lifts the standalone src-pack
  floor from 3.736× to 3.786×) recovers a measured part of it. The
  Phase-9F gap decomposition (sealed in the 9F tree court) shows where
  the remaining gap lives: the anchor policy is NOT the cap (zstd with
  the same per-directory anchor gains only ~8.5%, less than EntropyFS's
  pool already recovers), and the residual gap is **~2/3 per-extent
  persistence overhead** (per-chunk multi-stream rANS models + descriptors
  on small files; ~26.5% of the EntropyFS footprint) and **~1/3 coder
  quality**. Recorded as current state, not claims.
- **Phase-9G0 (sealed `80e36c8`) validated the 9F diagnosis directly**:
  the per-stream RAW/rANS gate now includes the persisted model bytes
  (was: rANS whenever `enc < raw`, which persisted models that could
  never pay for themselves), cutting the sequence families' model objects
  on the real tree from 277.6 KB to 74.3 KB — per-extent overhead 26.5% →
  11.1% of footprint, tree court 2.388× → **2.775×** post shared-dict,
  src corpus 4.327×. The model-sharing oracle (diagnostic) then decided
  the 9G design: sharing one model across an extent's streams is
  falsified (−125 KB); one aggregate model per stream type per directory
  cohort is validated (+49.6 KB) and needs no format change (the model
  object is content-addressed and CAS-amortized); bundle pools of 2/4
  lose to the single aggregate. 9G0's sealed campaign numbers are in
  `evidence/performance/INDEX.md`; the oracle is a diagnostic, not a
  claim.
- **Phase-9G (sealed `60ecaf2`) implemented the oracle's S2**: the
  background `model_bundle_pass` trains one aggregate model per stream
  type per directory cohort and re-encodes members against it (per-stream
  RAW fallback), rewriting only when the cohort's total strictly falls.
  No format change and no new feature bit — model objects were already
  content-addressed descriptors. Tree court: shared-dict 2.813× → **2.881×**
  (1,065,145 B reachable, 65 rewrites; the real post-GC saving 25.7 KiB
  exceeds the 7.5 KiB conservative cohort-accounted claim because GC
  reclaims the superseded per-extent models). The win is better-trained
  aggregate models (the enc side), not model dedup — the model-bytes
  metric is flat, recorded as-is.
- **Phase-9H (sealed `0a03ece`) — physical convergence**: the 9G tree
  court exposed that the optimized tree occupied 3.66 MB of backing for
  1.07 MB reachable (0.84× logical). The physical scanner
  (`store/physical.rs`) reconciled every segment byte and falsified the
  index-hidden-duplicate hypothesis on that workload (index-hidden = 0):
  the dead bytes were 2.66 MB of `BtreeNode` records staged by the GC
  chunk-index REBUILD, which rebuilt the tree with repeated COW inserts
  — every intermediate path version written and indexed. Fixes: GC
  victim selection now uses scanned physical occupancy; the rebuild
  bulk-loads the tree bottom-up (each final node staged exactly once);
  `entropyfs gc --compact` (`compact_full`) converges the backing and is
  idempotent. Tree court: backing **9,129,988 B → 1,100,161 B**;
  post-GC reconciliation = reachable 1,100,157 B + 0 B dead + 0 B
  index-hidden + 0 B unindexed + 4 B format; full compaction = reachable
  + 4 B (0.00% of logical); second compaction reclaims 0. The 2.88×
  representation win is now a real ~2.9× capacity win — the physical
  floor no longer eats the representation state.
- The campaign's H2 experiment (synthetic drift corpus) is now a sealed
  **controlled series**: `67d977a` +7.2% (RANS-era floor), `a6641d1`
  −24% (SequenceRans floor, positional residuals only), `43bf17e`
  **+35.2%** (SequenceRans floor + BASE_SEQUENCE shift-aware deltas), and
  `923df7b` **+40.6%** (marginal costing). The shuffled control grows in
  the delta campaign because copy/literal deltas also exploit structural
  similarity between unrelated-history chunks — the control no longer
  isolates pure temporal causality, and that confounding is itself
  recorded as the finding.
- The competitive filesystem court is a sealed **zero-waiver series** at
  9H: the density comparison is now computed and sealed by the tooling
  itself (`fs-court-1787688843-b4abc71/`, privileged docker VM,
  symmetric buffered/durable/warm/cold rules) — the same corpus apparent
  sum (136,162,907 B) over each filesystem's complete state (whole loop
  image allocated for XFS/Btrfs, complete store backing for EntropyFS;
  both include filesystem metadata; derivation footnoted in report.md).
  EntropyFS is reported in two states — **foreground 1.825×** (74.6 MB
  post-GC) and **settled 1.994×** (68.27 MB) after background optimize +
  full compaction (5.42 s, 1.047× physical write amplification) — and the
  settled density **beats Btrfs+zstd 1.724× (79.00 MB image)** on the
  same corpus set (64 MiB incompressible random + 64 MiB zeros + src +
  tgz); Btrfs raw 0.941×, XFS 0.668× (metadata exceeds the corpus). The
  throughput gaps are recorded honestly: src tiny-file writes 10.2 MiB/s
  vs 100–457, 64 MiB random writes 68 vs ~5,900 MiB/s. The write path is
  now the dominant weakness; the court points the next phase at
  **performance** (Phase 10), not another entropy codec. (The earlier
  courts' hand-derived `1.65×`/`1.72×` Btrfs figures are amended in
  `evidence/performance/INDEX.md`, never rewritten.)
- Random/encrypted/already-compressed data falls back toward RAW (urandom
  0.997×, zstd -19 pack 0.993×) — the honest negative control.

## Honesty rules

- A 128-bit seed does not "store" a gigabyte. Descriptor bits select at most
  `2^k` states; every persisted bit is accounted
  (`docs/theory/information-accounting.md`).
- No hidden corpus, no network, no RNG in materialization, no CPU-dependent
  floating point. The universe specification is part of the format version.
- No arbitrary generator programs: the descriptor language is bounded and
  not Turing-complete (`docs/adr/0005-representation-set.md`).
- DSFB has zero decoding authority (`docs/adr/0004-dsfb-observer.md`).
- `statfs` reports physical capacity; effective ratio is an observation,
  never a promise (`docs/adr/0018-statfs.md`).
- Every optimization claim requires reproducible evidence
  (`docs/performance/methodology.md`).
- The hostile-media court (`docs/security/hostile-media-court.md`) is the
  adversarial input suite: malformed persistent data must return typed
  errors, never panic, never OOM, never unbounded CPU. The court's
  resource-bounds claim is only ever documented as implemented when the
  sealed evidence exists (`evidence/hostile-media/`).

## Building

```sh
rustup toolchain install stable --profile minimal --component rustfmt,clippy
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Target platform

CachyOS/Arch Linux, x86-64, FUSE (`/dev/fuse` + `fusermount3`; kernel
`CONFIG_FUSE_FS=y`). No custom kernel, no out-of-tree module, no reboot.

## Reading order

- `docs/architecture/overview.md` — architecture map
- `docs/theory/entropy-medium.md` — the information-theory boundary statement
- `docs/format/ondisk-v1.md` — the on-disk format
- `docs/recovery/crash-consistency.md` — the crash protocol
- `docs/adr/` — all architecture decision records
- `docs/architecture/commentary-standard.md` — the code-commentary rule
  (how to read and write this codebase's rationale)
- `CONTRIBUTING.md` — the evidence discipline and workflow

## License

MIT OR Apache-2.0, at your option.
