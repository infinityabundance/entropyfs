//! `entropyfs benchmark [<store>]`: a reproducible write/read benchmark over
//! a synthetic corpus (§41–45), emitting ablation evidence (spec §43).
//!
//! Every claimed benefit must be attributable: `--ablation-all` runs the
//! same corpus through each candidate configuration and prints a comparison
//! table so savings can be assigned to exact dedup, rANS, base+residual
//! channels, configurational coding, and DSFB ranking.
//!
//! `--campaign <out-root>` runs the full evidence-sealing campaign
//! (`src/evidence/campaign.rs`, methodology §1–§9): repeated runs, exact
//! revision and Cargo.lock, device/kernel/governor context, corpus hashes,
//! representation distributions, physical allocation, result hashes,
//! p50/p95/p99, fsync latency, device writes, GC traffic, baselines and raw
//! outputs, archived under `<out-root>/campaign-<ts>-<rev>/`.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::evidence::campaign::CampaignOptions;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{Store, StoreConfig};

/// Options for benchmark.
#[derive(Debug, Clone, clap::Args)]
pub struct BenchmarkArgs {
    /// Store directory (not required with --campaign).
    #[arg(value_name = "STORE")]
    pub store: Option<PathBuf>,
    /// Total logical bytes to write (MiB).
    #[arg(long, default_value_t = 64)]
    pub size_mib: u64,
    /// Run a single ablation mode: a leave-one-out gate (full | raw |
    /// raw-rans | no-dedup | no-base | no-config | no-rans | no-universe |
    /// no-dsfb | no-temporal) or a cumulative-ladder step (A0-raw …
    /// A8-full+background).
    #[arg(long)]
    pub ablation: Option<String>,
    /// Run all ablation modes (leave-one-out gates + the cumulative
    /// ladder A0-A8) on fresh stores and print both comparison tables.
    #[arg(long)]
    pub ablation_all: bool,
    /// Run the full evidence-sealing campaign, archiving under this
    /// directory (e.g. `evidence/performance`).
    #[arg(long, value_name = "DIR")]
    pub campaign: Option<PathBuf>,
    /// Campaign repetition count for throughput corpora.
    #[arg(long, default_value_t = 5)]
    pub runs: usize,
    /// Repository root for revision/Cargo.lock/source-tree corpus
    /// (defaults to the directory this binary was built from).
    #[arg(long, value_name = "DIR")]
    pub repo_root: Option<PathBuf>,
    /// Campaign scratch directory — must live on the backing storage
    /// device, not tmpfs (default `<repo>/target/campaign-scratch`).
    #[arg(long, value_name = "DIR")]
    pub scratch: Option<PathBuf>,
}

/// Results of one benchmark run.
struct RunResult {
    mode: &'static str,
    logical: u64,
    physical: u64,
    write_mbps: f64,
    read_mbps: f64,
    families: std::collections::BTreeMap<&'static str, u64>,
}

/// Write the synthetic corpus through the given options and measure.
fn run_corpus(
    store_dir: &std::path::Path,
    size_mib: u64,
    options: OptimizeOptions,
) -> Result<RunResult, String> {
    let config = StoreConfig::default();
    let store = Store::open(store_dir, &config).map_err(|e| e.to_string())?;
    let inode = crate::store::inode::Inode::new_file(1000, 1000, 0o644);
    {
        let mut tx = store.begin_tx().map_err(|e| e.to_string())?;
        Store::put_inode_in_tx(&mut tx, 3, &inode).map_err(|e| e.to_string())?;
        tx.commit(&CrashHooks::none()).map_err(|e| e.to_string())?;
    }
    let total = size_mib * 1024 * 1024;
    let mut written = 0u64;
    let mut chunk: Vec<u8> = Vec::with_capacity(64 * 1024);
    let start = Instant::now();
    while written < total {
        chunk.clear();
        let pattern = (written / (1024 * 1024)) % 4;
        match pattern {
            0 => {
                for i in 0..65536u32 {
                    chunk.push(b'a' + (i % 26) as u8);
                }
            }
            1 => chunk.resize(65536, 0),
            2 => {
                for i in 0..65536u32 {
                    chunk.push((i % 7) as u8);
                }
            }
            _ => {
                for i in 0..65536u32 {
                    chunk.push((i.wrapping_mul(2654435761) >> 8) as u8);
                }
            }
        }
        store
            .write_region_with(3, written, &chunk, options)
            .map_err(|e| e.to_string())?;
        written += 65536;
    }
    let write_secs = start.elapsed().as_secs_f64();
    let write_mbps = total as f64 / write_secs / (1024.0 * 1024.0);

    let mstart = Instant::now();
    let mut off = 0u64;
    while off < total {
        let want = 65536u64.min(total - off);
        let data = store.read_file(3, off, want).map_err(|e| e.to_string())?;
        if data.len() as u64 != want {
            return Err("read verification failed".into());
        }
        off += want;
    }
    let read_secs = mstart.elapsed().as_secs_f64();
    let read_mbps = total as f64 / read_secs / (1024.0 * 1024.0);

    let physical = store.physical_used();
    let families = representation_distribution(&store, 3).map_err(|e| e.to_string())?;
    Ok(RunResult {
        mode: "run",
        logical: total,
        physical,
        write_mbps,
        read_mbps,
        families,
    })
}

