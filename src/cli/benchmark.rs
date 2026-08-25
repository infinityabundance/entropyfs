//! `entropyfs benchmark <store>`: a reproducible write/read benchmark over
//! a synthetic corpus (§41–45), emitting ablation evidence (spec §43).
//!
//! Every claimed benefit must be attributable: `--ablation-all` runs the
//! same corpus through each candidate configuration and prints a
//! comparison table so savings can be assigned to exact dedup, rANS,
//! base+residual channels, configurational coding, and DSFB ranking.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::time::Instant;

use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{Store, StoreConfig};

/// Options for benchmark.
#[derive(Debug, Clone, clap::Args)]
pub struct BenchmarkArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
    /// Total logical bytes to write (MiB).
    #[arg(long, default_value_t = 64)]
    pub size_mib: u64,
    /// Run a single ablation mode (full | raw | raw-rans | no-dedup |
    /// no-base | no-config | no-rans | no-dsfb).
    #[arg(long)]
    pub ablation: Option<String>,
    /// Run all ablation modes on fresh stores and print the comparison.
    #[arg(long)]
    pub ablation_all: bool,
}

/// One ablation configuration: name + option set.
struct AblationMode {
    name: &'static str,
    options: OptimizeOptions,
}

fn modes() -> Vec<AblationMode> {
    vec![
        AblationMode {
            name: "full",
            options: OptimizeOptions::default(),
        },
        AblationMode {
            name: "raw",
            options: OptimizeOptions::raw_only(),
        },
        AblationMode {
            name: "raw-rans",
            options: OptimizeOptions::raw_rans(),
        },
        AblationMode {
            name: "no-dedup",
            options: OptimizeOptions {
                allow_dedup: false,
                ..Default::default()
            },
        },
        AblationMode {
            name: "no-base",
            options: OptimizeOptions {
                allow_bases: false,
                ..Default::default()
            },
        },
        AblationMode {
            name: "no-config",
            options: OptimizeOptions {
                allow_configurational: false,
                ..Default::default()
            },
        },
        AblationMode {
            name: "no-rans",
            options: OptimizeOptions {
                allow_rans: false,
                ..Default::default()
            },
        },
        AblationMode {
            name: "no-dsfb",
            options: OptimizeOptions {
                allow_dsfb_ranking: false,
                ..Default::default()
            },
        },
    ]
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
    let mut store = Store::open(store_dir, &config).map_err(|e| e.to_string())?;
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
    crate::fsck::ensure_unmounted(&args.store)?;

    if args.ablation_all {
        return run_ablation_table(args);
    }
    if let Some(name) = &args.ablation {
        let mode = modes()
            .into_iter()
            .find(|m| m.name == name)
            .ok_or_else(|| format!("unknown ablation mode '{name}' (full|raw|raw-rans|no-dedup|no-base|no-config|no-rans|no-dsfb)"))?;
        let dir = tempfile::TempDir::new().map_err(|e| e.to_string())?;
        Store::create(dir.path(), &StoreConfig::default(), [0x66; 16])
            .map_err(|e| e.to_string())?;
        let r = run_corpus(dir.path(), args.size_mib, mode.options)?;
        print_run(&r, mode.name);
        return Ok(());
    }

    // Default: full optimization written to the given store (original
    // semantics: reproducible write/read benchmark over the store).
    let config = StoreConfig::default();
    let mut store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;
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

/// Run every ablation mode on a fresh store and print the attribution.
fn run_ablation_table(args: &BenchmarkArgs) -> Result<(), String> {
    let mut rows: Vec<RunResult> = Vec::new();
    for mode in modes() {
        let dir = tempfile::TempDir::new().map_err(|e| e.to_string())?;
        Store::create(dir.path(), &StoreConfig::default(), [0x66; 16])
            .map_err(|e| e.to_string())?;
        let mut r = run_corpus(dir.path(), args.size_mib, mode.options)?;
        r.mode = mode.name;
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
    // Attribution vs full.
    println!("\nsavings attributable to each component (physical bytes vs full):");
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
            "no-config" => "configurational coding",
            "no-rans" => "rANS",
            "no-dsfb" => "DSFB ranking",
            _ => r.mode,
        };
        println!(
            "  {label:<28} {:+12} bytes ({}x)",
            delta,
            (r.physical as f64 / full as f64)
        );
    }
    Ok(())
}
