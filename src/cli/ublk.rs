//! `entropyfs ublk <run|bench>`: the experimental block frontend
//! (ADR-0020, Phase 7).
//!
//! `run` registers a Linux ublk device backed by the entropy store
//! (requires root + the `ublk_drv` kernel module). `bench` exercises the
//! block adapter directly (no kernel) and reports block throughput.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::time::Instant;

use crate::store::StoreConfig;
use entropyfs::ublk::block::BlockStore;
use entropyfs::ublk::target;

/// Options for `ublk run`.
#[derive(Debug, Clone, clap::Args)]
pub struct UblkRunArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
    /// Device name (the kernel block device is /dev/ublkbN).
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Device capacity in bytes.
    #[arg(long, default_value_t = 1024 * 1024 * 1024)]
    pub size: u64,
    /// Number of IO queues.
    #[arg(long, default_value_t = 1)]
    pub queues: u16,
}

/// Options for `ublk bench`.
#[derive(Debug, Clone, clap::Args)]
pub struct UblkBenchArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
    /// Device name.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Total MiB to write/read.
    #[arg(long, default_value_t = 64)]
    pub size_mib: u64,
}

/// Run the ublk device (requires root + the kernel module).
pub fn run(args: &UblkRunArgs) -> Result<(), String> {
    if args.queues == 0 {
        return Err("queues must be >= 1".into());
    }
    target::run(&args.store, &args.name, args.size, args.queues)
}

/// Benchmark the block adapter without the kernel.
pub fn bench(args: &UblkBenchArgs) -> Result<(), String> {
    let mut dev = BlockStore::open_or_create(
        &args.store,
        &StoreConfig::default(),
        &args.name,
        (args.size_mib.max(1)) * 1024 * 1024,
    )
    .map_err(|e| e.to_string())?;
    let total = args.size_mib * 1024 * 1024;
    let block: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();

    let wstart = Instant::now();
    let mut off = 0u64;
    while off < total {
        let n = dev.write(off, &block).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        off += n;
    }
    let wrote = off;
    dev.flush().map_err(|e| e.to_string())?;
    let wsecs = wstart.elapsed().as_secs_f64();

    let rstart = Instant::now();
    let mut verified = 0u64;
    let mut off = 0u64;
    while off < wrote {
        let data = dev.read(off, 4096).map_err(|e| e.to_string())?;
        if data.is_empty() {
            break;
        }
        if data != block {
            return Err(format!("block verify failed at offset {off}"));
        }
        verified += data.len() as u64;
        off += data.len() as u64;
    }
    let rsecs = rstart.elapsed().as_secs_f64();

    println!(
        "ublk bench: {wrote} bytes written ({:.1} MiB/s), {verified} verified ({:.1} MiB/s)",
        wrote as f64 / wsecs / (1024.0 * 1024.0),
        verified as f64 / rsecs / (1024.0 * 1024.0)
    );
    Ok(())
}
