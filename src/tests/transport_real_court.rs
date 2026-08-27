//! Phase 12E.11: the SyncIo / UringIo real-device transport court.
//!
//! # PURPOSE
//!
//! The 10F evidence (tmpfs-backed, `fuse-court-*-10f-sync/uring`) measured
//! `UringIo` trailing `SyncIo` by 5–27% on writes and 7–12% on reads —
//! the ~2.3 µs ring submit/wait floor on sub-µs tmpfs I/O — and the
//! default stayed `sync` (the crash-consistency oracle, ADR-0021).
//! Phase 12E.11 reruns the transport court on REAL storage (NVMe +
//! SATA-SSD lanes with a tmpfs control), because the syscall-vs-ring
//! tradeoff shifts when the device latency is microseconds, not
//! sub-microseconds.
//!
//! # BOUNDARY
//!
//! KNOWS: the store write/read/barrier API and the perf phase table.
//! NEVER KNOWS: FUSE, the optimizer's internals, or any policy — this is
//! a measurement phase. It changes NO production code; the gate's only
//! outputs are the sealed evidence and a default-backend recommendation
//! (the driver, `tools/court-transport-real.sh`, turns the JSON rows into
//! the decision record).
//!
//! # MODEL
//!
//! One fresh store per (device, backend) run, driven through the same
//! store API a mounted filesystem would use, with the same foreground
//! policy. Phases:
//!
//! - pure group-commit write (one final durability barrier — the number
//!   is therefore an honest *durable* write rate);
//! - fsync-heavy write (a durability barrier after every 2 MiB flush);
//! - sequential warm read (64 KiB, per-read latency samples);
//! - random read (4 KiB samples at deterministic pseudo-random offsets);
//! - mixed read/write (interleaved 64 KiB ops in a 1 MiB window).
//!
//! CPU is measured as a per-phase DELTA of `/proc/self/stat`
//! utime+stime (USER_HZ ticks) — the units discipline from `src/perf`:
//! wall and CPU answer different questions and must not be conflated.
//!
//! # DECISION GATE (normative, from the 12E.11 brief)
//!
//! - Uring wins robustly across the target workloads on real storage →
//!   consider flipping the default.
//! - Sync wins small-QD, Uring wins high-QD → investigate a
//!   deterministic `auto` policy.
//! - Uring still loses → retain the Sync default.
//!
//! Never flip the default to satisfy a roadmap bullet; `SyncIo` remains
//! the semantic/crash-consistency oracle regardless (ADR-0021).
//!
//! # RESOURCE BOUNDS
//!
//! Work is `TRANSPORT_WORK_MIB` (default 256 MiB) per run; each run makes
//! its own store under the device directory and removes it afterwards.
//! The store's own resource limits (segment cap, optimizer budget) bound
//! everything inside.
//!
//! # FAILURE MODES
//!
//! A lane fails only if the store itself fails (create/commit/barrier/
//! read); the driver records the exact error and distinguishes a real
//! failure from an environment capability waiver (e.g. io_uring blocked
//! by seccomp). Missing env vars fall back to safe defaults so a human
//! can always rerun a single lane by hand.
//!
//! # HISTORY / EVIDENCE
//!
//! Phase 10F (v0.6.2) introduced the `SyncIo`/`UringIo` seam and kept
//! Sync default on tmpfs evidence. Phase 12E.2 made `uring` a Cargo
//! feature (base library = Sync only). This court is the first real-
//! device rerun of that decision.

#![forbid(unsafe_code)]

use std::time::Instant;

