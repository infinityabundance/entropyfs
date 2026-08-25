//! `entropyfs benchmark <store>`: a reproducible write/read benchmark
//! over a synthetic corpus (§41–45), emitting a manifest (evidence).

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::time::Instant;

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
}

/// Run benchmark.
pub fn run(args: &BenchmarkArgs) -> Result<(), String> {
    crate::fsck::ensure_unmounted(&args.store)?;
    let config = StoreConfig::default();
    let mut store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;
    let inode = crate::store::inode::Inode::new_file(1000, 1000, 0o644);
    {
        let mut tx = store.begin_tx().map_err(|e| e.to_string())?;
        Store::put_inode_in_tx(&mut tx, 3, &inode).map_err(|e| e.to_string())?;
        tx.commit(&CrashHooks::none()).map_err(|e| e.to_string())?;
    }
    let total = args.size_mib * 1024 * 1024;
    // Mixture: structured text, zeros, low-cardinality, incompressible.
    let mut written = 0u64;
    let mut chunk: Vec<u8> = Vec::with_capacity(64 * 1024);
    let start = Instant::now();
    while written < total {
        chunk.clear();
        let pattern = (written / (1024 * 1024)) % 4;
        match pattern {
            0 => {
                // structured text-ish
                for i in 0..65536u32 {
                    chunk.push(b'a' + (i % 26) as u8);
                }
            }
            1 => {
                // zeros
                chunk.resize(65536, 0);
            }
            2 => {
                // low-cardinality
                for i in 0..65536u32 {
                    chunk.push((i % 7) as u8);
                }
            }
            _ => {
                // incompressible-ish
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

    // Verify + measure materialization.
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
    println!("benchmark: {total} logical bytes");
    println!("write:   {write_mbps:.1} MiB/s");
    println!("read:    {read_mbps:.1} MiB/s (verified {verified} bytes)");
    println!("physical used: {used} bytes");
    if used > 0 {
        println!("effective ratio: {:.3}x", total as f64 / used as f64);
    }
    Ok(())
}