/// Count representation families across a file's extents.
fn representation_distribution(
    store: &Store,
    ino: u64,
) -> Result<std::collections::BTreeMap<&'static str, u64>, String> {
    let limits = *store.limits();
    let inode = store
        .get_inode(ino)
        .map_err(|e| e.to_string())?
        .ok_or("inode missing")?;
    let root = match inode.data {
        crate::store::inode::InodeData::File { extent_root } => extent_root,
        _ => return Ok(std::collections::BTreeMap::new()),
    };
    let mut counts = std::collections::BTreeMap::new();
    for (_, bytes) in crate::store::extent_tree::scan_all(
        root,
        crate::store::BTREE_ORDER,
        limits.max_fanout,
        store,
    )
    .map_err(|e| e.to_string())?
    {
        if let Ok(d) = crate::format::descriptor::decode(
            &bytes,
            limits.max_descriptor_bytes,
            limits.max_inline_bytes,
            limits.max_palette,
            limits.max_period,
            limits.max_chunk_size,
        ) {
            *counts.entry(d.family()).or_insert(0) += 1;
        }
    }
    Ok(counts)
}

/// Run benchmark.
pub fn run(args: &BenchmarkArgs) -> Result<(), String> {
    if let Some(campaign_dir) = &args.campaign {
        return run_campaign(args, campaign_dir);
    }
    let store_path = args
        .store
        .clone()
        .ok_or("a STORE argument is required (or pass --campaign <dir>)")?;
    crate::fsck::ensure_unmounted(&store_path)?;

    if args.ablation_all {
        return run_ablation_table(args);
    }
    if let Some(name) = &args.ablation {
        // Leave-one-out gates first, then the cumulative-ladder steps
        // (whose A8 step also runs the background optimizer pass).
        let single = OptimizeOptions::ablation_modes()
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(n, o)| (n, o, false));
        let ladder = single.or_else(|| {
            OptimizeOptions::cumulative_ladder_modes()
                .into_iter()
                .find(|(n, _, _)| n == name)
        });
        let (mode_name, options, run_background) = ladder.ok_or_else(|| {
            let names: Vec<&str> = OptimizeOptions::ablation_modes()
                .iter()
                .map(|(n, _)| *n)
                .chain(
                    OptimizeOptions::cumulative_ladder_modes()
                        .iter()
                        .map(|(n, _, _)| *n),
                )
                .collect();
            format!("unknown ablation mode '{name}' ({})", names.join("|"))
        })?;
        let dir = tempfile::TempDir::new().map_err(|e| e.to_string())?;
        Store::create(dir.path(), &StoreConfig::default(), [0x66; 16])
            .map_err(|e| e.to_string())?;
        let mut r = run_corpus(dir.path(), args.size_mib, options)?;
        if run_background {
            // Reopen the store and run the background pass (the A8 step).
            let store =
                Store::open(dir.path(), &StoreConfig::default()).map_err(|e| e.to_string())?;
            crate::optimizer::background::optimize_pass(&store, options, None, None)
                .map_err(|e| e.to_string())?;
            r.physical = store.physical_used();
        }
        r.mode = mode_name;
        print_run(&r, mode_name);
        return Ok(());
    }

    // Default: full optimization written to the given store (original
    // semantics: reproducible write/read benchmark over the store).
    let config = StoreConfig::default();
    let store = Store::open(&store_path, &config).map_err(|e| e.to_string())?;
    let inode = crate::store::inode::Inode::new_file(1000, 1000, 0o644);
    {
        let mut tx = store.begin_tx().map_err(|e| e.to_string())?;
        Store::put_inode_in_tx(&mut tx, 3, &inode).map_err(|e| e.to_string())?;
        tx.commit(&CrashHooks::none()).map_err(|e| e.to_string())?;
    }
    let total = args.size_mib * 1024 * 1024;
    let mut written = 0u64;
    let mut chunk: Vec<u8> = Vec::with_capacity(64 * 1024);
    let start = Instant::now();
    while written < total {
        chunk.clear();
        let pattern = (written / (1024 * 1024)) % 4;
        match pattern {
            0 => {
                for i in 0..65536u32 {
                    chunk.push(b'a' + (i % 26) as u8);
                }
            }
            1 => chunk.resize(65536, 0),
            2 => {
                for i in 0..65536u32 {
                    chunk.push((i % 7) as u8);
                }
            }
            _ => {
                for i in 0..65536u32 {
                    chunk.push((i.wrapping_mul(2654435761) >> 8) as u8);
                }
            }
        }
        store
            .write_region(3, written, &chunk)
            .map_err(|e| e.to_string())?;
        written += 65536;
    }
    let write_secs = start.elapsed().as_secs_f64();
    let write_mbps = total as f64 / write_secs / (1024.0 * 1024.0);

    let mstart = Instant::now();
    let mut verified = 0u64;
    let mut off = 0u64;
    while off < total {
        let want = 65536u64.min(total - off);
        let data = store.read_file(3, off, want).map_err(|e| e.to_string())?;
        assert_eq!(data.len() as u64, want);
        verified += want;
        off += want;
    }
    let read_secs = mstart.elapsed().as_secs_f64();
    let read_mbps = total as f64 / read_secs / (1024.0 * 1024.0);

    let used = store.physical_used();
    let families = representation_distribution(&store, 3).map_err(|e| e.to_string())?;
    // Deferred writes: make the benchmark durable before reporting.
    store
        .durability_barrier(&CrashHooks::none())
        .map_err(|e| e.to_string())?;
    println!("benchmark: {total} logical bytes");
    println!("write:   {write_mbps:.1} MiB/s");
    println!("read:    {read_mbps:.1} MiB/s (verified {verified} bytes)");
    println!("physical used: {used} bytes");
    if used > 0 {
        println!("effective ratio: {:.3}x", total as f64 / used as f64);
    }
    println!("representation distribution:");
    for (fam, count) in &families {
        println!("  {fam}: {count}");
    }
    Ok(())
}

