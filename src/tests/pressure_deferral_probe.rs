//! Phase 12C-1-2: the pressure-aware foreground deferral oracle.
//!
//! # PURPOSE
//!
//! 12C-1 answered \"is rANS valuable for this class?\" (the semantic
//! class-prior gate, adopted as `ForegroundMode::Focused`). 12C-1-2
//! answers the complementary question:
//!
//! ```text
//! \"Even if rANS is valuable, is NOW the right time to pay for it?\"
//! ```
//!
//! The 12C-1-0 frontier gave the empirical permission: raw foreground +
//! background optimization converges to within 0.000–0.618% of full
//! settled density on the adoption corpora, so some foreground search is
//! DEFERRABLE work, not mandatory write-path work. The policy under test:
//!
//! ```text
//! valuable + idle       -> run rANS now
//! valuable + pressured  -> persist the cheap exact representation,
//!                          enqueue explicit optimization debt
//! low-value             -> the class gate skips regardless of pressure
//! background            -> pay the deferred density debt
//! ```
//!
//! with the pressure scalar measured from the STORAGE ENGINE ITSELF (the
//! worker pool's `in_flight / capacity` — the 12C-1-2 brief's \"do not
//! use load average\" rule), a hysteresis band (enter `pressure_enter`,
//! leave `pressure_leave`) against search/skip flapping, and a hard
//! starvation bound (`pressure_max_deferred_bytes` — continuous pressure
//! cannot defer optimization forever).
//!
//! # THE ARMS (the brief's matrix)
//!
//! ```text
//! full      ForegroundMode::Full       the sealed replay anchor
//! focused   the 12C-1 policy           class gate only (pressure off)
//! p25       pressure threshold 0.25    defer rANS when P >= 0.25
//! p50       pressure threshold 0.50
//! p75       pressure threshold 0.75
//! raw       ForegroundMode::RawOnly    the no-search ceiling
//! ```
//!
//! The matrix runs under a sustained `Pressured` condition (P = 0.9 via
//! the probe's deterministic override): `full`/`focused`/`raw` are
//! pressure-independent (Full has no gate; RawOnly has no search;
//! Focused's pressure threshold is 2.0 — never reached), so their rows
//! are byte-comparable to the sealed 12C-1 numbers; only the p* arms
//! respond. A `P50Hyst` arm (enter 0.80 / leave 0.60) and the condition
//! lanes demonstrate the hysteresis + the brief's \"idle ≈ Full, CPU
//! saturated → defers aggressively, pressure clears → background catches
//! up, settled bytes converge\" oracle. A `P75Cap` arm (debt cap 2 MiB)
//! demonstrates the starvation bound: the debt plateaus at the cap and
//! the foreground resumes the search.
//!
//! # THE GATE (from the 12C-1-2 brief; applied by the writeup)
//!
//! ```text
//! byte exactness        absolute (asserted in every arm)
//! settled density       <= +1% preferred, <= +5% hard reject
//! 10x wedge             retained wherever Full had it
//! foreground wall       >= 2x where the 12C-1 frontier says possible,
//!                        else >= 70% of the measured available headroom
//! search CPU            >= 70% of the RawOnly-vs-Full removable
//!                        opportunity captured under pressure
//! p99                   materially improved or neutral
//! background convergence all deferred debt settles
//! starvation            no unbounded debt growth
//! raw controls          unchanged
//! ```
//!
//! # COMPARABILITY
//!
//! The corpus is the sealed 12E.13 generators verbatim
//! (`adoption_corpus::workloads`) + the shared noise control
//! (`adoption_corpus::noise_control` — the identical bytes the 12C-1
//! probe used). The runner replays the engine's put protocol (content-id
//! names, fast-dedup lookup, tmp-write-rename) and the sealed settle
//! measurements (GC-only footprint = the sealed 12E.13 measure;
//! post-background footprint = the settled state), so the `full` rows
//! anchor to the sealed courts.
//!
//! # BOUNDARY
//!
//! KNOWS: the store write path, the foreground policy, the pressure
//! signal, and the debt accounting — the 12C-1-2 mechanism surface.
//! NEVER KNOWS: nothing deeper; it changes the production code only via
//! the `Focused` mode's pressure parameters (public policy fields).
//!
//! # RUN
//!
//! ```text
//! cargo test --release --lib pressure_deferral_probe -- --nocapture
//! ```
//!
//! Prints `PRESSURE_DEFERRAL_ORACLE <json>`. `$PRESSURE_DEFERRAL_OUT`
//! writes the JSON (the driver seals it). Debug builds run the reduced
//! matrix (correctness asserts hold; timing is not meaningful).

