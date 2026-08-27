//! Phase 12C-1-0: the adaptive-foreground-budget FRONTIER oracle.
//!
//! # PURPOSE
//!
//! Phase 12E.13 discovered the adoption wedge — a **storage-density**
//! wedge (build-artifacts 0.049× raw ≈ 20×, four workloads clear 10×) —
//! but the tradeoff was explicit: engine put throughput is ~14× slower
//! than raw page-cache writes because the foreground search prices every
//! chunk with the full candidate sweep. The 12C-1 question is therefore:
//!
//! ```text
//! can EntropyFS preserve most of the 10–20x storage wedge while
//! spending dramatically less foreground search CPU?
//! ```
//!
//! The user's architecture for the answer:
//!
//! ```text
//! search_budget = f(
//!     semantic confidence,          (Phase-12C prior: class -> winner)
//!     historical winner confidence, (DSFB regime: Stable/Drift/Slew)
//!     worker queue pressure,        (Phase-11E pool saturation)
//!     current CPU saturation,
//!     expected marginal storage gain,(the Phase-10B entropy probe)
//!     object size,
//!     foreground latency target)
//!
//! high confidence + high pressure -> stop early
//! low confidence  + low pressure  -> search broadly
//! background optimizer            -> recover density later
//! ```
//!
//! Before ANY of that policy machinery is built, 12C-1-0 measures the
//! **cost–density frontier** of skipping with the mechanisms that ALREADY
//! exist — the oracle-first discipline: never implement a daemon or a
//! policy before evidence says the frontier allows the gate.
//!
//! # THE ARMS (one change per arm from the sealed 12E.13 baseline)
//!
//! Each arm differs ONLY in `ForegroundPolicy.mode`; `OptimizeOptions`,
//! the store config, the corpus, and the namespace shape are identical:
//!
//! ```text
//! full     ForegroundMode::Full     the sealed 12E.13 replay (anchor)
//! cheap    ForegroundMode::Cheap    the Phase-10B entropy-probe skip
//! focused  ForegroundMode::Focused  the Phase 12C-1 adaptive budget:
//!                                   entropy probe + the semantic
//!                                   class-prior rANS deferral (this
//!                                   arm ALSO enables the semantic prior
//!                                   — that is its defining input; the
//!                                   other arms keep the sealed None)
//! raw      ForegroundMode::RawOnly  the no-search control (CPU floor)
//! ```
//!
//! The `focused` arm measures the 12C-1-1 adaptive gate: when the chunk's
//! semantic class has enough observations and its winner distribution
//! says rANS rarely wins (`P(Rans) < 0.10`), the rANS sweep is deferred
//! to the background optimizer (which the frontier proved density-safe);
//! classes that genuinely win with rANS keep the full sweep. The
//! `mixed-control` workload (sparse / noise / text classes) demonstrates
//! the gate firing where the class distrusts rANS and staying off where
//! rANS wins — the adaptivity visible in one workload.
//!
//! The frontier rows per workload per arm:
//!
//! ```text
//! put wall          the whole write pass (mirrors the sealed PUT phase)
//! search CPU        the perf `search` row total (useful foreground work)
//! candidates/chunk  store.candidates_evaluated() / chunks
//! win rank          first-winning plan position (semantic_rank_stats)
//! raw fallback %    the RAW-winner fraction (semantic_raw_wins)
//! put p50/p95/p99   per-write latency percentiles (the p99 gate row)
//! gc footprint      physical_used() after ensure_epoch_flushed +
//!                   gc::compact_full — EXACTLY the sealed 12E.13
//!                   "settled physical" measurement
//! settled footprint physical_used() after the background optimizer pass
//!                   + shared-dict pass + checkpoint + compact — the
//!                   "background recovers density later" converged state
//! byte-exact        every file reads back exactly (always asserted)
//! ```
//!
//! # WHY THE FRONTIER IS THE 12C-1-0 DELIVERABLE
//!
//! The Phase-10B entropy probe only skips families for HIGH-ENTROPY
//! chunks (>= 7.2 bits/byte). Every adoption-wedge corpus is structured
//! low-entropy text (template/stamp lines), so on the wedge workloads
//! `cheap` and `full` are expected to run the SAME search: the frontier
//! will show whether the skip lever has ANY headroom on the wedge, and
//! where the addressable CPU actually is (`full` vs `raw`). That is the
//! evidence the 12C-1-1 adaptive policy (the `focused` arm — semantic +
//! winner confidence + pressure → budget) must be built against.
//!
//! A `noise-control` workload (deterministic random bytes) verifies the
//! arms on the RAW end: byte-exact everywhere, raw% ~ 100%, `cheap`
//! skipping to RAW (put-wall improvement) and `full` wasting CPU on
//! incompressible bytes — the "RAW controls unchanged" gate row.
//!
//! # COMPARABILITY (the anchor)
//!
//! The corpus is `crate::tests::adoption_corpus::workloads()` — the
//! sealed 12E.13 generators, verbatim — and the `full` arm replays the
//! sealed court's measurement: every blob put through the engine's own
//! put protocol (content-id file names, the fast-dedup acknowledged-blob
//! lookup, tmp-write-rename), `epoch_checkpoint`, byte-exact read-back,
//! then `ensure_epoch_flushed` + `gc::compact_full` + `physical_used()`.
//! The sealed 12E.13 "settled physical" rows are embedded here as the
//! reference; the probe reports the `full`-arm delta against them so the
//! writeup can verify the replay before trusting the regression curves.
//!
//! # BOUNDARY
//!
//! KNOWS: the store write path (`epoch_write`, the foreground policy,
//! DSFB diagnostics) — the CPU-budget authorities. NEVER KNOWS: nothing
//! deeper than the 12C oracle's own seam; it changes NO production code.
//! (The `focused` arm is 12C-1-1 and needs a new production
//! `ForegroundMode`; the arm table below is data-driven so that arm
//! slots in without restructuring.)
//!
//! # THE GATE (decided by the evidence tooling / writeup)
//!
//! ```text
//! on the adoption-wedge workloads, for the adopted arm vs `full`:
//!     put wall        >= 2x (ideally much more)
//!     search CPU      materially improved
//!     settled bytes   regression <= 5%
//!     byte identity   absolute (asserted)
//!     p99             no material regression (<= 5%)
//!     raw controls    unchanged (noise-control rows)
//! ```
//!
//! # HISTORY / EVIDENCE
//!
//! Sealed under `evidence/performance/adaptive-budget-probe-*/` by
//! `tools/court-adaptive-budget.sh` (v0.7.14 line). The sealed 12E.13
//! rows live in `docs/performance/adoption-oracle.md`.
//!
//! # RUN
//!
//! ```text
//! cargo test --release --lib adaptive_budget_probe -- --nocapture
//! ADAPTIVE_BUDGET_ARMS=full,cheap,raw ADAPTIVE_BUDGET_SETTLE=1 \
//!     cargo test --release --lib adaptive_budget_probe -- --nocapture
//! ```
//!
//! Prints `ADAPTIVE_BUDGET_ORACLE <json>`. `$ADAPTIVE_BUDGET_OUT` writes
//! the JSON to a file (the driver seals it). Debug builds run the `full`
//! arm only (correctness asserts hold; timing is not meaningful).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use tempfile::TempDir;