/// The evidence-sealing campaign (methodology §1–§9).
fn run_campaign(args: &BenchmarkArgs, out_root: &Path) -> Result<(), String> {
    let repo_root = args
        .repo_root
        .clone()
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    let scratch = args
        .scratch
        .clone()
        .unwrap_or_else(|| repo_root.join("target").join("campaign-scratch"));
    let opts = CampaignOptions {
        out_root: out_root.to_path_buf(),
        repo_root,
        scratch_dir: scratch,
        runs: args.runs,
        size_mib: args.size_mib,
        cache_state: "warm (page cache retained; every run uses a fresh store)".into(),
        policy_mode: "balanced".into(),
    };
    let dir = crate::evidence::campaign::run(&opts)?;
    println!("campaign complete: {}", dir.display());
    Ok(())
}

fn print_run(r: &RunResult, name: &str) {
    println!(
        "benchmark: {name}: {logical} logical bytes",
        logical = r.logical
    );
    println!("write:   {:.1} MiB/s", r.write_mbps);
    println!("read:    {:.1} MiB/s (verified)", r.read_mbps);
    println!("physical used: {} bytes", r.physical);
    if r.physical > 0 {
        println!(
            "effective ratio: {:.3}x",
            r.logical as f64 / r.physical as f64
        );
    }
    println!("representation distribution:");
    for (fam, count) in &r.families {
        println!("  {fam}: {count}");
    }
}

