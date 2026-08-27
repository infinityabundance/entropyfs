//! `entropyfs metrics [--json] <store>` (Phase 12E.6): the versioned
//! operational metrics DTO ([`crate::engine::EngineMetrics`]).
//!
//! # PURPOSE
//!
//! Machine-readable store accounting for operators and automation:
//! format identity, byte accounting, the Phase-9H physical
//! reconciliation, GC state, DSFB observer accounting, cache counters,
//! and write-path phase latencies. Every metric is defined in
//! [`METRIC_REGISTRY`] with unit / snapshot-vs-cumulative / scope /
//! reset / authority.
//!
//! # BOUNDARY
//!
//! KNOWS: `Store`'s public accounting accessors. NEVER KNOWS: any write
//! path. Refuses a mounted store (same lock discipline as status/fsck —
//! metrics are a settled-state inspection).
//!
//! # MODEL
//!
//! Opens the store read-only (Phase 12E.3) and collects
//! [`crate::engine::collect_engine_metrics`]; `--json` emits the
//! versioned DTO, otherwise a human-readable summary. Stores without an
//! engine namespace report `blob_count = 0`.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use crate::engine::{EngineError, EngineMetrics, ErrorCode, collect_engine_metrics};
use crate::store::{Store, StoreConfig};

/// Options for metrics.
#[derive(Debug, Clone, clap::Args)]
pub struct MetricsArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
    /// Emit the versioned JSON DTO (schema_version 1).
    #[arg(long)]
    pub json: bool,
}

/// Render the human-readable summary.
fn render_human(m: &EngineMetrics) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "format:        {}.{} (compat 0x{:016x}, ro_compat 0x{:016x}, incompat 0x{:016x}) | {}\n",
        m.format.format_major,
        m.format.format_minor,
        m.format.compat,
        m.format.ro_compat,
        m.format.incompat,
        m.format.io_backend
    ));
    s.push_str(&format!(
        "accounting:    {} B logical, {} B reachable, {} B used of {} B capacity ({} B free), {} objects, {} data records, {} blobs\n",
        m.accounting.logical_bytes,
        m.accounting.reachable_bytes,
        m.accounting.physical_used_bytes,
        m.accounting.physical_capacity_bytes,
        m.accounting.physical_free_bytes,
        m.accounting.object_count,
        m.accounting.data_record_count,
        m.accounting.blob_count
    ));
    s.push_str(&format!(
        "physical:      {} B live, {} B dead-indexed, {} B index-hidden, {} B unindexed, {} B torn, {} B padding, {} B overhead, {} B unexplained\n",
        m.physical.live_bytes,
        m.physical.dead_indexed_bytes,
        m.physical.index_hidden_bytes,
        m.physical.unindexed_bytes,
        m.physical.torn_bytes,
        m.physical.zero_padding_bytes,
        m.physical.format_overhead_bytes,
        m.physical.unexplained_bytes
    ));
    s.push_str(&format!(
        "gc:            {} B unreachable (last-known; refresh via compact/fsck)\n",
        m.gc.unreachable_bytes
    ));
    s.push_str(&format!(
        "dsfb:          {} chunks, {} steps, {} drift, {} slew, {} narrowed, {} candidates\n",
        m.dsfb.tracked_chunks,
        m.dsfb.steps,
        m.dsfb.drift_events,
        m.dsfb.slew_events,
        m.dsfb.narrowed_searches,
        m.dsfb.candidates_evaluated
    ));
    s.push_str(&format!(
        "cache:         {} model hits, {} model misses\n",
        m.cache.model_cache_hits, m.cache.model_cache_misses
    ));
    for p in &m.write_path_phases {
        s.push_str(&format!(
            "phase {:<20} count {}  total {} ms  p50 {} µs  p95 {} µs  p99 {} µs\n",
            p.phase, p.count, p.total_ms, p.p50_us, p.p95_us, p.p99_us
        ));
    }
    s
}

/// Run metrics.
pub fn run(args: &MetricsArgs) -> Result<(), String> {
    // Same lock discipline as status: never read a live (mounted) store.
    if crate::fsck::ensure_unmounted(&args.store).is_err() {
        return Err("store is mounted or otherwise in use (mount lock held); \
                    unmount before metrics"
            .into());
    }
    let config = StoreConfig {
        read_only: true,
        ..Default::default()
    };
    let store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;
    let metrics: EngineMetrics = collect_engine_metrics(&store).map_err(|e: EngineError| {
        // The collector only errors on store-internal failures.
        e.message.to_string()
    })?;
    if args.json {
        println!("{}", metrics.to_json());
    } else {
        print!("{}", render_human(&metrics));
    }
    let _ = ErrorCode::Ok; // (error-class import kept for the CLI contract)
    Ok(())
}