use crate::optimizer::foreground::ForegroundPolicy;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use crate::tests::adoption_corpus::{Workload, workloads};

/// The gate's normative targets (the writeup applies them to the rows).
const PUT_WALL_TARGET_X: f64 = 2.0;
const SETTLED_REGRESSION_LIMIT: f64 = 0.05;
const P99_REGRESSION_LIMIT: f64 = 0.05;
/// The 12E.13 wedge bar: footprint (physical / logical) <= 0.10 = 10x-class.
const WEDGE_FOOTPRINT_LIMIT: f64 = 0.10;

/// The sealed 12E.13 "settled physical" rows (logical, physical_after)
/// for the six adoption workloads — the replay anchor. Source:
/// `docs/performance/adoption-oracle.md` (evidence
/// `evidence/performance/adoption-oracle-1787840768-1b9926a/`).
const SEALED_12E13: &[(&str, u64, u64)] = &[
    ("build-artifacts", 18_666_000, 908_411),
    ("scientific-outputs", 8_013_660, 441_787),
    ("container-layers", 6_553_600, 552_512),
    ("generated-assets", 819_200, 78_510),
    ("ci-cache", 3_830_531, 402_651),
    ("source-trees", 9_088_550, 1_737_891),
];

/// One ablation arm: a `ForegroundPolicy` mode (the ONLY change vs the
/// sealed 12E.13 baseline; options and ranking stay default), plus
/// whether the arm enables the Phase-12C semantic prior (the `focused`
/// arm's defining input — the class confidence IS its budget signal; the
/// other arms keep the sealed baseline's disabled prior).
struct ArmSpec {
    name: &'static str,
    policy: ForegroundPolicy,
    /// Enable `SemanticMode::Combined` and feed per-write semantic
    /// contexts (the prior learns within the pass; later chunks of a
    /// class get guided budgets).
    semantic: bool,
}

