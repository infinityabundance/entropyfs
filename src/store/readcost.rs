//! Phase-12A read-cost sampling: the Hot-DAG terminalization oracle's
//! instrumentation.
//!
//! # Purpose
//!
//! Collect a bounded [`ReadCostSample`] per materialization so the 12A
//! oracle can answer the phase's one question: **for a reference DAG that
//! is legal and compact, when does its repeated read/materialization cost
//! exceed the storage savings it provides?** The first oracle constructs
//! controlled DAG families (depth 0–4, fanout 1/2/4, cold/warm/hot cache
//! states, ExactRef / BaseResidual / SequenceDict / mixed diamonds) and
//! measures random-read p50/p95/p99 against the samples — the
//! "depth != latency" distinction (a depth-4 chain whose dependencies are
//! hot in memory may be cheaper than a depth-1 representation requiring a
//! large cold fetch) is the hypothesis under test.
//!
//! # Model
//!
//! One sample per materialization, filled by the two halves of the
//! batched read (`Store::materialize_prepare` and
//! `Store::materialize_decode`), carried inside `PreparedRead` so the
//! two-phase FUSE read (guard-held prepare, guard-released decode)
//! completes the same sample. Fields:
//!
//! ```text
//! family              the representation family of the batch's first
//!                     descriptor (the oracle's families are homogeneous,
//!                     so the first is representative)
//! reference_depth     the descriptor's own contribution
//!                     (core::cost::reference_depth: 0 or 1)
//! max_path_depth      the deepest reference-chain level actually walked
//!                     by collect_read_deps (the real chain length)
//! dag_nodes           distinct DAG nodes traversed (top-level descriptors
//!                     + distinct nested descriptors resolved)
//! fanout              max nested children of any single node in the walk
//! referenced_objects  distinct object ids prefetched for the closure
//! bytes_fetched       physical bytes of those objects
//! read_many_submissions   backend fetch submissions (1 in the batched
//!                     path; more if decode-time fallbacks fire)
//! cache_hits/misses   decoded-model cache events (snapshotted as a DELTA
//!                     of the store's atomic counters across the
//!                     materialization — exact for sequential reads,
//!                     approximate under concurrent decodes, which the
//!                     oracle's single-threaded probe never triggers)
//! decode_cpu_ns       decode thread-CPU (exact for the inline single-
//!                     extent path the oracle reads; the requesting
//!                     thread's share for scoped/worker decodes)
//! io_wait_ns          the prefetch's wall time (the read_many)
//! read_latency_ns     total materialization wall (prepare + decode)
//! logical_bytes       materialized output length
//! ```
//!
//! The ring is bounded (FIFO, [`READ_COST_RING`]) so the instrumentation
//! is memory-bounded regardless of read volume.
//!
//! # Boundary
//!
//! Samples are diagnostic and **never persisted, never an authority**:
//! deleting every sample changes nothing. The hotness tracker (below) is
//! the same — advisory read-frequency evidence that a FUTURE
//! terminalizer would consult, kept strictly in-memory until the oracle
//! proves depth/hotness predict latency.
//!
//! # Hotness tracker
//!
//! Exponentially decayed per-chunk-id read-frequency counters
//! (`h ← h·0.9 + 1` per touch), diagnostic and non-persistent. Touched
//! once per distinct referenced object per materialization, so a hot base
//! shared by many consumers accumulates evidence while one-off chunks
//! decay toward zero. The 12A terminalization design (if adopted) would
//! use this to distinguish "cache-first" (retain a hot shared base) from
//! "rewrite-first" (terminalize a costly DAG); the oracle only reports it.
//!
//! # Correctness invariants
//!
//! - A sample never affects read bytes, scheduling, or persisted state.
//! - The ring never grows beyond [`READ_COST_RING`] entries.
//! - Hotness values stay in `[0, ∞)`; decay keeps stale entries bounded in
//!   count by the distinct-chunk set.
//!
//! # Concurrency
//!
//! The ring and the hotness map are each behind their own store-level
//! mutex (bounded, short critical sections — a push/read is a HashMap
//! op). The cache counters are per-store lock-free atomics (bumped by
//! `Store::decode_rans` from any thread, sampled as a delta by the
//! materialization — see the field note above). No thread-local state:
//! the sample travels inside `PreparedRead`, so worker-thread decodes
//! complete the same sample their requesting thread opened.
//!
//! # Resource bounds
//!
//! Ring: [`READ_COST_RING`] fixed-size samples. Hotness: one entry per
//! distinct chunk id touched (bounded by the store's object set; no
//! growth vector beyond distinct content).
//!
//! # Performance
//!
//! Per materialization: one ring push (mutex + Vec push, amortized), one
//! thread-CPU read in the decode half (the same `CLOCK_THREAD_CPUTIME`
//! the 11D worker clock uses), and the hotness touches (one HashMap
//! lookup per distinct dep — already O(deps) work the read must do
//! anyway). The oracle measured no material impact on the read rows it
//! reports (the samples are the measurement, not the perturbation).
//!
//! # Failure modes
//!
//! Infallible by construction. A poisoned ring/hotness mutex panics
//! (loudly, like every other store mutex); samples are advisory, so a
//! panic would be an instrumentation defect, not data corruption.
//!
//! # History / evidence
//!
//! Phase 12A oracle (`docs/performance/dag-read-cost.md`, sealed
//! `evidence/performance/dag-read-cost-probe-*/`): the first oracle
//! measured whether depth predicts read latency once family and cache
//! state are controlled, and decided terminalization's fate (adopted only
//! on a measured latency penalty; `depth > N => RAW` was never a
//! candidate — the brief's explicit rejection).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::time::Instant;