#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use tempfile::TempDir;

use crate::optimizer::foreground::ForegroundPolicy;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use crate::tests::adoption_corpus::{Workload, noise_control, workloads};

/// The gate's normative targets (the writeup applies them to the rows).
const WALL_TARGET_X: f64 = 2.0;
const SETTLED_REGRESSION_PREFERRED: f64 = 0.01;
const SETTLED_REGRESSION_REJECT: f64 = 0.05;
const SEARCH_CAPTURE_TARGET: f64 = 0.70;
const WALL_CAPTURE_TARGET: f64 = 0.70;
const P99_REGRESSION_LIMIT: f64 = 0.05;
const WEDGE_FOOTPRINT_LIMIT: f64 = 0.10;
/// The starvation-lane debt cap (2 MiB of deferred logical bytes).
const STARVATION_CAP: u64 = 2 * 1024 * 1024;

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

/// A `Focused`-mode policy with the 12C-1-2 pressure parameters set
/// explicitly (the matrix arms; the fields are public policy
/// parameters, not hidden state).
fn pressure_policy(enter: f64, leave: f64, max_deferred: u64) -> ForegroundPolicy {
    ForegroundPolicy {
        pressure_enter: enter,
        pressure_leave: leave,
        pressure_max_deferred_bytes: max_deferred,
        ..ForegroundPolicy::focused()
    }
}

/// One arm of the matrix: the policy + whether the semantic prior is
/// enabled (the `Focused`-mode arms' defining input, as in 12C-1).
struct ArmSpec {
    name: &'static str,
    policy: ForegroundPolicy,
    semantic: bool,
}

fn matrix_arms() -> Vec<ArmSpec> {
    vec![
        ArmSpec {
            name: "full",
            policy: ForegroundPolicy::full(),
            semantic: false,
        },
        ArmSpec {
            name: "focused",
            policy: ForegroundPolicy::focused(),
            semantic: true,
        },
        ArmSpec {
            name: "p25",
            policy: pressure_policy(0.25, 0.25, u64::MAX),
            semantic: true,
        },
        ArmSpec {
            name: "p50",
            policy: pressure_policy(0.50, 0.50, u64::MAX),
            semantic: true,
        },
        ArmSpec {
            name: "p75",
            policy: pressure_policy(0.75, 0.75, u64::MAX),
            semantic: true,
        },
        // The full "expensive representation search" deferral: rANS +
        // configurational (the p50 threshold + the configurational mask;
        // the evidence picks whether this becomes the production shape).
        ArmSpec {
            name: "p50c",
            policy: ForegroundPolicy {
                pressure_defer_configurational: true,
                ..pressure_policy(0.50, 0.50, u64::MAX)
            },
            semantic: true,
        },
        ArmSpec {
            name: "raw",
            policy: ForegroundPolicy::raw_only(),
            semantic: false,
        },
    ]
}

/// The hysteresis arm (enter 0.80 / leave 0.60 — the brief's example).
fn hysteresis_policy(max_deferred: u64) -> ForegroundPolicy {
    pressure_policy(0.80, 0.60, max_deferred)
}

/// The deterministic pressure conditions: `fn(blob_index) -> P`.
/// The probe overrides the store's pressure per blob so the matrix is
/// reproducible; the production signal (the worker pool's live
/// `in_flight / capacity`) is validated separately by the pool test.
fn condition_idle(_i: usize) -> f64 {
    0.0
}
fn condition_pressured(_i: usize) -> f64 {
    0.9
}
fn condition_oscillating(i: usize) -> f64 {
    if i.is_multiple_of(2) { 0.70 } else { 0.80 }
}
fn condition_clearing(i: usize, total: usize) -> f64 {
    if i < total / 2 { 0.9 } else { 0.0 }
}

/// One arm's measured rows for one workload (the 12C-1-2 row set: the
/// 12C-1 rows plus the pressure/debt witnesses).
#[derive(Default)]
struct ArmRows {
    blobs: u64,
    logical: u64,
    unique: u64,
    put_wall_s: f64,
    put_cpu_s: f64,
    search_cpu_ms: f64,
    prepare_cpu_ms: f64,
    candidates: u64,
    chunks: u64,
    candidates_per_chunk: f64,
    win_rank: f64,
    raw_wins: u64,
    raw_pct: f64,
    rans_skips: u64,
    pressure_deferrals: u64,
    debt_extents: u64,
    debt_bytes: u64,
    debt_age_ms: f64,
    pressure_transitions: u64,
    put_latency_us: Vec<u64>,
    physical_after_gc: u64,
    gc_reclaimed: u64,
    physical_after_settle: u64,
    bg_rewrites: u64,
    bg_saved_bytes: u64,
    byte_exact: bool,
}