/// The 12C-1 arms. `full` replays the sealed baseline; `cheap` is the
/// existing 10B skip; `focused` is the 12C-1-1 adaptive budget (with the
/// semantic prior enabled); `raw` is the CPU floor control.
fn arms() -> Vec<ArmSpec> {
    let requested: Vec<String> = std::env::var("ADAPTIVE_BUDGET_ARMS")
        .unwrap_or_else(|_| {
            if cfg!(debug_assertions) {
                "full".to_string()
            } else {
                "full,cheap,focused,raw".to_string()
            }
        })
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    let all: Vec<ArmSpec> = vec![
        ArmSpec {
            name: "full",
            policy: ForegroundPolicy::full(),
            semantic: false,
        },
        ArmSpec {
            name: "cheap",
            policy: ForegroundPolicy::cheap(),
            semantic: false,
        },
        ArmSpec {
            name: "focused",
            policy: ForegroundPolicy::focused(),
            semantic: true,
        },
        ArmSpec {
            name: "raw",
            policy: ForegroundPolicy::raw_only(),
            semantic: false,
        },
    ];
    all.into_iter()
        .filter(|a| requested.iter().any(|r| r == a.name))
        .collect()
}

/// Self thread CPU seconds via `/proc/self/stat` (utime+stime in USER_HZ
/// ticks) — no libc, no unsafe (same helper as the adoption court).
fn thread_cpu_seconds() -> f64 {
    let body = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let Some(rest) = body.split_once(')') else {
        return 0.0;
    };
    let fields: Vec<&str> = rest.1[1..].split_whitespace().collect();
    let utime: f64 = fields.get(11).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let stime: f64 = fields.get(12).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    (utime + stime) / 100.0
}

/// Nearest-rank percentile over a µs sample slice.
fn pct(samples: &mut [u64], q: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.sort_unstable();
    let i = ((samples.len() - 1) as f64 * q).round() as usize;
    samples[i] as f64
}

fn ratio(a: f64, b: f64) -> f64 {
    if b <= 0.0 { 0.0 } else { a / b }
}

fn create_store(dir: &TempDir) -> Arc<Store> {
    let cfg = StoreConfig {
        segment_size: 128 * 1024 * 1024,
        ..Default::default()
    };
    Arc::new(Store::create(dir.path(), &cfg, [0x66; 16]).unwrap())
}

