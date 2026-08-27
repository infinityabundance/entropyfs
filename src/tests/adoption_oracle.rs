//! Phase 12E.13: the object-store adoption court (the embeddable
//! immutable-object engine).
//!
//! # PURPOSE
//!
//! The 12E.13 brief: build a benchmark and correctness court around the
//! stable Engine facade itself — NOT FUSE — to discover an adoption
//! wedge. Workloads are natural immutable-object populations (versioned
//! build artifacts, incremental source trees, container-like layers,
//! near-duplicate generated assets, CI/cache-style object sets,
//! versioned scientific outputs). The court must be allowed to conclude
//! "no compelling 10× pain-point win found yet" — that is a valid
//! result, and the data must not be distorted to produce a headline.
//!
//! # BOUNDARY
//!
//! KNOWS: the public [`Engine`] facade (`put_blob` / `get_blob` /
//! `read_blob_range` / `sync` / `compact` / `metrics`) and nothing
//! deeper — the court is the adoption customer, exactly what 12E.13
//! must prove. NEVER KNOWS: store internals, representation policy, or
//! the persistent format. It changes NO production code.
//!
//! # MODEL
//!
//! Each workload gets its OWN engine store (clean dedup attribution, no
//! cross-workload leakage). Per workload:
//!
//! - PUT phase: every blob via `put_blob` (Ack durability); wall + CPU
//!   deltas; unique-id census (duplicate puts return the same id — the
//!   dedup attribution);
//! - SYNC: one durability barrier (the fsync cost of the batch);
//! - GET phase: every blob read back byte-exact through `get_blob`
//!   (the engine's own hash gate); throughput + per-blob latency
//!   percentiles;
//! - RANGE phase: `read_blob_range` on every 10th blob (a 4 KiB window
//!   at one-third offset), verified byte-for-byte against the source;
//! - SETTLE: `compact` + `metrics` — physical footprint after full
//!   settlement.
//!
//! Baselines: the same blobs written as RAW FILES on the same device
//! (one file per blob, one trailing fsync) — raw put wall and raw
//! physical footprint (== logical). The adoption wedge metric is
//! `footprint_vs_raw = physical_after / logical` per workload.
//!
//! # THE GATE (normative, from the 12E.13 brief)
//!
//! - any workload with `footprint_vs_raw <= 0.10` → a 10×-class
//!   footprint wedge candidate is admitted (the adoption story starts
//!   there);
//! - otherwise → **"no compelling 10× pain-point win found yet"** is
//!   recorded as the valid conclusion, with the best workload noted.
//!
//! Dedup and compression savings are attributed separately: dedup saves
//! logical-vs-unique (the versioned-workload term), compression saves
//! unique-vs-physical (the content term).
//!
//! # HISTORY / EVIDENCE
//!
//! The facade is 12E.1 (v0.7.12); this is the first court that treats
//! the engine as a black-box storage API — the adoption customer view.
//!
//! # RUN (the driver tools/court-adoption.sh orchestrates this)
//!
//! ```text
//! cargo test --release --lib adoption_oracle -- --nocapture
//! ```
//!
//! Prints `ADOPTION_ORACLE <json>`.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::time::Instant;

use tempfile::TempDir;

use crate::engine::{BlobId, Engine, EngineOpenOptions};

/// Self thread CPU seconds via `/proc/self/stat` (utime+stime in USER_HZ
/// ticks) — no libc, no unsafe (same helper as the transport court).
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

/// Deterministic pseudo-random (LCG) helper.
fn lcg(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 16
}

/// Fill `buf` with template lines carrying a (workload, version, index)
/// stamp so near-duplicates share structure but differ deterministically.
fn stamp_lines(buf: &mut Vec<u8>, tag: &str, ver: u64, idx: u64, line: &str) {
    let target = buf.capacity().min(1 << 20);
    while buf.len() < target {
        buf.extend_from_slice(
            format!(
                "{line} // {tag} v{ver} item{idx} seq{seq}\n",
                seq = buf.len() % 4096
            )
            .as_bytes(),
        );
    }
    buf.truncate(target);
}

/// One workload: a named set of immutable blobs (the adoption corpus).
struct Workload {
    name: &'static str,
    blobs: Vec<Vec<u8>>,
}