/// Run every ablation mode on a fresh store and print both attribution
/// tables: the leave-one-out gates (one mechanism disabled at a time) and
/// the strict cumulative ladder A0-A8 (spec §43, methodology §4).
fn run_ablation_table(args: &BenchmarkArgs) -> Result<(), String> {
    let mut rows: Vec<RunResult> = Vec::new();
    for (name, options) in OptimizeOptions::ablation_modes() {
        let dir = tempfile::TempDir::new().map_err(|e| e.to_string())?;
        Store::create(dir.path(), &StoreConfig::default(), [0x66; 16])
            .map_err(|e| e.to_string())?;
        let mut r = run_corpus(dir.path(), args.size_mib, options)?;
        r.mode = name;
        rows.push(r);
    }
    println!(
        "ablation: {size} MiB synthetic corpus (text/zeros/low-cardinality/random)",
        size = args.size_mib
    );
    println!(
        "{:<10} {:>12} {:>14} {:>10} {:>10}",
        "mode", "logical", "physical", "ratio", "write MB/s"
    );
    let full = rows
        .iter()
        .find(|r| r.mode == "full")
        .map(|r| r.physical)
        .unwrap_or(1);
    for r in &rows {
        println!(
            "{:<10} {:>12} {:>14} {:>9.3}x {:>10.1}",
            r.mode,
            r.logical,
            r.physical,
            r.logical as f64 / r.physical as f64,
            r.write_mbps
        );
    }
    // Attribution vs full (leave-one-out: removing a mechanism must make
    // physical bytes grow if the mechanism contributed).
    println!(
        "\nleave-one-out attribution (physical bytes vs full; + means the\nmechanism contributed to density):"
    );
    for r in &rows {
        if r.mode == "full" {
            continue;
        }
        let delta = r.physical as i64 - full as i64;
        let label = match r.mode {
            "raw" => "all structure (RAW alone)",
            "raw-rans" => "rANS over RAW",
            "no-dedup" => "exact dedup",
            "no-base" => "base+residual channels",
            "no-temporal" => "temporal base channels",
            "no-config" => "configurational coding",
            "no-rans" => "rANS",
            "no-universe" => "entropy universes",
            "no-dsfb" => "DSFB ranking",
            _ => r.mode,
        };
        println!(
            "  {label:<28} {:+12} bytes ({}x)",
            delta,
            (r.physical as f64 / full as f64)
        );
    }

    // Cumulative ladder A0-A8: each step adds exactly one mechanism.
    println!(
        "\ncumulative ladder A0-A8 (each step adds one mechanism; A8 also\nruns the background optimizer pass):"
    );
    let mut ladder_rows: Vec<RunResult> = Vec::new();
    for (name, options, run_background) in OptimizeOptions::cumulative_ladder_modes() {
        let dir = tempfile::TempDir::new().map_err(|e| e.to_string())?;
        Store::create(dir.path(), &StoreConfig::default(), [0x66; 16])
            .map_err(|e| e.to_string())?;
        let mut r = run_corpus(dir.path(), args.size_mib, options)?;
        if run_background {
            let store =
                Store::open(dir.path(), &StoreConfig::default()).map_err(|e| e.to_string())?;
            crate::optimizer::background::optimize_pass(&store, options, None, None)
                .map_err(|e| e.to_string())?;
            r.physical = store.physical_used();
        }
        r.mode = name;
        println!(
            "{:<18} {:>12} {:>14} {:>9.3}x {:>10.1}",
            r.mode,
            r.logical,
            r.physical,
            r.logical as f64 / r.physical as f64,
            r.write_mbps
        );
        ladder_rows.push(r);
    }
    // Incremental attribution: the gain each step adds over the previous.
    println!("\nincremental contribution of each ladder step (bytes saved vs the previous step):");
    let mut prev: Option<i64> = None;
    for r in &ladder_rows {
        match prev {
            None => println!("  {:<18} baseline", r.mode),
            Some(p) => {
                let delta = p - r.physical as i64;
                println!(
                    "  {:<18} {:+12} bytes ({:.3}x)",
                    r.mode,
                    delta,
                    r.physical as f64 / p.max(1) as f64
                );
            }
        }
        prev = Some(r.physical as i64);
    }
    Ok(())
}