/// The hostile-media RAW control: 200 deterministic random blobs of
/// 1–64 KiB. The arms must all be byte-exact here; the RAW winner must
/// stay ~100% in every arm (a budget or policy change must never turn
/// random bytes into a "compressed" winner — the "RAW controls
/// unchanged" gate row). Semantic-deception NAMES (random bytes named
/// `.rs`, compressed data named `.txt`, extensionless files) return with
/// the 12C-1-1 `focused` arm's runner, which is where the prior is
/// active; here the content itself is the only signal.
fn noise_control() -> Workload {
    let mut blobs = Vec::new();
    let mut state: u64 = 0x0123_4567_89AB_CDEF;
    for i in 0..200u64 {
        let len = 1024 + ((i * 2654435761) % (63 * 1024)) as usize;
        let mut b = Vec::with_capacity(len);
        while b.len() < len {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            b.push((state >> 33) as u8);
        }
        blobs.push(b);
    }
    Workload {
        name: "noise-control",
        blobs,
    }
}

/// The 12C-1-1 adaptivity control: three DISTINCT semantic classes whose
/// winner distributions disagree, so the `focused` arm's gate must fire
/// where the class distrusts rANS and stay off where rANS wins:
///
/// ```text
/// sparse  64 KiB mostly-zeros with a few stamped nonzero bytes:
///         the SPARSE family wins (attributed Channel::Raw), so the
///         class's P(Rans) -> 0 and the focused gate defers rANS
///         (correct: rANS cannot beat SPARSE on sparse data)
/// noise   uniform random: RAW wins; the entropy probe handles it
/// text    structured template lines: rANS wins, so P(Rans) -> 1 and
///         the focused gate stays OFF (density protected)
/// ```
///
/// All chunks are DISTINCT content (no fast-dedup hits), so every chunk
/// is searched and observed — the classes accumulate observations and
/// the gate's self-calibration is exercised end-to-end. Byte-exactness
/// is asserted in every arm.
fn mixed_control() -> Workload {
    let mut blobs = Vec::new();
    let mut state: u64 = 0xFEED_FACE_CAFE_BEEF;
    // sparse: mostly zeros, distinct nonzero stamps.
    for i in 0..60u64 {
        let mut b = vec![0u8; 65536];
        for k in 0..8u64 {
            let pos = (k * 7919 + i * 104729) as usize % 65528;
            b[pos] = (i.wrapping_mul(31).wrapping_add(k.wrapping_mul(7))) as u8;
        }
        blobs.push(b);
    }
    // noise: uniform random.
    for _ in 0..60u64 {
        let mut b = Vec::with_capacity(65536);
        while b.len() < 65536 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            b.push((state >> 33) as u8);
        }
        blobs.push(b);
    }
    // text: structured template lines (rANS-winning).
    for i in 0..60u64 {
        let mut b = Vec::with_capacity(65536);
        while b.len() < 65536 {
            b.extend_from_slice(format!("config item {i} key=value seq={}\n", b.len()).as_bytes());
        }
        blobs.push(b);
    }
    Workload {
        name: "mixed-control",
        blobs,
    }
}

/// One arm's measured rows for one workload.
#[derive(Default)]
struct ArmRows {
    blobs: u64,
    logical: u64,
    unique: u64,
    put_wall_s: f64,
    put_cpu_s: f64,
    search_cpu_ms: f64,
    candidates: u64,
    chunks: u64,
    candidates_per_chunk: f64,
    win_rank: f64,
    raw_wins: u64,
    raw_pct: f64,
    rans_skips: u64,
    put_latency_us: Vec<u64>,
    physical_after_gc: u64,
    gc_reclaimed: u64,
    physical_after_settle: u64,
    byte_exact: bool,
}

impl ArmRows {
    /// Footprint after the GC-only settle (the sealed 12E.13 measure).
    fn gc_footprint(&self) -> f64 {
        ratio(self.physical_after_gc as f64, self.logical as f64)
    }

    /// Footprint after the background-optimizer settle (the converged
    /// "background recovers density later" state).
    fn settled_footprint(&self) -> f64 {
        ratio(self.physical_after_settle as f64, self.logical as f64)
    }