/// The six brief-mandated workload families. Deterministic.
fn workloads() -> Vec<Workload> {
    let mut out = Vec::new();
    let mut seed: u64 = 0xDEAD_BEEF_CAFE_F00D;

    // 1. Versioned build artifacts: 12 versions × 200 objects; 80% of
    //    each version is byte-identical to the previous (dedup), 20%
    //    carries a version stamp (near-duplicate).
    {
        let mut blobs = Vec::new();
        for ver in 0..12u64 {
            for i in 0..200u64 {
                let mut b = Vec::with_capacity(4096 + ((i * 37) % 60000) as usize);
                if i % 5 == 0 {
                    // the changed fifth: version-stamped object
                    stamp_lines(&mut b, "build", ver, i, "artifact(v){v} = {a} {b} {c}");
                } else {
                    // the stable 80%: identical across versions
                    stamp_lines(&mut b, "build", 0, i, "stable_object = {a} {b} {c}");
                }
                blobs.push(b);
            }
        }
        out.push(Workload {
            name: "build-artifacts",
            blobs,
        });
    }

    // 2. Incremental source trees: 10 versions × 150 files; each file
    //    changes only its version stamp + a few edited lines.
    {
        let mut blobs = Vec::new();
        for ver in 0..10u64 {
            for i in 0..150u64 {
                let mut b = Vec::with_capacity(2048 + ((i * 53) % 24000) as usize);
                stamp_lines(
                    &mut b,
                    "src",
                    ver,
                    i,
                    "fn handler_{i}(ctx: &mut Ctx) -> Result<(), E> { let v = ctx.get({i}); v.map(|_| ()) }",
                );
                // a few genuinely edited lines per version
                for e in 0..(ver % 5) {
                    b.extend_from_slice(
                        format!("// edit {ver}.{e} applied to file {i}\n").as_bytes(),
                    );
                }
                blobs.push(b);
            }
        }
        out.push(Workload {
            name: "source-trees",
            blobs,
        });
    }

    // 3. Container-like layers: 8 layers; each layer keeps 60% of the
    //    previous layer's files byte-identical and adds 40% new.
    {
        let mut blobs = Vec::new();
        let mut kept: Vec<Vec<u8>> = Vec::new();
        for layer in 0..8u64 {
            let mut next_kept: Vec<Vec<u8>> = Vec::new();
            // keep 60% of previous layer's files (or seed files)
            let prev_len = kept.len().max(100);
            for i in 0..prev_len {
                if i % 10 < 6 {
                    let b = if kept.is_empty() {
                        let mut x = Vec::with_capacity(8192);
                        stamp_lines(&mut x, "layer", 0, i as u64, "layer_base_file = {a} {b}");
                        x
                    } else {
                        kept[i].clone()
                    };
                    next_kept.push(b.clone());
                    blobs.push(b);
                }
            }
            // add 40% new
            for i in 0..(prev_len / 10 * 4) {
                let mut b = Vec::with_capacity(8192);
                stamp_lines(&mut b, "layer", layer, i as u64, "layer_new_file = {a} {b}");
                next_kept.push(b.clone());
                blobs.push(b);
            }
            kept = next_kept;
        }
        out.push(Workload {
            name: "container-layers",
            blobs,
        });
    }

    // 4. Near-duplicate generated assets: 50 assets from one template
    //    with per-asset parameters (strong shared structure).
    {
        let mut blobs = Vec::new();
        for i in 0..50u64 {
            let mut b = Vec::with_capacity(16 * 1024);
            stamp_lines(
                &mut b,
                "asset",
                0,
                i,
                "asset {{ id: {i}, palette: [r,g,b], scale: {s}, label: \"gen-{i}\" }}",
            );
            blobs.push(b);
        }
        out.push(Workload {
            name: "generated-assets",
            blobs,
        });
    }

    // 5. CI/cache-style object population: 300 objects; 60% unique, 40%
    //    exact duplicates of earlier ones (cache hits); mixed sizes.
    {
        let mut blobs = Vec::new();
        let mut pool: Vec<Vec<u8>> = Vec::new();
        for i in 0..300u64 {
            if i >= 120 && i % 10 < 4 {
                // duplicate a random earlier object (cache hit)
                let hit = pool[(lcg(&mut seed) as usize) % pool.len()].clone();
                blobs.push(hit);
                continue;
            }
            let mut b = Vec::with_capacity(1024 + ((i * 97) % 63000) as usize);
            stamp_lines(&mut b, "ci", 0, i, "cache_entry = {hash} {status} {bytes}");
            pool.push(b.clone());
            blobs.push(b);
        }
        out.push(Workload {
            name: "ci-cache",
            blobs,
        });
    }

    // 6. Versioned scientific outputs: 6 versions × 20 outputs of
    //    64–512 KiB; mostly stable across versions (high dedup).
    {
        let mut blobs = Vec::new();
        for ver in 0..6u64 {
            for i in 0..20u64 {
                let mut b = Vec::with_capacity(64 * 1024 + ((i * 131) % (448 * 1024)) as usize);
                stamp_lines(
                    &mut b,
                    "science",
                    ver,
                    i,
                    "run {i} metric={m} value={v} timestamp=1787840{ver}00",
                );
                blobs.push(b);
            }
        }
        out.push(Workload {
            name: "scientific-outputs",
            blobs,
        });
    }

    out
}