impl ArmRows {
    fn gc_footprint(&self) -> f64 {
        ratio(self.physical_after_gc as f64, self.logical as f64)
    }
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

/// Run one (workload, arm, pressure-condition) triple in a fresh store,
/// mirroring the engine's put protocol exactly (content-id names,
/// fast-dedup lookup, tmp-write-rename) so the `full` rows anchor to the
/// sealed 12E.13/12C-1 courts. The condition drives the per-blob
/// pressure override (deterministic); the debt is sampled at write-end
/// and the settle phases mirror the sealed measurement (GC-only + the
/// background optimizer).
#[allow(clippy::too_many_arguments)]
fn run_arm(
    root: &Path,
    wl: &Workload,
    arm: &ArmSpec,
    condition: &dyn Fn(usize) -> f64,
    settle: bool,
) -> ArmRows {
    let dir = TempDir::new_in(root).expect("arm tmpdir");
    let store = create_store(&dir);
    let fg = arm.policy;
    let opts = OptimizeOptions::default();
    let hooks = &CrashHooks::none();

    // The Focused-mode arms' defining input: the semantic prior.
    if arm.semantic {
        store.set_semantic_mode(crate::dsfb::semantics::SemanticMode::Combined);
    }

    let root_ino = store.current_root().root_dir_ino;
    let dir_ino = store
        .epoch_create(
            root_ino,
            wl.name.as_bytes(),
            NewEntry::dir(0o755, 1000, 1000),
            hooks,
        )
        .unwrap();

    // ---- PUT phase: the engine's put_blob protocol, mirrored exactly ----
    let mut rows = ArmRows::default();
    let cpu0 = thread_cpu_seconds();
    let t0 = Instant::now();
    let mut inos: Vec<u64> = Vec::with_capacity(wl.blobs.len());
    let mut unique: std::collections::BTreeMap<crate::core::extent::ChunkId, u64> =
        std::collections::BTreeMap::new();
    let mut prev_pressure_state = false;
    for (i, b) in wl.blobs.iter().enumerate() {
        let cid = crate::core::extent::ChunkId::of(b);
        unique.entry(cid).or_insert(b.len() as u64);
        let final_name = crate::engine::BlobId::from(cid).to_string().into_bytes();
        // The deterministic pressure condition for this blob.
        store.set_pressure_override(Some(condition(i)));
        let sem = if arm.semantic {
            let mut s = crate::dsfb::semantics::SemanticContext::from_name(&final_name, 1);
            let sketch = crate::dsfb::semantics::SemanticContext::from_bytes(b);
            s.magic_class = sketch.magic_class;
            s.printable_ratio = sketch.printable_ratio;
            s.entropy_class = sketch.entropy_class;
            s.lifecycle = 0;
            Some(s)
        } else {
            None
        };
        let t = Instant::now();
        {
            let ep = store.epoch();
            match store.dir_lookup_epoch(&ep, dir_ino, &final_name).unwrap() {
                Some(e) if e.d_type == crate::store::directory::dt::DT_REG => {
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
        // The pressure-state transition witness (the hysteresis test's
        // flap counter): only Focused-mode arms sample the state, and
        // only on real writes.
        if arm.semantic {
            let now = store.pressure_state();
            if now != prev_pressure_state {
                rows.pressure_transitions += 1;
                prev_pressure_state = now;
            }
        }
    }
    rows.put_wall_s = t0.elapsed().as_secs_f64();
    rows.put_cpu_s = thread_cpu_seconds() - cpu0;
    rows.logical = wl.blobs.iter().map(|b| b.len() as u64).sum();
    rows.unique = unique.values().sum();
    rows.blobs = wl.blobs.len() as u64;
    store.set_pressure_override(None);

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
    rows.search_cpu_ms = perf
        .iter()
        .find(|r| r.phase == "search")
        .map(|r| r.total_ms)
        .unwrap_or(0.0);
    rows.prepare_cpu_ms = perf
        .iter()
        .find(|r| r.phase == "prepare")
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
    let (de, db, ds) = store.deferred_debt();
    rows.pressure_deferrals = de;
    rows.debt_extents = de;
    rows.debt_bytes = db;
    rows.debt_age_ms = if ds == 0 {
        0.0
    } else {
        (crate::perf::wall_ns() - ds) as f64 / 1e6
    };

    // ---- Settle A: the sealed 12E.13 measurement (GC only) ----
    store.ensure_epoch_flushed(hooks).unwrap();
    rows.gc_reclaimed = crate::store::gc::compact_full(&store, hooks).unwrap();
    rows.physical_after_gc = store.physical_used();

    // ---- Settle B: the background optimizer (pays the debt) ----
    if settle {
        let opt = crate::optimizer::background::optimize_pass(&store, opts, None, None).unwrap();
        let sd = crate::optimizer::background::shared_dict_pass(&store, opts, None).unwrap();
        rows.bg_rewrites = opt.rewritten.saturating_add(sd.rewritten);
        rows.bg_saved_bytes = opt.saved_bytes.saturating_add(sd.saved_bytes);
        store.epoch_checkpoint(hooks).unwrap();
        store.ensure_epoch_flushed(hooks).unwrap();
        crate::store::gc::compact_full(&store, hooks).unwrap();
        rows.physical_after_settle = store.physical_used();
        // A completed optimize_pass reset the debt (all deferred work
        // re-examined). Record the post-settle debt (must be 0) and the
        // peak age before the pass for the convergence/starvation rows.
        let (de2, db2, _) = store.deferred_debt();
        debug_assert_eq!((de2, db2), (0, 0), "debt must settle");
    } else {
        rows.physical_after_settle = rows.physical_after_gc;
    }
    rows
}

fn arm_json(r: &ArmRows) -> serde_json::Value {
    serde_json::json!({
        "blobs": r.blobs,
        "logical_bytes": r.logical,
        "unique_bytes": r.unique,
        "put_wall_s": r.put_wall_s,
        "put_cpu_s": r.put_cpu_s,
        "search_cpu_ms": r.search_cpu_ms,
        "prepare_cpu_ms": r.prepare_cpu_ms,
        "candidates_per_chunk": r.candidates_per_chunk,
        "win_rank": r.win_rank,
        "raw_fallback_pct": r.raw_pct,
        "rans_skips": r.rans_skips,
        "pressure_deferrals": r.pressure_deferrals,
        "debt_extents": r.debt_extents,
        "debt_bytes": r.debt_bytes,
        "debt_age_ms": r.debt_age_ms,
        "pressure_transitions": r.pressure_transitions,
        "put_p50_us": r.p50(),
        "put_p95_us": r.p95(),
        "put_p99_us": r.p99(),
        "gc_footprint": r.gc_footprint(),
        "settled_footprint": r.settled_footprint(),
        "physical_after_gc": r.physical_after_gc,
        "physical_after_settle": r.physical_after_settle,
        "bg_rewrites": r.bg_rewrites,
        "bg_saved_bytes": r.bg_saved_bytes,
        "byte_exact": r.byte_exact,
    })
}

/// The gate comparison of a pressure arm against the full anchor + the
/// raw ceiling: wall gain + ceiling + capture, search-CPU capture,
/// settled/foreground regressions, wedge retention, p99, and the debt
/// witnesses.
fn pressure_comparison(full: &ArmRows, raw: &ArmRows, arm: &ArmRows) -> serde_json::Value {
    let wall_gain = ratio(full.put_wall_s, arm.put_wall_s);
    let wall_ceiling = ratio(full.put_wall_s, raw.put_wall_s);
    let wall_capture = ratio(
        full.put_wall_s - arm.put_wall_s,
        full.put_wall_s - raw.put_wall_s,
    );
    let search_capture = ratio(
        full.search_cpu_ms - arm.search_cpu_ms,
        full.search_cpu_ms - raw.search_cpu_ms,
    );
    let settled_reg = ratio(arm.settled_footprint(), full.settled_footprint()) - 1.0;
    let foreground_reg = ratio(arm.gc_footprint(), full.gc_footprint()) - 1.0;
    let p99_ratio = ratio(arm.p99(), full.p99());
    serde_json::json!({
        "wall_gain_x": wall_gain,
        "wall_ceiling_x": wall_ceiling,
        "wall_capture": wall_capture.clamp(0.0, 1.0),
        "search_capture": search_capture.clamp(0.0, 1.0),
        "settled_regression": settled_reg,
        "foreground_regression": foreground_reg,
        "wedge_retained": arm.settled_footprint() <= WEDGE_FOOTPRINT_LIMIT
            || full.settled_footprint() > WEDGE_FOOTPRINT_LIMIT,
        "p99_ratio": p99_ratio,
        "debt_extents": arm.debt_extents,
        "debt_bytes": arm.debt_bytes,
        "debt_age_ms": arm.debt_age_ms,
        "gate_wall_2x": wall_gain >= WALL_TARGET_X,
        "gate_wall_capture": wall_capture.clamp(0.0, 1.0) >= WALL_CAPTURE_TARGET,
        "gate_search_capture": search_capture.clamp(0.0, 1.0) >= SEARCH_CAPTURE_TARGET,
        "gate_settled_preferred": settled_reg <= SETTLED_REGRESSION_PREFERRED,
        "gate_settled_reject": settled_reg <= SETTLED_REGRESSION_REJECT,
        "gate_p99": p99_ratio <= 1.0 + P99_REGRESSION_LIMIT,
    })
}

#[test]
fn pressure_deferral_probe() {
    let settle = std::env::var("PRESSURE_DEFERRAL_SETTLE")
        .map(|v| v != "0")
        .unwrap_or(true);
    let tmp = TempDir::new().expect("tmp");

    let mut wls = workloads();
    wls.push(noise_control());

    // ---- The matrix: all arms under sustained pressure (P = 0.9). ----
    let arms = matrix_arms();
    let mut details = serde_json::Map::new();
    for wl in &wls {
        let mut per_arm: std::collections::BTreeMap<String, ArmRows> =
            std::collections::BTreeMap::new();
        for arm in &arms {
            let rows = run_arm(tmp.path(), wl, arm, &condition_pressured, settle);
            per_arm.insert(arm.name.to_string(), rows);
        }
        let full = per_arm.get("full");
        let raw = per_arm.get("raw");
        let mut arm_map = serde_json::Map::new();
        let mut comp_map = serde_json::Map::new();
        for (name, rows) in &per_arm {
            arm_map.insert(name.clone(), arm_json(rows));
            if let (Some(f), Some(r)) = (full, raw) {
                if name != "full" && name != "raw" {
                    comp_map.insert(name.clone(), pressure_comparison(f, r, rows));
                }
            }
        }
        details.insert(
            wl.name.to_string(),
            serde_json::json!({
                "blobs": wl.blobs.len(),
                "matrix": arm_map,
                "comparisons": comp_map,
            }),
        );
    }

    // ---- The condition lanes: hysteresis + idle/pressured/clearing on
    // the representative workloads (build-artifacts = search-limited
    // flagship, scientific-outputs = search-limited, noise-control = RAW
    // control). ----
    let lane_wls: Vec<&Workload> = wls
        .iter()
        .filter(|w| {
            matches!(
                w.name,
                "build-artifacts" | "scientific-outputs" | "noise-control"
            )
        })
        .collect();
    let hyst = ArmSpec {
        name: "p50hyst",
        policy: hysteresis_policy(u64::MAX),
        semantic: true,
    };
    let mut lanes = serde_json::Map::new();
    for wl in &lane_wls {
        let mut cond_map = serde_json::Map::new();
        // Idle: the policy must behave close to Full (no deferral).
        let idle = run_arm(tmp.path(), wl, &hyst, &condition_idle, settle);
        // Pressured: defers aggressively (the brief's "CPU saturated").
        let pressured = run_arm(tmp.path(), wl, &hyst, &condition_pressured, settle);
        // Oscillating (0.70/0.80): the hysteresis band must NOT flap.
        let oscillating = run_arm(tmp.path(), wl, &hyst, &condition_oscillating, settle);
        // Clearing: defers early, resumes after the pressure clears.
        let total = wl.blobs.len();
        let clearing = run_arm(
            tmp.path(),
            wl,
            &hyst,
            &|i| condition_clearing(i, total),
            settle,
        );
        cond_map.insert("idle".into(), arm_json(&idle));
        cond_map.insert("pressured".into(), arm_json(&pressured));
        cond_map.insert("oscillating".into(), arm_json(&oscillating));
        cond_map.insert("clearing".into(), arm_json(&clearing));
        lanes.insert(
            wl.name.to_string(),
            serde_json::json!({
                "policy": "p50hyst (enter 0.80 / leave 0.60)",
                "idle_deferrals": idle.pressure_deferrals,
                "pressured_deferrals": pressured.pressure_deferrals,
                "oscillating_transitions": oscillating.pressure_transitions,
                "clearing_deferrals": clearing.pressure_deferrals,
                "clearing_transitions": clearing.pressure_transitions,
                "idle_settled": idle.settled_footprint(),
                "pressured_settled": pressured.settled_footprint(),
                "conditions": cond_map,
            }),
        );
    }

    // ---- The oscillation contrast: the PLAIN p75 (0.75/0.75) flaps
    // under the 0.70/0.80 oscillation; the hysteresis p50hyst does not. ----
    let p75 = ArmSpec {
        name: "p75",
        policy: pressure_policy(0.75, 0.75, u64::MAX),
        semantic: true,
    };
    let ba = wls.iter().find(|w| w.name == "build-artifacts").unwrap();
    let p75_osc = run_arm(tmp.path(), ba, &p75, &condition_oscillating, settle);
    let hyst_osc = run_arm(tmp.path(), ba, &hyst, &condition_oscillating, settle);
    let oscillation = serde_json::json!({
        "workload": "build-artifacts",
        "condition": "oscillating 0.70/0.80",
        "plain_p75_transitions": p75_osc.pressure_transitions,
        "plain_p75_deferrals": p75_osc.pressure_deferrals,
        "hysteresis_p50hyst_transitions": hyst_osc.pressure_transitions,
        "hysteresis_p50hyst_deferrals": hyst_osc.pressure_deferrals,
    });

    // ---- The starvation lane: sustained pressure with a small debt cap
    // (2 MiB). The debt must plateau at the cap and the foreground must
    // resume the search; the settle must converge. ----
    let capped = ArmSpec {
        name: "p25cap",
        policy: pressure_policy(0.25, 0.25, STARVATION_CAP),
        semantic: true,
    };
    let starvation = run_arm(tmp.path(), ba, &capped, &condition_pressured, settle);
    // The unbounded twin for contrast (the same policy, no cap).
    let uncapped = ArmSpec {
        name: "p25uncapped",
        policy: pressure_policy(0.25, 0.25, u64::MAX),
        semantic: true,
    };
    let starvation_uncapped = run_arm(tmp.path(), ba, &uncapped, &condition_pressured, settle);
    let starvation_json = serde_json::json!({
        "workload": "build-artifacts",
        "condition": "sustained pressure P=0.9",
        "policy": "p25, debt cap 2 MiB",
        "debt_bytes": starvation.debt_bytes,
        "debt_extents": starvation.debt_extents,
        // The bound allows the single chunk that crossed the cap (the
        // check runs before the record; the cap itself is never exceeded
        // by more than one chunk — the starvation invariant).
        "debt_bounded": starvation.debt_bytes <= STARVATION_CAP + 64 * 1024,
        "settled_footprint": starvation.settled_footprint(),
        "settled_converged": starvation.settled_footprint() <= WEDGE_FOOTPRINT_LIMIT,
        "uncapped_debt_bytes": starvation_uncapped.debt_bytes,
        "uncapped_debt_extents": starvation_uncapped.debt_extents,
        "rans_skips_capped": starvation.rans_skips,
        "rans_skips_uncapped": starvation_uncapped.rans_skips,
        "byte_exact": starvation.byte_exact && starvation_uncapped.byte_exact,
    });

    // ---- Human table (stderr) ----
    eprintln!(
        "\n==== Phase-12C-1-2 pressure-deferral oracle ({} workloads x {} arms under P=0.9, settle={}) ====",
        wls.len(),
        arms.len(),
        settle
    );
    for wl in &wls {
        let d = &details[wl.name];
        eprintln!("-- {} (matrix, sustained pressure) --", wl.name);
        eprintln!(
            "{:<7} {:>8} {:>8} {:>7} {:>7} {:>7} {:>8} {:>8} {:>6} {:>6} {:>6}",
            "arm",
            "put_ms",
            "search",
            "prep",
            "rank",
            "raw%",
            "gc_foot",
            "settled",
            "debtKB",
            "p99us",
            "exact"
        );
        for arm in &arms {
            let a = &d["matrix"][arm.name];
            eprintln!(
                "{:<7} {:>8.1} {:>8.1} {:>7.1} {:>7.2} {:>7.1} {:>8.3} {:>8.3} {:>6.0} {:>6.0} {:>6}",
                arm.name,
                a["put_wall_s"].as_f64().unwrap_or(0.0) * 1e3,
                a["search_cpu_ms"].as_f64().unwrap_or(0.0),
                a["prepare_cpu_ms"].as_f64().unwrap_or(0.0),
                a["win_rank"].as_f64().unwrap_or(0.0),
                a["raw_fallback_pct"].as_f64().unwrap_or(0.0),
                a["gc_footprint"].as_f64().unwrap_or(0.0),
                a["settled_footprint"].as_f64().unwrap_or(0.0),
                a["debt_bytes"].as_u64().unwrap_or(0) as f64 / 1024.0,
                a["put_p99_us"].as_f64().unwrap_or(0.0),
                if a["byte_exact"].as_bool().unwrap_or(false) {
                    "ok"
                } else {
                    "MISMATCH"
                },
            );
        }
    }
    eprintln!("\n-- condition lanes (p50hyst) --");
    for (wl, l) in &lanes {
        eprintln!(
            "{:20} idle_defer={} pressured_defer={} osc_trans={} clear_defer={} clear_trans={}",
            wl,
            l["idle_deferrals"],
            l["pressured_deferrals"],
            l["oscillating_transitions"],
            l["clearing_deferrals"],
            l["clearing_transitions"],
        );
    }
    eprintln!(
        "-- oscillation contrast: plain p75 transitions {} vs hysteresis p50hyst transitions {} --",
        oscillation["plain_p75_transitions"], oscillation["hysteresis_p50hyst_transitions"]
    );
    eprintln!(
        "-- starvation: capped debt {} B (bounded {}) vs uncapped {} B; settled footprint {} --",
        starvation_json["debt_bytes"],
        starvation_json["debt_bounded"],
        starvation_json["uncapped_debt_bytes"],
        starvation_json["settled_footprint"],
    );

    let result = serde_json::json!({
        "schema": "pressure-deferral-oracle-v1",
        "arms": arms.iter().map(|a| a.name).collect::<Vec<_>>(),
        "settle_passes": settle,
        "gate": {
            "wall_target_x": WALL_TARGET_X,
            "settled_preferred": SETTLED_REGRESSION_PREFERRED,
            "settled_reject": SETTLED_REGRESSION_REJECT,
            "search_capture_target": SEARCH_CAPTURE_TARGET,
            "wall_capture_target": WALL_CAPTURE_TARGET,
            "p99_limit": P99_REGRESSION_LIMIT,
        },
        "workloads": details,
        "condition_lanes": lanes,
        "oscillation_contrast": oscillation,
        "starvation": starvation_json,
    });
    println!("PRESSURE_DEFERRAL_ORACLE {}", result);
    if let Ok(path) = std::env::var("PRESSURE_DEFERRAL_OUT") {
        if let Some(parent) = Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, serde_json::to_string_pretty(&result).unwrap())
            .expect("write oracle json");
        eprintln!("oracle JSON written to {path}");
    }
}

/// Phase 12C-1-2 regression pin: the starvation cap must bound the
/// deferred-logical-bytes debt (the "continuous pressure cannot defer
/// optimization forever" invariant). 300 distinct 16 KiB structured
/// chunks under sustained pressure (P = 0.9) with a 1 MiB cap: the
/// debt must stop growing at the cap and the remaining chunks must run
/// the full search (their rANS is NOT deferred).
#[test]
fn starvation_cap_bounds_debt() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let hooks = &CrashHooks::none();
    store.set_semantic_mode(crate::dsfb::semantics::SemanticMode::Combined);
    store.set_pressure_override(Some(0.9));
    let fg = pressure_policy(0.25, 0.25, 1024 * 1024);
    let opts = OptimizeOptions::default();
    let root = store.current_root().root_dir_ino;
    let dir_ino = store
        .epoch_create(root, b"cap", NewEntry::dir(0o755, 1000, 1000), hooks)
        .unwrap();
    let mut state: u64 = 42;
    for i in 0..300u64 {
        let mut b = Vec::with_capacity(16384);
        while b.len() < 16384 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            b.push(
                b"abcdefghijklmnopqrstuvwxyz0123456789{}();,= \n"[((state >> 33) as usize) % 45],
            );
        }
        let name = format!("f{i}");
        let ino = store
            .epoch_create(
                dir_ino,
                name.as_bytes(),
                NewEntry::file(0o644, 1000, 1000),
                hooks,
            )
            .unwrap();
        let mut sem = crate::dsfb::semantics::SemanticContext::from_name(name.as_bytes(), 1);
        let sketch = crate::dsfb::semantics::SemanticContext::from_bytes(&b);
        sem.magic_class = sketch.magic_class;
        sem.printable_ratio = sketch.printable_ratio;
        sem.entropy_class = sketch.entropy_class;
        store
            .epoch_write_semantic(ino, 0, &b, opts, fg, Some(sem), hooks)
            .unwrap();
    }
    let (de, db, _) = store.deferred_debt();
    eprintln!("cap test: deferred extents {de}, deferred bytes {db}");
    // The debt may overshoot the cap by at most one chunk (the deferral
    // that crossed it), never by the whole corpus.
    assert!(
        db <= 1024 * 1024 + 16384,
        "debt {db} must be bounded by the cap + one chunk"
    );
    assert!(de <= 1024 * 1024 / 16384 + 2, "extents bounded too");
}

/// Phase 12C-1-3 regression pin (the user-named race): debt created
/// DURING a background optimizer pass must survive the pass's completion.
///
/// # The race being pinned
///
/// A naive "reset the debt counters when a pass completes" would clear
/// debt that was deferred AFTER the pass's effective scan frontier — the
/// operator would be told the store is settled when it is not. The
/// generation/cut model (`Store::begin_debt_generation` snapshots the
/// pending debt as the CUT; `Store::complete_debt_generation` subtracts
/// ONLY that snapshot, saturating) keeps during-pass deferrals visible
/// and restarts the age clock at the generation start — the conservative
/// upper bound on the survivors' age (every surviving deferral happened
/// at or after the pass began).
#[test]
fn debt_created_during_optimizer_pass_survives_completion() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let hooks = &CrashHooks::none();
    store.set_semantic_mode(crate::dsfb::semantics::SemanticMode::Combined);
    store.set_pressure_override(Some(0.9));
    // A large cap: every deferral is admitted (the cap's refusal path is
    // the starvation test's job, not this race pin's).
    let fg = pressure_policy(0.25, 0.25, 64 * 1024 * 1024);
    let opts = OptimizeOptions::default();
    let root = store.current_root().root_dir_ino;
    let dir_ino = store
        .epoch_create(root, b"gen", NewEntry::dir(0o755, 1000, 1000), hooks)
        .unwrap();