    fn p50(&self) -> f64 {
        pct(&mut self.put_latency_us.clone(), 0.50)
    }
    fn p95(&self) -> f64 {
        pct(&mut self.put_latency_us.clone(), 0.95)
    }
    fn p99(&self) -> f64 {
        pct(&mut self.put_latency_us.clone(), 0.99)
    }
}

/// Run one (workload, arm) pair in a fresh store. Returns the rows; the
/// settled footprint is measured after the background optimizer only
/// when `settle` is set (the driver enables it; quick smoke runs skip).
fn run_arm(root: &Path, wl: &Workload, arm: &ArmSpec, settle: bool) -> ArmRows {
    let dir = TempDir::new_in(root).expect("arm tmpdir");
    let store = create_store(&dir);
    let fg = arm.policy;
    let opts = OptimizeOptions::default();
    let hooks = &CrashHooks::none();

    // One namespace dir per workload (the engine's blob-namespace role;
    // created through the epoch, the light path).
    let root_ino = store.current_root().root_dir_ino;
    let dir_ino = store
        .epoch_create(
            root_ino,
            wl.name.as_bytes(),
            NewEntry::dir(0o755, 1000, 1000),
            hooks,
        )
        .unwrap();

    // The `focused` arm's defining input: the semantic prior (Phase-12C
    // Combined mode), fed a per-write context so the class table learns
    // within the pass. The other arms keep the sealed baseline's disabled
    // prior — the ONLY difference between the arms is the policy mode
    // (plus, for focused, the prior it requires).
    if arm.semantic {
        store.set_semantic_mode(crate::dsfb::semantics::SemanticMode::Combined);
    }

    // ---- PUT phase: the engine's put_blob protocol, mirrored exactly ----
    //
    // Every blob is put through the SAME operations `Engine::put_blob`
    // performs, so the `full` arm's put wall and footprint are directly
    // comparable to the sealed 12E.13 rows:
    //
    //   1. the final name IS the blob's content id;
    //   2. fast-dedup: a REG file under the final name is an
    //      acknowledged blob — the put returns immediately (the engine's
    //      acknowledged-blob check; ~80% of build-artifacts are hits);
    //   3. otherwise: epoch_create(tmp) + epoch_write(blob) +
    //      epoch_rename(tmp -> final) — the write-then-rename protocol
    //      that makes partial states invisible under a final name.
    let mut rows = ArmRows::default();
    let cpu0 = thread_cpu_seconds();
    let t0 = Instant::now();
    let mut inos: Vec<u64> = Vec::with_capacity(wl.blobs.len());
    let mut unique: BTreeMap<crate::core::extent::ChunkId, u64> = BTreeMap::new();
    for b in wl.blobs.iter() {
        let cid = crate::core::extent::ChunkId::of(b);
        // First-seen only: identical content costs nothing a second time
        // (the dedup attribution, same definition as the sealed court).
        unique.entry(cid).or_insert(b.len() as u64);
        let final_name = crate::engine::BlobId::from(cid).to_string().into_bytes();
        // The per-write semantic context (focused arm only): the content-id
        // name contributes weak name classes; the byte sketch carries the
        // content signal (printable/entropy/magic) that the classes in
        // this corpus actually separate on.
        let sem = if arm.semantic {
            let mut s = crate::dsfb::semantics::SemanticContext::from_name(&final_name, 1);
            let sketch = crate::dsfb::semantics::SemanticContext::from_bytes(b);
            s.magic_class = sketch.magic_class;
            s.printable_ratio = sketch.printable_ratio;
            s.entropy_class = sketch.entropy_class;
            s.lifecycle = 0; // new (single write)
            Some(s)
        } else {
            None
        };
        let t = Instant::now();
        {
            // The lookup guard must drop before the create (the epoch
            // mutex is not reentrant).
            let ep = store.epoch();
            match store.dir_lookup_epoch(&ep, dir_ino, &final_name).unwrap() {
                Some(e) if e.d_type == crate::store::directory::dt::DT_REG => {
                    // Fast-dedup hit: the content is already acknowledged
                    // under its name; the put is a lookup (the sealed
                    // court's Ack path for duplicate blobs — the 80%
                    // stable build-artifact rows never re-search).
                    rows.put_latency_us.push(t.elapsed().as_micros() as u64);
                    inos.push(e.ino);
                    continue;
                }
                _ => {}
            }
        }
        let tmp = format!("-tmp-{:016x}", inos.len());
        let ino = store
            .epoch_create(
                dir_ino,
                tmp.as_bytes(),
                NewEntry::file(0o600, 1000, 1000),
                hooks,
            )
            .unwrap();
        store
            .epoch_write_semantic(ino, 0, b, opts, fg, sem, hooks)
            .unwrap();
        store
            .epoch_rename(dir_ino, tmp.as_bytes(), dir_ino, &final_name, hooks)
            .unwrap();
        rows.put_latency_us.push(t.elapsed().as_micros() as u64);
        inos.push(ino);
    }
    rows.put_wall_s = t0.elapsed().as_secs_f64();
    rows.put_cpu_s = thread_cpu_seconds() - cpu0;
    rows.logical = wl.blobs.iter().map(|b| b.len() as u64).sum();
    rows.unique = unique.values().sum();
    rows.blobs = wl.blobs.len() as u64;

    // Commit, then read every file back byte-exact (asserted — the byte
    // identity is absolute in every arm).
    store.epoch_checkpoint(hooks).unwrap();
    rows.byte_exact = wl.blobs.iter().enumerate().all(|(i, b)| {
        store
            .read_file(inos[i], 0, b.len() as u64)
            .map(|g| g == *b)
            .unwrap_or(false)
    });
    assert!(
        rows.byte_exact,
        "{} arm {}: read-back mismatch",
        wl.name, arm.name
    );

    // ---- Diagnostic rows ----
    let perf = store.perf().snapshot();
    if std::env::var("ADAPTIVE_BUDGET_PERF")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        eprintln!("  [perf] {} arm {}:", wl.name, arm.name);
        for r in perf.iter().filter(|r| r.total_ms >= 0.5) {
            eprintln!("    {:28} {:10.2} ms", r.phase, r.total_ms);
        }
    }
    rows.search_cpu_ms = perf
        .iter()
        .find(|r| r.phase == "search")
        .map(|r| r.total_ms)
        .unwrap_or(0.0);
    rows.candidates = store.candidates_evaluated();
    let (rank_sum, rank_count) = store.semantic_rank_stats();
    rows.chunks = rank_count;
    rows.candidates_per_chunk = ratio(rows.candidates as f64, rank_count.max(1) as f64);
    rows.win_rank = ratio(rank_sum as f64, rank_count.max(1) as f64);
    rows.raw_wins = store.semantic_raw_wins();
    rows.raw_pct = rows.raw_wins as f64 / rank_count.max(1) as f64 * 100.0;
    rows.rans_skips = store.focused_rans_skips();

    // ---- Settle A: the sealed 12E.13 measurement (GC only) ----
    store.ensure_epoch_flushed(hooks).unwrap();
    let _unreachable_before = crate::store::gc::unreachable_bytes(&store).unwrap();
    rows.gc_reclaimed = crate::store::gc::compact_full(&store, hooks).unwrap();
    rows.physical_after_gc = store.physical_used();

    // ---- Settle B: the background-optimizer converged state ----
    if settle {
        let _opt = crate::optimizer::background::optimize_pass(&store, opts, None, None).unwrap();
        let _sd = crate::optimizer::background::shared_dict_pass(&store, opts, None).unwrap();
        store.epoch_checkpoint(hooks).unwrap();
        store.ensure_epoch_flushed(hooks).unwrap();
        crate::store::gc::compact_full(&store, hooks).unwrap();
        rows.physical_after_settle = store.physical_used();
    } else {
        rows.physical_after_settle = rows.physical_after_gc;
    }
    let _ = cpu0;
    rows
}