/// One workload's measured rows (the court's machine-readable surface).
#[derive(Default)]
struct WlRows {
    logical: u64,
    unique: u64,
    puts: u64,
    put_wall_s: f64,
    put_cpu_s: f64,
    sync_wall_s: f64,
    get_wall_s: f64,
    get_cpu_s: f64,
    get_samples: Vec<u64>, // per-blob get latency, µs
    range_wall_s: f64,
    physical_after: u64,
    compact_reclaimed: u64,
    raw_wall_s: f64,
    raw_physical: u64,
}

/// Percentiles (nearest-rank) over a µs sample vector.
fn pct(samples: &mut Vec<u64>, q: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.sort_unstable();
    let i = ((samples.len() - 1) as f64 * q).round() as usize;
    samples[i] as f64
}

fn run_workload(tmp_root: &std::path::Path, wl: &Workload) -> WlRows {
    let mut rows = WlRows::default();

    // --- raw-file baseline (same device, one file per blob) ---------------
    let raw_dir = tmp_root.join(format!("raw-{}", wl.name));
    std::fs::create_dir_all(&raw_dir).expect("raw dir");
    let cpu0 = thread_cpu_seconds();
    let t0 = Instant::now();
    for (i, b) in wl.blobs.iter().enumerate() {
        std::fs::write(raw_dir.join(format!("{i:05}.blob")), b).expect("raw write");
    }
    // one trailing fsync (the batch durability cost)
    let f = std::fs::File::open(&raw_dir).expect("raw dir open");
    f.sync_all().expect("raw fsync");
    rows.raw_wall_s = t0.elapsed().as_secs_f64();
    let _ = thread_cpu_seconds() - cpu0;
    rows.raw_physical = wl.blobs.iter().map(|b| b.len() as u64).sum();
    for b in wl.blobs.iter() {
        let _ = b;
    }

    // --- the engine store --------------------------------------------------
    let store_dir = tmp_root.join(format!("efs-{}", wl.name));
    let engine = Engine::create(&store_dir, &EngineOpenOptions::default()).expect("engine create");

    // PUT phase (Ack durability; the batch is made durable by sync()).
    let cpu0 = thread_cpu_seconds();
    let t0 = Instant::now();
    let mut unique_map: BTreeMap<BlobId, u64> = BTreeMap::new();
    for b in &wl.blobs {
        let id = engine.put_blob(b).expect("put");
        // First-seen only: a duplicate put returns the same id and must
        // NOT re-count the bytes (dedup attribution is logical-vs-unique).
        if !unique_map.contains_key(&id) {
            unique_map.insert(id, b.len() as u64);
        }
    }
    rows.put_wall_s = t0.elapsed().as_secs_f64();
    rows.put_cpu_s = thread_cpu_seconds() - cpu0;
    rows.puts = wl.blobs.len() as u64;
    rows.logical = wl.blobs.iter().map(|b| b.len() as u64).sum();
    rows.unique = unique_map.values().sum();

    // SYNC (durability barrier for the whole batch).
    let t0 = Instant::now();
    engine.sync().expect("sync");
    rows.sync_wall_s = t0.elapsed().as_secs_f64();

    // GET phase: every blob, byte-exact (the engine's own hash gate).
    let cpu0 = thread_cpu_seconds();
    let t0 = Instant::now();
    for (i, b) in wl.blobs.iter().enumerate() {
        let id = crate::engine::BlobId::from(crate::core::extent::ChunkId::of(b));
        let t = Instant::now();
        let got = engine.get_blob(id).expect("get");
        rows.get_samples.push(t.elapsed().as_micros() as u64);
        assert_eq!(&got, b, "workload {} blob {} byte-exact", wl.name, i);
    }
    rows.get_wall_s = t0.elapsed().as_secs_f64();
    rows.get_cpu_s = thread_cpu_seconds() - cpu0;

    // RANGE phase: every 10th blob, 4 KiB window at one-third offset.
    let t0 = Instant::now();
    for (i, b) in wl.blobs.iter().enumerate() {
        if i % 10 != 0 {
            continue;
        }
        let id = crate::engine::BlobId::from(crate::core::extent::ChunkId::of(b));
        let off = (b.len() as u64) / 3;
        let len = 4096usize.min(b.len());
        let want = &b[off as usize..(off as usize + len.min(b.len() - off as usize))];
        let got = engine.read_blob_range(id, off, len).expect("range read");
        assert_eq!(&got, want, "workload {} blob {} range exact", wl.name, i);
    }
    rows.range_wall_s = t0.elapsed().as_secs_f64();

    // SETTLE: compact + metrics (the settled physical footprint).
    let report = engine.compact().expect("compact");
    rows.compact_reclaimed = report.reclaimed_bytes;
    let m = engine.metrics().expect("metrics");
    rows.physical_after = m.accounting.physical_used_bytes;

    engine.close().expect("close");
    rows
}

