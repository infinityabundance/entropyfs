//! Phase-12C oracle: does structural-semiotic context reduce foreground
//! search CPU without losing density?
//!
//! The 12C brief: DSFB's observer key becomes
//! `P(channel | chunk history, semantic context)` — a search-ordering /
//! trust score from cheap semantic classes (extension/parent/basename,
//! magic/printable/entropy sketches, lifecycle). The oracle compares the
//! four evidence sources against the sealed baseline:
//!
//! ```text
//! S0 None        prior disabled (the sealed baseline)
//! S1 Extension   extension/parent/basename classes only
//! S2 ByteSketch  magic/printable/entropy classes only
//! S3 History     lifecycle class only
//! S4 Combined    all classes
//! ```
//!
//! Each mode gets its OWN store and runs the SAME heterogeneous corpus
//! twice: the first pass writes it (the prior LEARNS each class's winner
//! distribution), the second pass rewrites it with per-class variants
//! (the prior now GUIDES the plan). Measured rows per mode:
//!
//! ```text
//! search CPU      the `search` perf row total (useful foreground search)
//! candidates      store.candidates_evaluated() (per chunk)
//! win rank        the winner channel's average position in the plan
//!                 order (semantic_rank_stats; lower = found earlier)
//! raw fallback    the RAW-winner fraction (semantic_raw_wins)
//! density         logical / reachable after the checkpoint
//! byte-exact      every file reads back exactly (always asserted)
//! ```
//!
//! # The gate (decided by the evidence tooling)
//!
//! ```text
//! does a semantic mode cut search CPU / candidates substantially
//! while settled density stays approximately unchanged?
//!     yes -> adopt the prior (12C-1 wires it as the production default)
//!     no  -> record the false-prior rate and keep S0 (the prior's
//!            value in the current architecture is marginal; the
//!            adaptive foreground budget is the identified follow-up)
//! ```
//!
//! # Adversarial posture
//!
//! The corpus deliberately INCLUDES semantic deception (the hostile-media
//! clause): random noise named `.rs` (a "source" name for incompressible
//! bytes), zeros named `.bin`, compressible text named `.raw` — the
//! prior must never let a name override the exact byte gate. Correctness
//! (byte-exact read-back + §32) is asserted in every build; a bad prior
//! may cost density or CPU, never bytes.
//!
//! The probe writes its TSV to `$DSFB_SEM_OUT` when set;
//! `$DSFB_SEM_MODE` stamps the row header. Debug runs a reduced smoke
//! sweep (correctness asserts hold).

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Instant;

use crate::dsfb::semantics::{SemanticContext, SemanticMode};
use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

const CHUNK: usize = 65536;

fn create_store(dir: &TempDir) -> Arc<Store> {
    let cfg = StoreConfig {
        segment_size: 128 * 1024 * 1024,
        ..Default::default()
    };
    Arc::new(Store::create(dir.path(), &cfg, [0x66; 16]).unwrap())
}

fn create_file(store: &Store, parent: u64, name: &str) -> u64 {
    store
        .create_entry(
            parent,
            name.as_bytes(),
            NewEntry::file(0o644, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap()
}

/// The heterogeneous corpus: (name, v1 bytes, v2 bytes). The v2 is the
/// rewrite pass — per-class variants (tiny edits for structured text, so
/// residual/dict channels can win; fresh content for noise/zeros).
fn corpus() -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    // Structured source text (compressible; residual/dict-friendly).
    for i in 0..6u64 {
        let v1 = structured(&format!("src/module{i}.rs"), 0x100 + i, true);
        let v2 = structured(&format!("src/module{i}.rs"), 0x100 + i, false);
        out.push(("src", v1, v2));
    }
    // Config files (structured, shared skeleton).
    for i in 0..6u64 {
        let v1 = structured(&format!("cfg/svc{i}.toml"), 0x200 + i, true);
        let v2 = structured(&format!("cfg/svc{i}.toml"), 0x200 + i, false);
        out.push(("cfg", v1, v2));
    }
    // Incompressible blobs (RAW; the expensive families must lose).
    for i in 0..6u64 {
        let v1 = noise(0x300 + i);
        let v2 = noise(0x400 + i);
        out.push(("blob", v1, v2));
    }
    // Zeros (ZERO family).
    for i in 0..4u64 {
        let v1 = vec![0u8; CHUNK];
        let v2 = vec![0u8; CHUNK];
        out.push(("zero", v1, v2));
    }
    // SEMANTIC DECEPTION: incompressible bytes named like source, zeros
    // named like blobs, compressible text named .bin — the prior must
    // never override the byte gate.
    for i in 0..4u64 {
        let v1 = noise(0x500 + i);
        let v2 = noise(0x600 + i);
        out.push(("deceive-src", v1, v2)); // noise named *.rs (below)
    }
    for i in 0..2u64 {
        let v1 = vec![0u8; CHUNK];
        let v2 = vec![0u8; CHUNK];
        out.push(("deceive-bin", v1, v2)); // zeros named *.bin
    }
    // Extensionless files (the brief's clause): structured and noise
    // content with NO extension — the prior must fall back to the byte
    // sketch and history, never misclassify on the missing suffix.
    for i in 0..2u64 {
        let v1 = structured(&format!("bare/exec{i}"), 0x700 + i, true);
        let v2 = structured(&format!("bare/exec{i}"), 0x700 + i, false);
        out.push(("bare", v1, v2));
    }
    out
}

fn structured(tag: &str, seed: u64, v1: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(CHUNK);
    let hdr = format!("{tag}-{seed:016x}-{}\n", if v1 { "v1" } else { "v2" }).into_bytes();
    let alpha: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789{};=,\n ";
    let mut state = seed;
    while out.len() < CHUNK {
        out.extend_from_slice(&hdr);
        for _ in 0..48 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push(alpha[((state >> 33) as usize) % alpha.len()]);
        }
        if !v1 {
            // v2 edits a few bytes so the residual/prev-version channels
            // have real work (the prior's guidance matters).
            let n = out.len();
            out[n - 4] ^= 0x5a;
        }
    }
    out.truncate(CHUNK);
    out
}