/// The per-workload comparison block: the adopted-arm deltas vs `full`.
fn comparisons(full: &ArmRows, arm: &ArmRows) -> serde_json::Value {
    serde_json::json!({
        "put_wall_speedup_x": ratio(full.put_wall_s, arm.put_wall_s),
        "search_cpu_speedup_x": ratio(full.search_cpu_ms, arm.search_cpu_ms),
        "gc_footprint_regression": ratio(arm.gc_footprint(), full.gc_footprint()) - 1.0,
        "settled_footprint_regression": ratio(arm.settled_footprint(), full.settled_footprint()) - 1.0,
        "p99_ratio": ratio(arm.p99(), full.p99()),
        "raw_fallback_delta_pp": arm.raw_pct - full.raw_pct,
        "wedge_preserved": arm.gc_footprint() <= WEDGE_FOOTPRINT_LIMIT,
        "gate_put_wall": ratio(full.put_wall_s, arm.put_wall_s) >= PUT_WALL_TARGET_X,
        "gate_settled": ratio(arm.settled_footprint(), full.settled_footprint()) - 1.0 <= SETTLED_REGRESSION_LIMIT,
        "gate_p99": ratio(arm.p99(), full.p99()) <= 1.0 + P99_REGRESSION_LIMIT,
    })
}