#[test]
fn adoption_oracle() {
    let tmp = TempDir::new().expect("tmp");
    let wls = workloads();
    let mut rows: BTreeMap<String, WlRows> = BTreeMap::new();
    for wl in &wls {
        rows.insert(wl.name.to_string(), run_workload(tmp.path(), wl));
    }

    // --- the gate ----------------------------------------------------------
    let mut best: Option<(String, f64)> = None;
    let mut details = serde_json::Map::new();
    for (name, r) in &rows {
        let footprint = if r.logical == 0 {
            0.0
        } else {
            r.physical_after as f64 / r.logical as f64
        };
        let dedup_saved = r.logical.saturating_sub(r.unique);
        let put_mbps = if r.put_wall_s <= 0.0 {
            0.0
        } else {
            r.logical as f64 / r.put_wall_s / 1024.0 / 1024.0
        };
        let get_mbps = if r.get_wall_s <= 0.0 {
            0.0
        } else {
            r.logical as f64 / r.get_wall_s / 1024.0 / 1024.0
        };
        let mut s = r.get_samples.clone();
        let get_p = (pct(&mut s, 0.50), pct(&mut s, 0.95), pct(&mut s, 0.99));
        if best.as_ref().map(|(_, f)| footprint < *f).unwrap_or(true) {
            best = Some((name.clone(), footprint));
        }
        details.insert(
            name.clone(),
            serde_json::json!({
                "blobs": r.puts,
                "logical_bytes": r.logical,
                "unique_bytes": r.unique,
                "dedup_saved_bytes": dedup_saved,
                "physical_after_bytes": r.physical_after,
                "footprint_vs_raw": footprint,
                "raw_physical_bytes": r.raw_physical,
                "raw_write_wall_s": r.raw_wall_s,
                "put_wall_s": r.put_wall_s,
                "put_cpu_s": r.put_cpu_s,
                "put_mbps": put_mbps,
                "sync_wall_s": r.sync_wall_s,
                "get_wall_s": r.get_wall_s,
                "get_cpu_s": r.get_cpu_s,
                "get_mbps": get_mbps,
                "get_p50_us": get_p.0,
                "get_p95_us": get_p.1,
                "get_p99_us": get_p.2,
                "range_wall_s": r.range_wall_s,
                "compact_reclaimed_bytes": r.compact_reclaimed,
            }),
        );
    }

    let (best_name, best_footprint) = best.expect("workloads");
    let (verdict, rationale) = if best_footprint <= 0.10 {
        (
            "WEDGE-CANDIDATE",
            format!(
                "workload `{best_name}` reaches footprint {:.3}x of raw (<= 0.10x) — a 10x-class footprint wedge",
                best_footprint
            ),
        )
    } else {
        (
            "NO-10X-WEDGE",
            format!(
                "best workload `{best_name}` footprint {:.3}x of raw (> 0.10x) — no compelling 10x pain-point win found yet (valid conclusion)",
                best_footprint
            ),
        )
    };

    let result = serde_json::json!({
        "schema": "adoption-oracle-v1",
        "workloads": details,
        "decision": { "verdict": verdict, "rationale": rationale },
    });
    println!("ADOPTION_ORACLE {}", result);
    eprintln!(
        "adoption-oracle: best workload `{best_name}` footprint {best_footprint:.3}x of raw -> {verdict}",
    );
    for (name, r) in &rows {
        let footprint = if r.logical == 0 {
            0.0
        } else {
            r.physical_after as f64 / r.logical as f64
        };
        let put_mbps = if r.put_wall_s <= 0.0 {
            0.0
        } else {
            r.logical as f64 / r.put_wall_s / 1048576.0
        };
        let get_mbps = if r.get_wall_s <= 0.0 {
            0.0
        } else {
            r.logical as f64 / r.get_wall_s / 1048576.0
        };
        eprintln!(
            "  {name:20} logical {logical:.2} MiB physical {physical:.2} MiB footprint {footprint:.3}x put {put:.1} MiB/s get {get:.1} MiB/s",
            logical = r.logical as f64 / 1048576.0,
            physical = r.physical_after as f64 / 1048576.0,
            footprint = footprint,
            put = put_mbps,
            get = get_mbps,
        );
    }
}