use crate::core::extent::ChunkId;

/// Bounded sample ring length (FIFO; the per-materialization sample
/// stream, capped so read volume cannot grow memory).
pub const READ_COST_RING: usize = 4096;

/// Hotness decay per touch. 0.9: a chunk read once per sweep stays warm
/// for ~20 sweeps; the tracker is clocked by READS (like the DSFB
/// observer is clocked by writes), not wall time.
pub const HOTNESS_DECAY: f64 = 0.9;

/// One materialization's read-cost sample (Phase-12A; fields documented
/// in the module doc).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReadCostSample {
    /// Representation family of the batch's first descriptor
    /// (`Representation::family`; the oracle's families are homogeneous).
    pub family: &'static str,
    /// The descriptor's own reference contribution
    /// (`core::cost::reference_depth`: 0 terminal, 1 reference).
    pub reference_depth: u8,
    /// Deepest reference-chain level actually walked by the dependency
    /// enumeration (the real chain length; depth 0 = terminal).
    pub max_path_depth: u8,
    /// Distinct DAG nodes traversed (top-level descriptors + distinct
    /// nested descriptors resolved through the chunk index).
    pub dag_nodes: u32,
    /// Max nested children of any single node in the dependency walk.
    pub fanout: u32,
    /// Distinct object ids prefetched for the reference closure.
    pub referenced_objects: u32,
    /// Physical bytes of the fetched objects.
    pub bytes_fetched: u64,
    /// Backend fetch submissions (1 in the batched path; decode-time
    /// fallbacks would add).
    pub read_many_submissions: u32,
    /// Decoded-model cache hits during this materialization (delta of the
    /// store's atomic counters; exact for sequential reads).
    pub cache_hits: u32,
    /// Decoded-model cache misses during this materialization (same
    /// delta note).
    pub cache_misses: u32,
    /// Decode thread-CPU (exact for the inline single-extent path; the
    /// requesting thread's share otherwise — documented module limitation).
    pub decode_cpu_ns: u64,
    /// The prefetch's wall time (the one `read_many` submission).
    pub io_wait_ns: u64,
    /// Total materialization wall (prepare + decode).
    pub read_latency_ns: u64,
    /// Materialized output length (logical bytes returned).
    pub logical_bytes: u64,
}

impl Default for ReadCostSample {
    fn default() -> Self {
        Self {
            family: "UNKNOWN",
            reference_depth: 0,
            max_path_depth: 0,
            dag_nodes: 0,
            fanout: 0,
            referenced_objects: 0,
            bytes_fetched: 0,
            read_many_submissions: 0,
            cache_hits: 0,
            cache_misses: 0,
            decode_cpu_ns: 0,
            io_wait_ns: 0,
            read_latency_ns: 0,
            logical_bytes: 0,
        }
    }
}

/// The bounded sample ring (FIFO; [`READ_COST_RING`] entries).
#[derive(Debug, Default)]
pub struct ReadCostRing {
    samples: Vec<ReadCostSample>,
}

impl ReadCostRing {
    /// Push one sample; evicts the oldest past the cap.
    pub fn push(&mut self, s: ReadCostSample) {
        if self.samples.len() >= READ_COST_RING {
            self.samples.remove(0);
        }
        self.samples.push(s);
    }

    /// Snapshot (probe/evidence read; never on the read path).
    pub fn snapshot(&self) -> Vec<ReadCostSample> {
        self.samples.clone()
    }

    /// Reset (per-run isolation in the probe).
    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

/// Exponentially decayed per-chunk-id read-frequency evidence
/// (`h ← h·0.9 + 1` per touch). Diagnostic and non-persistent; a future
/// terminalizer's cache-first/rewrite-first input (module doc).
#[derive(Debug, Default)]
pub struct HotnessTracker {
    entries: HashMap<ChunkId, f64>,
}

impl HotnessTracker {
    /// One read touch for a chunk id (decayed increment).
    pub fn touch(&mut self, id: ChunkId) {
        let h = self.entries.entry(id).or_insert(0.0);
        *h = *h * HOTNESS_DECAY + 1.0;
    }

    /// Current hotness of a chunk id (0 = never read in the decay window).
    pub fn hotness(&self, id: &ChunkId) -> f64 {
        self.entries.get(id).copied().unwrap_or(0.0)
    }

    /// Number of tracked chunk ids.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is tracked.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Reset (per-run isolation).
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// True thread-CPU time (`CLOCK_THREAD_CPUTIME`) on Linux; `None`
/// elsewhere (the same clock the 11D worker oracle uses; wall is the
/// documented fallback where the kernel lacks it).
pub(crate) fn thread_cpu_ns() -> Option<u64> {
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
    #[cfg(target_os = "linux")]
    {
        let t = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
        Some(t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64)
    }
}

/// A start marker for thread-CPU measurement (the decode half's clock).
pub(crate) struct CpuClock {
    cpu: Option<u64>,
    wall: Instant,
}

impl CpuClock {
    /// Start (thread CPU + wall).
    pub(crate) fn start() -> Self {
        Self {
            cpu: thread_cpu_ns(),
            wall: Instant::now(),
        }
    }

    /// Elapsed thread-CPU ns (wall fallback where unavailable).
    pub(crate) fn cpu_ns(&self) -> u64 {
        match self.cpu {
            Some(c0) => thread_cpu_ns().unwrap_or(c0).saturating_sub(c0),
            None => self.wall.elapsed().as_nanos() as u64,
        }
    }
}