fn arm_json(r: &ArmRows) -> serde_json::Value {
    serde_json::json!({
        "blobs": r.blobs,
        "logical_bytes": r.logical,
        "unique_bytes": r.unique,
        "dedup_saved_bytes": r.logical.saturating_sub(r.unique),
        "put_wall_s": r.put_wall_s,
        "put_cpu_s": r.put_cpu_s,
        "search_cpu_ms": r.search_cpu_ms,
        "candidates": r.candidates,
        "chunks": r.chunks,
        "candidates_per_chunk": r.candidates_per_chunk,
        "win_rank": r.win_rank,
        "raw_wins": r.raw_wins,
        "raw_fallback_pct": r.raw_pct,
        "rans_skips": r.rans_skips,
        "put_p50_us": r.p50(),
        "put_p95_us": r.p95(),
        "put_p99_us": r.p99(),
        "gc_footprint": r.gc_footprint(),
        "settled_footprint": r.settled_footprint(),
        "physical_after_gc": r.physical_after_gc,
        "physical_after_settle": r.physical_after_settle,
        "gc_reclaimed_bytes": r.gc_reclaimed,
        "byte_exact": r.byte_exact,
    })
}

#[test]
fn adaptive_budget_probe() {
    let settle = std::env::var("ADAPTIVE_BUDGET_SETTLE")
        .map(|v| v != "0")
        .unwrap_or(true);
    let tmp = TempDir::new().expect("tmp");

    // The sealed corpus + the 12C-1 controls.
    let mut wls = workloads();
    wls.push(noise_control());
    wls.push(mixed_control());

    let arms = arms();
    let mut details = serde_json::Map::new();
    for wl in &wls {
        let mut per_arm: BTreeMap<String, ArmRows> = BTreeMap::new();
        for arm in &arms {
            let rows = run_arm(tmp.path(), wl, arm, settle);
            per_arm.insert(arm.name.to_string(), rows);
        }
        // The `full` arm is the anchor; skip comparisons without it.
        let full = per_arm.get("full");
        let mut arm_map = serde_json::Map::new();
        let mut comp_map = serde_json::Map::new();
        for (name, rows) in &per_arm {
            arm_map.insert(name.clone(), arm_json(rows));
            if let (Some(f), true) = (full, name.as_str() != "full") {
                comp_map.insert(name.clone(), comparisons(f, rows));
            }
        }
        // The sealed-anchor delta: how close the `full` arm's GC settle
        // lands to the sealed 12E.13 row (the namespace-form replay check;
        // the writeup verifies the replay before trusting the curves).
        let full_delta = match (SEALED_12E13.iter().find(|(n, _, _)| *n == wl.name), full) {
            (Some((_, logical, physical)), Some(f)) => Some(serde_json::json!({
                "sealed_logical": logical,
                "sealed_physical_after": physical,
                "sealed_footprint": *physical as f64 / *logical as f64,
                "full_gc_physical_delta_pct": (ratio(f.physical_after_gc as f64, *physical as f64) - 1.0) * 100.0,
            })),
            _ => None,
        };
        details.insert(
            wl.name.to_string(),
            serde_json::json!({
                "blobs": wl.blobs.len(),
                "arms": arm_map,
                "comparisons": comp_map,
                "sealed_12e13": full_delta,
                "wedge_limit": WEDGE_FOOTPRINT_LIMIT,
            }),
        );
    }

    // ---- Human table (stderr) ----
    eprintln!(
        "\n==== Phase-12C-1-0 adaptive-budget frontier ({} workloads x {} arms, settle={}) ====",
        wls.len(),
        arms.len(),
        settle
    );
    for wl in &wls {
        let d = &details[wl.name];
        eprintln!("-- {} --", wl.name);
        eprintln!(
            "{:<6} {:>8} {:>9} {:>9} {:>9} {:>8} {:>8} {:>8} {:>7} {:>5} {:>6} {:>6}",
            "arm",
            "put_ms",
            "search",
            "cand/ch",
            "win_rank",
            "raw%",
            "gc_foot",
            "settled",
            "p99us",
            "skip",
            "exact",
            "wedge"
        );
        for arm in &arms {
            let a = &d["arms"][arm.name];
            let wedge = a["gc_footprint"].as_f64().unwrap_or(1.0) <= WEDGE_FOOTPRINT_LIMIT;
            eprintln!(
                "{:<6} {:>8.1} {:>9.1} {:>9.2} {:>9.2} {:>8.1} {:>8.3} {:>8.3} {:>7.0} {:>5} {:>6} {:>6}",
                arm.name,
                a["put_wall_s"].as_f64().unwrap_or(0.0) * 1e3,
                a["search_cpu_ms"].as_f64().unwrap_or(0.0),
                a["candidates_per_chunk"].as_f64().unwrap_or(0.0),
                a["win_rank"].as_f64().unwrap_or(0.0),
                a["raw_fallback_pct"].as_f64().unwrap_or(0.0),
                a["gc_footprint"].as_f64().unwrap_or(0.0),
                a["settled_footprint"].as_f64().unwrap_or(0.0),
                a["put_p99_us"].as_f64().unwrap_or(0.0),
                a["rans_skips"].as_u64().unwrap_or(0),
                if a["byte_exact"].as_bool().unwrap_or(false) {
                    "ok"
                } else {
                    "MISMATCH"
                },
                if wedge { "yes" } else { "no" },
            );
        }
    }

    let result = serde_json::json!({
        "schema": "adaptive-budget-oracle-v1",
        "arms": arms.iter().map(|a| a.name).collect::<Vec<_>>(),
        "settle_passes": settle,
        "gate": {
            "put_wall_target_x": PUT_WALL_TARGET_X,
            "settled_regression_limit": SETTLED_REGRESSION_LIMIT,
            "p99_regression_limit": P99_REGRESSION_LIMIT,
            "wedge_footprint_limit": WEDGE_FOOTPRINT_LIMIT,
        },
        "workloads": details,
    });
    println!("ADAPTIVE_BUDGET_ORACLE {}", result);
    if let Ok(path) = std::env::var("ADAPTIVE_BUDGET_OUT") {
        if let Some(parent) = Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, serde_json::to_string_pretty(&result).unwrap())
            .expect("write oracle json");
        eprintln!("oracle JSON written to {path}");
    }
}
