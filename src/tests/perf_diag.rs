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
use crate::optimizer::foreground::ForegroundPolicy;
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
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    store
        .create_entry(
            store.current_root().root_dir_ino,
            format!("f{n}").as_bytes(),
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

fn direct_write_fg(
    store: &Store,
    ino: u64,
    label: &str,
    bytes: &[u8],
    options: OptimizeOptions,
    fg: ForegroundPolicy,
) {
    let t0 = Instant::now();
    store
        .write_region_with_fg(ino, 0, bytes, options, fg)
        .unwrap();
    let dt = t0.elapsed().as_secs_f64();
    let out = store.read_file(ino, 0, bytes.len() as u64).unwrap();
    assert_eq!(out, bytes, "{label} byte-exactness");
    println!(
        "{label:<28} write {:>9.1} MiB/s   ({} bytes)",
        mbps(bytes.len() as u64, dt),
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

    // Phase-10B: the foreground policy comparison on the SAME corpora
    // (fresh inodes per policy so the comparison is not dedup-biased).
    let cheap = ForegroundPolicy::cheap();
    let raw = ForegroundPolicy::raw_only();
    println!("\n-- Phase-10B foreground policy comparison (same bytes, fresh files) --");
    for (label, fg) in [("cheap", cheap), ("raw-only", raw)] {
        for (cname, cbytes) in [("random", &random), ("src", &pack), ("zeros", &zeros)] {
            let f = new_file(&store);
            direct_write_fg(&store, f, &format!("{cname} ({label})"), cbytes, full, fg);
        }
    }
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

/// Phase-10B correctness: the cheap/raw-only foreground policies must be
/// byte-exact (RAW is exact; the content-id gate enforces it), and the
/// background optimizer must recover the density the cheap policy gives
/// up — the foreground-state/settled-state contract.
#[test]
fn foreground_policy_is_byte_exact_and_background_recovers_density() {
    let dir = TempDir::new().unwrap();
    let cfg = StoreConfig {
        segment_size: 128 * 1024 * 1024,
        ..Default::default()
    };
    // Two identical stores: full-policy foreground vs raw-only foreground.
    let store_a = Store::create(dir.path().join("a").as_path(), &cfg, [0x51; 16]).unwrap();
    let store_b = Store::create(dir.path().join("b").as_path(), &cfg, [0x52; 16]).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let pack = crate::evidence::corpus::source_tree_pack(root).unwrap();
    let random = deterministic_noise(4 * 1024 * 1024, 0x1234_5678_9abc_def0);

    let options = OptimizeOptions::default();
    let write_all = |store: &Store, fg: ForegroundPolicy| -> Vec<u64> {
        let mut inos = Vec::new();
        for (name, bytes) in [
            (b"pack".as_slice(), &pack),
            (b"random".as_slice(), &random),
            (b"zeros".as_slice(), &vec![0u8; 1024 * 1024]),
        ] {
            let ino = store
                .create_entry(
                    store.current_root().root_dir_ino,
                    name,
                    NewEntry::file(0o644, 1000, 1000),
                    &CrashHooks::none(),
                )
                .unwrap();
            store
                .write_region_with_fg(ino, 0, bytes, options, fg)
                .unwrap();
            inos.push(ino);
        }
        inos
    };
    let inos_a = write_all(&store_a, ForegroundPolicy::full());
    let inos_b = write_all(&store_b, ForegroundPolicy::raw_only());

    // Byte-exactness under the raw-only policy.
    for (i, ino) in inos_b.iter().enumerate() {
        let bytes = store_b.read_file(*ino, 0, 16 * 1024 * 1024).unwrap();
        let expect = if i == 0 {
            &pack
        } else if i == 1 {
            &random
        } else {
            &vec![0u8; 1024 * 1024]
        };
        assert_eq!(bytes, *expect, "raw-only policy must be byte-exact");
    }

    // The raw-only store is denser after the background optimizer recovers
    // it (settled state): run the full optimize pass on both and compare
    // reachable bytes. The raw-only store must converge to (at most) the
    // full store's settled footprint — the policy defers, never loses.
    for (store, _inos) in [(&store_a, &inos_a), (&store_b, &inos_b)] {
        crate::optimizer::background::optimize_pass(store, options, None, None).unwrap();
    }
    crate::store::gc::collect(&store_a, &CrashHooks::none()).unwrap();
    crate::store::gc::collect(&store_b, &CrashHooks::none()).unwrap();
    let reachable = |s: &Store| -> u64 {
        let records_total: u64 = s
            .object_index()
            .iter()
            .into_iter()
            .map(|(_, loc)| loc.total_size())
            .sum();
        records_total.saturating_sub(crate::store::gc::unreachable_bytes(s).unwrap())
    };
    let a = reachable(&store_a);
    let b = reachable(&store_b);
    println!("settled reachable: full-foreground {a} B vs raw-only-foreground {b} B");
    assert!(
        b <= a.saturating_add(64 * 1024),
        "background recovery must converge the raw-only store: {b} > {a} + 64 KiB"
    );
    // Byte-exactness after recovery.
    for (i, ino) in inos_b.iter().enumerate() {
        let bytes = store_b.read_file(*ino, 0, 16 * 1024 * 1024).unwrap();
        let expect = if i == 0 {
            &pack
        } else if i == 1 {
            &random
        } else {
            &vec![0u8; 1024 * 1024]
        };
        assert_eq!(bytes, *expect, "post-recovery byte-exactness");
    }
    // fsck clean.
    let report = crate::fsck::fsck(
        dir.path().join("b").as_path(),
        &crate::fsck::FsckOptions::default(),
    )
    .unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}