    // One 16 KiB DISTINCT structured chunk (rANS-valuable text: the
    // pressure gate must want to defer it). The LCG state makes every
    // chunk's bytes distinct, so no fast-dedup lookup masks the gate.
    let mut state: u64 = 7;
    let mut write_chunk = |store: &Arc<Store>, name: &[u8]| {
        let mut b = Vec::with_capacity(16384);
        while b.len() < 16384 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            b.push(
                b"abcdefghijklmnopqrstuvwxyz0123456789{}();,= \n"[((state >> 33) as usize) % 45],
            );
        }
        let ino = store
            .epoch_create(dir_ino, name, NewEntry::file(0o644, 1000, 1000), hooks)
            .unwrap();
        let mut sem = crate::dsfb::semantics::SemanticContext::from_name(name, 1);
        let sketch = crate::dsfb::semantics::SemanticContext::from_bytes(&b);
        sem.magic_class = sketch.magic_class;
        sem.printable_ratio = sketch.printable_ratio;
        sem.entropy_class = sketch.entropy_class;
        store
            .epoch_write_semantic(ino, 0, &b, opts, fg, Some(sem), hooks)
            .unwrap();
    };

    // Phase A: 40 chunks deferred BEFORE the pass begins — the debt the
    // pass's snapshot will take as its generation cut.
    for i in 0..40u64 {
        write_chunk(&store, format!("a{i}").as_bytes());
    }
    let (de0, db0, _) = store.deferred_debt();
    assert!(
        de0 > 0 && db0 > 0,
        "the gate must defer pre-pass debt (de0={de0}, db0={db0})"
    );

    // Phase B: the pass begins — the snapshot IS the cut.
    let gen_start = crate::perf::wall_ns();
    store.begin_debt_generation();

    // Phase C: 20 chunks deferred DURING the pass (racing its frontier).
    for i in 0..20u64 {
        write_chunk(&store, format!("b{i}").as_bytes());
    }
    let (de1, db1, _) = store.deferred_debt();
    assert!(
        db1 > db0,
        "during-pass debt must grow the total (db0={db0}, db1={db1})"
    );

    store.complete_debt_generation();

    // Phase D: ONLY the cut is paid. The during-pass debt survives
    // (single-threaded here, so the surviving debt is exactly the
    // during-pass delta).
    let (de2, db2, ds2) = store.deferred_debt();
    assert_eq!(
        (de2, db2),
        (de1 - de0, db1 - db0),
        "the pass must pay exactly its generation cut, never the debt that raced it"
    );
    assert!(de2 > 0, "during-pass extents must remain visible");
    assert!(db2 > 0, "during-pass bytes must remain visible");
    // The age clock restarts at the generation start (the survivors' age
    // floor — they all happened at or after the pass began).
    assert!(ds2 != 0, "surviving debt keeps the age clock running");
    assert!(
        ds2 >= gen_start,
        "survivors' age floor must be the generation start (ds2={ds2}, gen_start={gen_start})"
    );

    // Phase E: a second generation with NO during-pass deferrals — the
    // completion must settle the debt entirely and reset the age clock.
    store.begin_debt_generation();
    store.complete_debt_generation();
    let (de3, db3, ds3) = store.deferred_debt();
    assert_eq!(
        (de3, db3, ds3),
        (0, 0, 0),
        "a pass with no racing deferrals settles fully (got {de3}/{db3}/{ds3})"
    );
}
