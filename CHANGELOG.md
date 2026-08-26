# EntropyFS changelog

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