use crate::store::inode::Inode;
use crate::store::transaction::CrashHooks;
use crate::store::{Store, StoreConfig};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Self thread CPU seconds via `/proc/self/stat` (utime + stime in
/// USER_HZ clock ticks).
///
/// No libc, no `unsafe` — the crate-wide `forbid(unsafe_code)` rule and
/// the unsafe ledger (only `src/platform/io_uring.rs` may contain
/// `unsafe`) are preserved. `rusage` would be the POSIX-clean way, but
/// libc bindings are not in this crate's dependency policy; the procfs
/// parse is exact for the fields we need.
///
/// # Format note
///
/// `/proc/pid/stat` fields: 1 = pid, 2 = comm (in parens, may contain
/// spaces), 3 = state ... 14 = utime, 15 = stime (clock ticks). After
/// splitting on the first `)` the remaining fields start at state
/// (field 3), so utime is index 11 and stime index 12.
fn thread_cpu_seconds() -> f64 {
    let body = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let Some(rest) = body.split_once(')') else {
        return 0.0;
    };
    let fields: Vec<&str> = rest.1[1..].split_whitespace().collect();
    let utime: f64 = fields.get(11).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let stime: f64 = fields.get(12).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    (utime + stime) / 100.0 // USER_HZ (Linux)
}

/// Write `mib` MiB in 64 KiB chunks as a group-commit batch.
///
/// `fsync_every` = durability barrier cadence in FLUSHES (a flush is 32
/// chunks = 2 MiB):
///
/// - `0` → no mid-stream barrier; ONE final durability barrier makes the
///   reported rate an honest durable-write rate (the ack semantics of
///   the Engine API and the `fsync` contract);
/// - `N` → a durability barrier after every N-th flush (the fsync-heavy
///   knob; `1` = barrier per 2 MiB).
///
/// Returns (MiB/s, wall s, CPU s, physical barrier count). CPU is the
/// phase delta (see [`thread_cpu_seconds`]).
fn write_phase(store: &Store, ino: u64, mib: u64, fsync_every: u64) -> (f64, f64, f64, u64) {
    let options = crate::optimizer::policy::OptimizeOptions::default();
    let cpu_start = thread_cpu_seconds();
    let total = mib * 1024 * 1024;
    let chunk = 64 * 1024usize;
    let batches = total / chunk as u64;
    let wall = Instant::now();
    let mut written = 0u64;
    let mut barriers = 0u64;
    let mut flushes = 0u64;
    let mut batch: Vec<(u64, Vec<u8>)> = Vec::new();
    for b in 0..batches {
        // Four deterministic patterns (compressible / zeros / low-entropy
        // ramp / near-incompressible): the write path must handle the
        // mixed real-world case, not a synthetic single-entropy corpus.
        let pat = (b / 16) % 4;
        let mut data = Vec::with_capacity(chunk);
        match pat {
            0 => {
                for i in 0..chunk {
                    data.push(b'a' + (i % 26) as u8);
                }
            }
            1 => data.resize(chunk, 0),
            2 => {
                for i in 0..chunk {
                    data.push((i % 7) as u8);
                }
            }
            _ => {
                for i in 0..chunk {
                    data.push(((i * 31 + b as usize) % 251) as u8);
                }
            }
        }
        batch.push((written, data));
        written += chunk as u64;
        if batch.len() >= 32 {
            store
                .write_region_batch(ino, &batch, options)
                .expect("write batch");
            batch.clear();
            flushes += 1;
            if fsync_every > 0 && flushes.is_multiple_of(fsync_every) {
                store
                    .durability_barrier(&CrashHooks::none())
                    .expect("barrier");
                barriers += 1;
            }
        }
    }
    if !batch.is_empty() {
        store
            .write_region_batch(ino, &batch, options)
            .expect("write tail");
    }
    if fsync_every == 0 {
        // The pure group-commit phase: the final barrier is the durable
        // acknowledgement, so `write_mbps` is a real durable rate.
        store
            .durability_barrier(&CrashHooks::none())
            .expect("final barrier");
        barriers += 1;
    }
    let wall_s = wall.elapsed().as_secs_f64();
    let cpu_s = thread_cpu_seconds() - cpu_start;
    (
        total as f64 / wall_s / 1024.0 / 1024.0,
        wall_s,
        cpu_s,
        barriers,
    )
}

/// Sequential warm read of `mib` MiB in 64 KiB chunks. Returns
/// (MiB/s, CPU s, p50 µs, p95 µs, p99 µs) over per-read latency.
fn read_phase_seq(store: &Store, ino: u64, mib: u64) -> (f64, f64, f64, f64, f64) {
    let chunk = 64 * 1024usize;
    let total = mib * 1024 * 1024;
    let cpu_start = thread_cpu_seconds();
    let mut samples: Vec<u64> = Vec::new();
    let wall = Instant::now();
    let mut off = 0u64;
    while off < total {
        let t = Instant::now();
        let data = store.read_file(ino, off, chunk as u64).expect("read");
        samples.push(t.elapsed().as_micros() as u64);
        assert_eq!(data.len(), chunk.min((total - off) as usize));
        off += chunk as u64;
    }
    let wall_s = wall.elapsed().as_secs_f64();
    let cpu_s = thread_cpu_seconds() - cpu_start;
    let mbps = total as f64 / wall_s / 1024.0 / 1024.0;
    let (p50, p95, p99) = percentiles(&mut samples);
    (mbps, cpu_s, p50, p95, p99)
}

/// Random 4 KiB reads: 4096 samples at deterministic pseudo-random
/// offsets across a `mib`-MiB span of the file (an LCG; the same seed
/// every run, so the offset sequence is reproducible evidence).
/// Returns (MiB/s, CPU s, p50 µs, p95 µs, p99 µs).
fn read_phase_random(store: &Store, ino: u64, mib: u64) -> (f64, f64, f64, f64, f64) {
    let span = (mib * 1024 * 1024).saturating_sub(4096);
    let samples_n = 4096u64;
    let cpu_start = thread_cpu_seconds();
    let wall = Instant::now();
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15; // fixed seed (splitmix constant)
    let mut samples: Vec<u64> = Vec::with_capacity(samples_n as usize);
    for _ in 0..samples_n {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let off = (x >> 16) % span;
        let t = Instant::now();
        let data = store.read_file(ino, off, 4096).expect("random read");
        samples.push(t.elapsed().as_micros() as u64);
        assert_eq!(data.len(), 4096);
    }
    let wall_s = wall.elapsed().as_secs_f64();
    let cpu_s = thread_cpu_seconds() - cpu_start;
    let mbps = (samples_n * 4096) as f64 / wall_s / 1024.0 / 1024.0;
    let (p50, p95, p99) = percentiles(&mut samples);
    (mbps, cpu_s, p50, p95, p99)
}

/// Sort and return (p50, p95, p99) µs (nearest-rank, matching the store
/// perf-table convention in `src/perf`).
fn percentiles(samples: &mut [u64]) -> (f64, f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    samples.sort_unstable();
    let p = |q: f64| {
        let i = ((samples.len() - 1) as f64 * q).round() as usize;
        samples[i] as f64
    };
    (p(0.50), p(0.95), p(0.99))
}

/// Mixed read/write: interleaved 64 KiB writes and reads inside a 1 MiB
/// window of `ino` (the backend-attributable steady-state mix). Returns
/// (MiB/s, CPU s).
fn mixed_phase(store: &Store, ino: u64, mib: u64) -> (f64, f64) {
    let chunk = 64 * 1024usize;
    let ops = mib * 1024 * 1024 / chunk as u64;
    let cpu_start = thread_cpu_seconds();
    let wall = Instant::now();
    for i in 0..ops {
        let off = (i % 16) * chunk as u64;
        if i % 2 == 0 {
            let mut data = vec![(i % 251) as u8; chunk];
            data[0] = i as u8;
            store.write_region(ino, off, &data).expect("mixed write");
        } else {
            store.read_file(ino, off, chunk as u64).expect("mixed read");
        }
    }
    let wall_s = wall.elapsed().as_secs_f64();
    let cpu_s = thread_cpu_seconds() - cpu_start;
    (
        (ops * chunk as u64) as f64 / wall_s / 1024.0 / 1024.0,
        cpu_s,
    )
}

#[test]
fn transport_real_court() {
    let device = env_or("TRANSPORT_DEVICE", "/dev/shm");
    let backend_s = env_or("TRANSPORT_BACKEND", "sync");
    let backend = crate::store::io::IoBackendKind::parse(&backend_s).expect("backend");
    let work_mib: u64 = env_or("TRANSPORT_WORK_MIB", "256").parse().expect("mib");
    let dir = std::path::PathBuf::from(&device).join(format!(
        "efs-transport-{}-{}",
        backend_s,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("device dir");

    let config = StoreConfig {
        io_backend: backend,
        ..Default::default()
    };
    let store = Store::create(&dir, &config, [0x55; 16]).expect("create");
    {
        let mut tx = store.begin_tx().expect("tx");
        let inode = Inode::new_file(1000, 1000, 0o644);
        // Two inodes: 7 = the group-commit + read + mixed target, 8 = the
        // fsync-heavy target (so the fsync-heavy phase never perturbs the
        // file the read phases measure).
        Store::put_inode_in_tx(&mut tx, 7, &inode).expect("inode 7");
        Store::put_inode_in_tx(&mut tx, 8, &inode).expect("inode 8");
        tx.commit(&CrashHooks::none()).expect("commit");
    }

    // Pure group-commit write (one final durable barrier).
    let (w_mbps, w_wall, w_cpu, w_bar) = write_phase(&store, 7, work_mib, 0);
    // Fsync-heavy: a durability barrier after every 2 MiB flush.
    let (f_mbps, _, f_cpu, f_bar) = write_phase(&store, 8, work_mib / 4, 1);
    // Reads over inode 7: sequential warm + random 4 KiB.
    let (r_mbps, r_cpu, r50, r95, r99) = read_phase_seq(&store, 7, work_mib / 2);
    let (rr_mbps, rr_cpu, rr50, rr95, rr99) = read_phase_random(&store, 7, work_mib / 4);
    // Mixed read/write over the 1 MiB window of inode 7.
    let (m_mbps, m_cpu) = mixed_phase(&store, 7, work_mib / 8);

    // The store's phase rows: the backend-attributable cost surface
    // (prepare / append / barrier_fdatasync / commit_lock_wait / the
    // epoch / checkpoint rows). Cumulative WALL ms + per-row p50/p95/p99.
    let phases: Vec<(String, u64, f64, f64, f64, f64)> = store
        .perf()
        .snapshot()
        .into_iter()
        .map(|row| {
            (
                row.phase.to_string(),
                row.count,
                row.total_ms,
                row.p50_us,
                row.p95_us,
                row.p99_us,
            )
        })
        .collect();

    let result = serde_json::json!({
        "device": device,
        "backend": backend_s,
        "work_mib": work_mib,
        "write_mbps": w_mbps,
        "write_wall_s": w_wall,
        "write_cpu_s": w_cpu,
        "write_barriers": w_bar,
        "write_fsync_every_mbps": f_mbps,
        "write_fsync_cpu_s": f_cpu,
        "write_fsync_barriers": f_bar,
        "read_mbps": r_mbps,
        "read_cpu_s": r_cpu,
        "read_p50_us": r50,
        "read_p95_us": r95,
        "read_p99_us": r99,
        "random_read_mbps": rr_mbps,
        "random_read_cpu_s": rr_cpu,
        "random_read_p50_us": rr50,
        "random_read_p95_us": rr95,
        "random_read_p99_us": rr99,
        "mixed_mbps": m_mbps,
        "mixed_cpu_s": m_cpu,
        "phases": phases,
    });
    println!("TRANSPORT_RESULT {}", result);
    eprintln!(
        "transport: device={device} backend={backend_s} write={w_mbps:.1} MiB/s \
         (cpu {w_cpu:.2}s/{w_wall:.2}s) fsync={f_mbps:.1} MiB/s read={r_mbps:.1} MiB/s \
         p50={r50:.0} p95={r95:.0} p99={r99:.0} µs random={rr_mbps:.1} MiB/s \
         mixed={m_mbps:.1} MiB/s"
    );
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}
