//! Phase-10A direct-`Store` performance diagnostic (no FUSE): ZERO vs
//! random vs source-pack writes, RAW-only foreground control, reads, and
//! the write-path phase timings (where every millisecond goes). Prints
//! with `--nocapture`; diagnostic, not sealed evidence.
//!
//! This isolates the store engine from the FUSE/VFS layer: the mounted
//! court's numbers are the FUSE cost ON TOP of these.

#![forbid(unsafe_code)]

use std::time::Instant;

use crate::core::extent::ChunkId;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 128 * 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x42; 16]).unwrap()
}

fn new_file(store: &Store) -> u64 {
    store
        .create_entry(
            store.current_root().root_dir_ino,
            b"f",
            NewEntry::file(0o644, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap()
}

fn mbps(bytes: u64, secs: f64) -> f64 {
    bytes as f64 / 1048576.0 / secs.max(1e-9)
}

fn direct_write(store: &Store, ino: u64, label: &str, bytes: &[u8], options: OptimizeOptions) {
    let t0 = Instant::now();
    store.write_region_with(ino, 0, bytes, options).unwrap();
    let dt = t0.elapsed().as_secs_f64();
    let t1 = Instant::now();
    let out = store.read_file(ino, 0, bytes.len() as u64).unwrap();
    let dr = t1.elapsed().as_secs_f64();
    assert_eq!(out, bytes, "{label} byte-exactness");
    println!(
        "{label:<28} write {:>9.1} MiB/s   read {:>9.1} MiB/s   ({} bytes)",
        mbps(bytes.len() as u64, dt),
        mbps(bytes.len() as u64, dr),
        bytes.len()
    );
}

fn deterministic_noise(len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = seed;
    for _ in 0..len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push((state >> 33) as u8);
    }
    out
}

#[test]
fn print_direct_store_perf_diag() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let ino = new_file(&store);
    let mib = 1024 * 1024;

    let zeros = vec![0u8; 64 * mib];
    let random = deterministic_noise(64 * mib, 0xdead_beef_cafe_f00d);
    // Source-like content: the src pack (already deterministic).
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let pack = crate::evidence::corpus::source_tree_pack(root).unwrap();

    println!("\n==== Phase-10A direct-Store perf diagnostic ====");
    let full = OptimizeOptions::default();
    let raw_only = OptimizeOptions::raw_only();

    // ZERO: trivial structural path (baseline transaction/I/O overhead).
    direct_write(&store, ino, "ZERO 64MiB (full)", &zeros, full);
    // Random with the FULL search (configurational + rANS + SequenceRans +
    // dict attempts all run before RAW wins).
    direct_write(&store, ino, "random 64MiB (full)", &random, full);
    // Random with the RAW-only foreground control: the same bytes must
    // fall to RAW without the expensive search.
    direct_write(&store, ino, "random 64MiB (raw-only)", &random, raw_only);
    // Zeros again under raw-only (ZERO is configurational, so raw-only
    // stores them RAW — the control's structural-path cost).
    direct_write(&store, ino, "ZERO 64MiB (raw-only)", &zeros, raw_only);
    // Source pack under the full search (the compression-relevant path).
    direct_write(&store, ino, "src pack (full)", &pack, full);

    println!("\n{}", store.perf().render());
    // Chunk-id hashing throughput (the per-chunk cost).
    let t0 = Instant::now();
    for off in (0..pack.len()).step_by(65536) {
        let end = (off + 65536).min(pack.len());
        let _ = ChunkId::of(&pack[off..end]);
    }
    let dt = t0.elapsed().as_secs_f64();
    let chunks = pack.len().div_ceil(65536);
    println!(
        "hash: {chunks} chunks in {dt:.4} s ({:.0} ns/chunk)",
        dt * 1e9 / chunks as f64
    );
}