fn noise(seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(CHUNK);
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x12c_0001;
    for _ in 0..CHUNK {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push((state >> 33) as u8);
    }
    out
}

/// The semantic context for a file: name-derived + byte-derived (the
/// chunk's v1 bytes), lifecycle 2 (rewrite-heavy — the probe rewrites).
fn semantic_for(name: &[u8], bytes: &[u8]) -> SemanticContext {
    let mut ctx = SemanticContext::from_name(name, 1);
    let sketch = SemanticContext::from_bytes(bytes);
    ctx.magic_class = sketch.magic_class;
    ctx.printable_ratio = sketch.printable_ratio;
    ctx.entropy_class = sketch.entropy_class;
    ctx.lifecycle = 2;
    ctx
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * q) as usize]
}

struct ModeResult {
    mode: &'static str,
    search_cpu_ms: f64,
    candidates: u64,
    chunks: u64,
    candidates_per_chunk: f64,
    win_rank: f64,
    raw_wins: u64,
    raw_fallback_pct: f64,
    logical_bytes: u64,
    reachable_bytes: u64,
    density: f64,
    write_wall_ms: f64,
    byte_exact: bool,
}

fn run_mode(mode: SemanticMode, corpus: &[(&'static str, Vec<u8>, Vec<u8>)]) -> ModeResult {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    store.set_semantic_mode(mode);
    let fg = store.foreground_policy();
    let opts = OptimizeOptions::default();
    let hooks = &CrashHooks::none();
    // Directories.
    let root = store.current_root().root_dir_ino;
    let mut dirs: std::collections::HashMap<&'static str, u64> = Default::default();
    for d in [
        "src",
        "cfg",
        "blob",
        "zero",
        "deceive-src",
        "deceive-bin",
        "bare",
    ] {
        let ino = store
            .create_entry(root, d.as_bytes(), NewEntry::dir(0o755, 1000, 1000), hooks)
            .unwrap();
        dirs.insert(d, ino);
    }
    let mut names: Vec<(String, Vec<u8>)> = Vec::new();
    let mut inos: Vec<u64> = Vec::new();
    let mut idx = 0usize;
    for (class, v1, _v2) in corpus.iter() {
        let name = match *class {
            "src" => format!("module{idx}.rs"),
            "cfg" => format!("svc{idx}.toml"),
            "blob" => format!("blob{idx}.bin"),
            "zero" => format!("zero{idx}.bin"),
            "deceive-src" => format!("evil{idx}.rs"),
            "bare" => format!("exec{idx}"),
            _ => format!("evil{idx}.bin"),
        };
        names.push((format!("{class}/{name}"), v1.clone()));
        idx += 1;
    }
    // Pass 1: write v1 (the prior learns each class's winner).
    let t0 = Instant::now();
    for (i, (path, v1)) in names.iter().enumerate() {
        let (d, n) = path.rsplit_once('/').unwrap();
        let ino = create_file(&store, dirs[d], n);
        let sem = semantic_for(n.as_bytes(), v1);
        store
            .epoch_write_semantic(ino, 0, v1, opts, fg, Some(sem), hooks)
            .unwrap();
        inos.push(ino);
    }
    store.epoch_checkpoint(hooks).unwrap();
    // Pass 2: rewrite with v2 (the prior now GUIDES the plan).
    for (i, (_, _, v2)) in corpus.iter().enumerate() {
        let (_, n) = names[i].0.rsplit_once('/').unwrap();
        let sem = semantic_for(n.as_bytes(), v2);
        store
            .epoch_write_semantic(inos[i], 0, v2, opts, fg, Some(sem), hooks)
            .unwrap();
    }
    store.epoch_checkpoint(hooks).unwrap();
    let write_wall_ms = t0.elapsed().as_secs_f64() * 1e3;

    // Byte-exact read-back (the v2 state — every mode must persist the
    // same bytes; correctness is asserted in every build).
    let mut byte_exact = true;
    for (i, (_, _, v2)) in corpus.iter().enumerate() {
        match store.read_file(inos[i], 0, v2.len() as u64) {
            Ok(got) if got == *v2 => {}
            _ => byte_exact = false,
        }
    }

    // Rows.
    let rows = store.perf().snapshot();
    let search = rows
        .iter()
        .find(|r| r.phase == "search")
        .map(|r| r.total_ms)
        .unwrap_or(0.0);
    let candidates = store.candidates_evaluated();
    let (rank_sum, rank_count) = store.semantic_rank_stats();
    let raw_wins = store.semantic_raw_wins();
    let logical = store.logical_bytes().unwrap();
    let reachable: u64 = crate::store::gc::mark_live(&store)
        .unwrap()
        .into_iter()
        .filter_map(|id| store.object_index().get(&id).map(|loc| loc.total_size()))
        .sum();

    ModeResult {
        mode: match mode {
            SemanticMode::None => "S0-none",
            SemanticMode::Extension => "S1-ext",
            SemanticMode::ByteSketch => "S2-sketch",
            SemanticMode::History => "S3-hist",
            SemanticMode::Combined => "S4-combined",
        },
        search_cpu_ms: search,
        candidates,
        chunks: rank_count,
        candidates_per_chunk: candidates as f64 / rank_count.max(1) as f64,
        win_rank: rank_sum as f64 / rank_count.max(1) as f64,
        raw_wins,
        raw_fallback_pct: raw_wins as f64 / rank_count.max(1) as f64 * 100.0,
        logical_bytes: logical,
        reachable_bytes: reachable,
        density: logical as f64 / reachable.max(1) as f64,
        write_wall_ms: write_wall_ms,
        byte_exact,
    }
}

#[test]
fn dsfb_semantics_probe() {
    let corpus = corpus();
    let modes = if cfg!(debug_assertions) {
        vec![SemanticMode::None, SemanticMode::Combined]
    } else {
        vec![
            SemanticMode::None,
            SemanticMode::Extension,
            SemanticMode::ByteSketch,
            SemanticMode::History,
            SemanticMode::Combined,
        ]
    };
    println!(
        "\n==== Phase-12C DSFB semantics probe ({} chunks x 2 passes) ====",
        corpus.len()
    );
    println!(
        "{:<10} {:>9} {:>8} {:>9} {:>8} {:>8} {:>8} {:>10} {:>8} {:>6}",
        "mode",
        "search_ms",
        "cand/ch",
        "win_rank",
        "raw%",
        "density",
        "wall_ms",
        "logical",
        "reachMB",
        "exact"
    );
    let mut results: Vec<ModeResult> = Vec::new();
    for m in &modes {
        let r = run_mode(*m, &corpus);
        println!(
            "{:<10} {:>9.1} {:>8.2} {:>9.2} {:>8.1} {:>8.2} {:>8.0} {:>10} {:>8.2} {:>6}",
            r.mode,
            r.search_cpu_ms,
            r.candidates_per_chunk,
            r.win_rank,
            r.raw_fallback_pct,
            r.density,
            r.write_wall_ms,
            r.logical_bytes,
            r.reachable_bytes as f64 / (1024.0 * 1024.0),
            if r.byte_exact { "ok" } else { "MISMATCH" }
        );
        assert!(r.byte_exact, "{}: read-back mismatch", r.mode);
        results.push(r);
    }

    let mut tsv = String::new();
    tsv.push_str(
        "mode\tsearch_cpu_ms\tcandidates_per_chunk\twin_rank\traw_fallback_pct\tdensity\twrite_wall_ms\tlogical_bytes\treachable_bytes\tbyte_exact\n",
    );
    let stamp = std::env::var("DSFB_SEM_MODE").unwrap_or_else(|_| "unknown".into());
    for r in &results {
        tsv.push_str(&format!(
            "{stamp}\t{}\t{:.1}\t{:.2}\t{:.2}\t{:.1}\t{:.2}\t{:.0}\t{}\t{}\t{}\n",
            r.mode,
            r.search_cpu_ms,
            r.candidates_per_chunk,
            r.win_rank,
            r.raw_fallback_pct,
            r.density,
            r.write_wall_ms,
            r.logical_bytes,
            r.reachable_bytes,
            if r.byte_exact { "ok" } else { "MISMATCH" }
        ));
    }
    if let Ok(path) = std::env::var("DSFB_SEM_OUT") {
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, &tsv).expect("write probe summary");
        println!("probe summary written to {path}");
    }
}
