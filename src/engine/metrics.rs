//! Engine metrics DTO (Phase 12E.1/12E.6).
//!
//! # PURPOSE
//!
//! The stable, versioned, machine-readable accounting surface of the
//! embeddable [`crate::engine::Engine`]. This module defines the *external
//! DTO* for `Engine::metrics()` and for the `entropyfs metrics --json`
//! CLI — deliberately NOT the internal Rust state types, so the public
//! schema can evolve (by `schema_version`) without leaking store internals.
//!
//! # BOUNDARY
//!
//! KNOWS: precisely-defined aggregates that the store exposes through
//! public accessors (`StoreStats`, `PhysicalReport`, DSFB stats, the perf
//! snapshot). NEVER KNOWS: how any aggregate is computed internally, the
//! record format, or any write path. Adding a field here REQUIRES a
//! definition-table entry (see [`METRIC_REGISTRY`]) and a `schema_version`
//! bump; removing or renaming a field is a breaking schema change.
//!
//! # MODEL
//!
//! Every metric is either a *snapshot* (the value at collection time, e.g.
//! physical bytes used) or *cumulative* (a monotonic counter since the
//! store was opened, e.g. model-cache hits). Units are always explicit in
//! the field name or the registry. The registry (`METRIC_REGISTRY`) is the
//! normative definition of every exposed metric: name, unit, snapshot-vs-
//! cumulative, scope, reset behavior, and authority (which store accessor
//! produced it). A test walks the registry and the DTO together so the
//! documentation cannot drift from the implementation.
//!
//! # PERSISTENT AUTHORITY
//!
//! None. Metrics are diagnostic; nothing here is persisted, and nothing
//! here affects bytes, durability, or recovery.
//!
//! # FAILURE MODES
//!
//! Collection can fail when the underlying store accessor fails (e.g. the
//! physical report against a torn store). The DTO is still returned with
//! the failing section omitted; `Engine::metrics` documents which
//! accessors may be absent. Metrics never panic.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Format identity as recorded in the superblock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatInfo {
    /// On-disk format major (the compatibility contract level).
    pub format_major: u16,
    /// On-disk format minor.
    pub format_minor: u16,
    /// `compat` feature bits set by this store (unknown bits ignorable).
    pub compat: u64,
    /// `ro_compat` feature bits (unknown bits force read-only).
    pub ro_compat: u64,
    /// `incompat` feature bits (unknown bits refuse open).
    pub incompat: u64,
    /// Transport backend in use (`sync` | `uring`) — a runtime choice, not
    /// an on-disk format property.
    pub io_backend: String,
}

/// Core byte-accounting aggregates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingMetrics {
    /// Sum of materialized logical bytes across all reachable inodes
    /// (snapshot; unit: bytes).
    pub logical_bytes: u64,
    /// Sum of physical record bytes of root-reachable objects (snapshot;
    /// unit: bytes).
    pub reachable_bytes: u64,
    /// Sum of segment-file lengths (snapshot; unit: bytes).
    pub physical_used_bytes: u64,
    /// statvfs-based physical capacity of the backing store, capped by any
    /// `capacity_override` (snapshot; unit: bytes).
    pub physical_capacity_bytes: u64,
    /// `physical_capacity − physical_used` (snapshot; unit: bytes).
    pub physical_free_bytes: u64,
    /// Entries in the derived object index (snapshot; unit: objects).
    pub object_count: u64,
    /// Number of reachable data records (snapshot; unit: records).
    pub data_record_count: u64,
    /// Files in the engine blob namespace (snapshot; unit: blobs; O(n) to
    /// collect — every blob is one file under the hidden `.engine` dir).
    pub blob_count: u64,
}

/// Phase-9H physical reconciliation aggregates (independent of the derived
/// index — the index-vs-physical drift surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalMetrics {
    /// Sum of root-reachable canonical record bytes.
    pub live_bytes: u64,
    /// Sum of unreachable-but-indexed record bytes (reclaimable).
    pub dead_indexed_bytes: u64,
    /// Sum of index-hidden record bytes.
    pub index_hidden_bytes: u64,
    /// Sum of unindexed record bytes.
    pub unindexed_bytes: u64,
    /// Sum of torn bytes.
    pub torn_bytes: u64,
    /// Sum of zero padding.
    pub zero_padding_bytes: u64,
    /// Sum of format overhead (magic + unclassified).
    pub format_overhead_bytes: u64,
    /// Physical bytes the reconciliation cannot explain (must be 0 on a
    /// healthy store).
    pub unexplained_bytes: u64,
}

/// Garbage-collection accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcMetrics {
    /// Last-known unreachable (reclaimable) bytes, as recorded in the
    /// store's stats — NOT recomputed on every call (a full mark walk is
    /// O(store)); run `compact()` or the CLI `gc`/`fsck` to refresh
    /// (snapshot; unit: bytes).
    pub unreachable_bytes: u64,
}

/// DSFB advisory-observer accounting (zero decoding authority).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DsfbMetrics {
    /// Chunks currently tracked by the observer (snapshot).
    pub tracked_chunks: usize,
    /// Observer steps (cumulative since open).
    pub steps: u64,
    /// Drift events (cumulative).
    pub drift_events: u64,
    /// Slew events (cumulative).
    pub slew_events: u64,
    /// Narrowed searches (cumulative).
    pub narrowed_searches: u64,
    /// Candidate evaluations across the write path (cumulative).
    pub candidates_evaluated: u64,
}

/// Phase 12C-1-2 pressure-deferral accounting (the operator's
/// optimization-debt witness — advisory; the background optimizer pays
/// the debt).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PressureMetrics {
    /// Whether the foreground pressure gate is currently engaged
    /// (hysteresis-transitioned; snapshot).
    pub pressured: bool,
    /// Cumulative rANS/configurational deferrals by the `Focused` gate
    /// (cumulative since open; class-gate + pressure-gate skips).
    pub rans_skips: u64,
    /// Pending pressure-deferred extents since the last completed
    /// background pass (snapshot; the debt).
    pub deferred_extents: u64,
    /// Pending pressure-deferred logical bytes since the last completed
    /// background pass (snapshot; the debt — the operator's "accepted
    /// writes quickly and has N bytes of optimization debt" number).
    pub deferred_logical_bytes: u64,
    /// Age of the oldest pending deferral (unit: milliseconds since the
    /// first deferral after the last background pass; snapshot).
    pub deferred_age_ms: u64,
}

/// Performance-cache accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheMetrics {
    /// Model-object cache hits (cumulative since open).
    pub model_cache_hits: u64,
    /// Model-object cache misses (cumulative since open).
    pub model_cache_misses: u64,
}

/// One write-path phase latency sample (snapshot of the perf ring).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseMetrics {
    /// Phase name (the stable perf-row key, e.g. `prepare`, `append`,
    /// `commit_lock_wait`, `epoch_wait`).
    pub phase: String,
    /// Sample count (cumulative).
    pub count: u64,
    /// Cumulative total (unit: milliseconds; cumulative).
    pub total_ms: f64,
    /// p50 (unit: microseconds; snapshot over the bounded ring).
    pub p50_us: f64,
    /// p95 (unit: microseconds; snapshot).
    pub p95_us: f64,
    /// p99 (unit: microseconds; snapshot).
    pub p99_us: f64,
}

/// The complete engine metrics DTO. Schema-versioned; add fields with a
/// version bump, never rewrite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineMetrics {
    /// DTO schema version (currently 1).
    pub schema_version: u32,
    /// Format identity.
    pub format: FormatInfo,
    /// Byte accounting.
    pub accounting: AccountingMetrics,
    /// Physical reconciliation.
    pub physical: PhysicalMetrics,
    /// GC accounting.
    pub gc: GcMetrics,
    /// DSFB observer accounting.
    pub dsfb: DsfbMetrics,
    /// Phase 12C-1-2 pressure-deferral accounting (the optimization-debt
    /// witness; advisory).
    pub pressure: PressureMetrics,
    /// Performance-cache accounting.
    pub cache: CacheMetrics,
    /// Write-path phase latencies (only phases with samples are listed).
    pub write_path_phases: Vec<PhaseMetrics>,
}

impl EngineMetrics {
    /// Render as pretty JSON (the `metrics --json` surface).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }
}

/// One row of the normative metric registry.
#[derive(Debug, Clone, Copy)]
pub struct MetricDef {
    /// Stable metric key (`section.field`).
    pub key: &'static str,
    /// Unit, or `count`/`ratio`.
    pub unit: &'static str,
    /// `snapshot` (value at collection time) or `cumulative` (monotonic).
    pub kind: &'static str,
    /// Scope (store / engine / per-phase).
    pub scope: &'static str,
    /// Reset behavior (e.g. `on close` for cumulative counters).
    pub reset: &'static str,
    /// Authority: which store accessor produced the value.
    pub authority: &'static str,
}

/// The normative definition of every exposed metric (12E.6: "Every metric
/// requires: name, unit, snapshot vs cumulative, scope, reset behavior,
/// authority/source"). Extend this list with every DTO field.
pub const METRIC_REGISTRY: &[MetricDef] = &[
    MetricDef {
        key: "format.format_major",
        unit: "version",
        kind: "snapshot",
        scope: "store",
        reset: "never (on-disk)",
        authority: "superblock format_major",
    },
    MetricDef {
        key: "format.format_minor",
        unit: "version",
        kind: "snapshot",
        scope: "store",
        reset: "never (on-disk)",
        authority: "superblock format_minor",
    },
    MetricDef {
        key: "format.compat",
        unit: "bitmask",
        kind: "snapshot",
        scope: "store",
        reset: "never (on-disk)",
        authority: "superblock compat",
    },
    MetricDef {
        key: "format.ro_compat",
        unit: "bitmask",
        kind: "snapshot",
        scope: "store",
        reset: "never (on-disk)",
        authority: "superblock ro_compat",
    },
    MetricDef {
        key: "format.incompat",
        unit: "bitmask",
        kind: "snapshot",
        scope: "store",
        reset: "never (on-disk)",
        authority: "superblock incompat",
    },
    MetricDef {
        key: "format.io_backend",
        unit: "enum (sync|uring)",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "StoreConfig.io_backend",
    },
    MetricDef {
        key: "accounting.logical_bytes",
        unit: "bytes",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "Store::logical_bytes",
    },
    MetricDef {
        key: "accounting.reachable_bytes",
        unit: "bytes",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "StoreStats.reachable_bytes",
    },
    MetricDef {
        key: "accounting.physical_used_bytes",
        unit: "bytes",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "Store::physical_used",
    },
    MetricDef {
        key: "accounting.physical_capacity_bytes",
        unit: "bytes",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "Store::physical_capacity",
    },
    MetricDef {
        key: "accounting.physical_free_bytes",
        unit: "bytes",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "capacity − used (derived)",
    },
    MetricDef {
        key: "accounting.object_count",
        unit: "objects",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "ObjectIndex::len",
    },
    MetricDef {
        key: "accounting.data_record_count",
        unit: "records",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "StoreStats.data_record_count",
    },
    MetricDef {
        key: "accounting.blob_count",
        unit: "blobs",
        kind: "snapshot",
        scope: "engine",
        reset: "at open",
        authority: "engine blob-namespace directory scan (O(n))",
    },
    MetricDef {
        key: "physical.live_bytes",
        unit: "bytes",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "physical_report (Phase-9H)",
    },
    MetricDef {
        key: "physical.dead_indexed_bytes",
        unit: "bytes",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "physical_report (Phase-9H)",
    },
    MetricDef {
        key: "physical.index_hidden_bytes",
        unit: "bytes",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "physical_report (Phase-9H)",
    },
    MetricDef {
        key: "physical.unindexed_bytes",
        unit: "bytes",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "physical_report (Phase-9H)",
    },
    MetricDef {
        key: "physical.torn_bytes",
        unit: "bytes",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "physical_report (Phase-9H)",
    },
    MetricDef {
        key: "physical.zero_padding_bytes",
        unit: "bytes",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "physical_report (Phase-9H)",
    },
    MetricDef {
        key: "physical.format_overhead_bytes",
        unit: "bytes",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "physical_report (Phase-9H)",
    },
    MetricDef {
        key: "physical.unexplained_bytes",
        unit: "bytes",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "physical_report::unexplained",
    },
    MetricDef {
        key: "gc.unreachable_bytes",
        unit: "bytes",
        kind: "snapshot",
        scope: "store",
        reset: "refreshed by compact/gc/fsck",
        authority: "StoreStats.unreachable_bytes (last-known)",
    },
    MetricDef {
        key: "dsfb.tracked_chunks",
        unit: "chunks",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "ShardedStorageObserver stats",
    },
    MetricDef {
        key: "dsfb.steps",
        unit: "events",
        kind: "cumulative",
        scope: "store",
        reset: "at open",
        authority: "ShardedStorageObserver stats",
    },
    MetricDef {
        key: "dsfb.drift_events",
        unit: "events",
        kind: "cumulative",
        scope: "store",
        reset: "at open",
        authority: "ShardedStorageObserver stats",
    },
    MetricDef {
        key: "dsfb.slew_events",
        unit: "events",
        kind: "cumulative",
        scope: "store",
        reset: "at open",
        authority: "ShardedStorageObserver stats",
    },
    MetricDef {
        key: "dsfb.narrowed_searches",
        unit: "searches",
        kind: "cumulative",
        scope: "store",
        reset: "at open",
        authority: "ShardedStorageObserver stats",
    },
    MetricDef {
        key: "dsfb.candidates_evaluated",
        unit: "candidates",
        kind: "cumulative",
        scope: "store",
        reset: "at open",
        authority: "Store::candidates_evaluated",
    },
    MetricDef {
        key: "pressure.pressured",
        unit: "flag",
        kind: "snapshot",
        scope: "store",
        reset: "at open",
        authority: "Store::pressure_state",
    },
    MetricDef {
        key: "pressure.rans_skips",
        unit: "extents",
        kind: "cumulative",
        scope: "store",
        reset: "at open",
        authority: "Store::focused_rans_skips",
    },
    MetricDef {
        key: "pressure.deferred_extents",
        unit: "extents",
        kind: "snapshot",
        scope: "store",
        reset: "at completed background pass",
        authority: "Store::deferred_debt",
    },
    MetricDef {
        key: "pressure.deferred_logical_bytes",
        unit: "bytes",
        kind: "snapshot",
        scope: "store",
        reset: "at completed background pass",
        authority: "Store::deferred_debt",
    },
    MetricDef {
        key: "pressure.deferred_age_ms",
        unit: "milliseconds",
        kind: "snapshot",
        scope: "store",
        reset: "at completed background pass",
        authority: "Store::deferred_debt",
    },
    MetricDef {
        key: "cache.model_cache_hits",
        unit: "objects",
        kind: "cumulative",
        scope: "store",
        reset: "at open",
        authority: "Store::model_cache_hits",
    },
    MetricDef {
        key: "cache.model_cache_misses",
        unit: "objects",
        kind: "cumulative",
        scope: "store",
        reset: "at open",
        authority: "Store::model_cache_misses",
    },
    MetricDef {
        key: "write_path_phases[]",
        unit: "ms (total) / µs (p50/p95/p99)",
        kind: "snapshot+cumulative",
        scope: "per-phase",
        reset: "at open",
        authority: "perf::Timings::snapshot",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dto_json_roundtrip() {
        let m = EngineMetrics {
            schema_version: 1,
            format: FormatInfo {
                format_major: 1,
                format_minor: 0,
                compat: 0,
                ro_compat: 0,
                incompat: 0x8000,
                io_backend: "sync".to_string(),
            },
            accounting: AccountingMetrics {
                logical_bytes: 1024,
                reachable_bytes: 512,
                physical_used_bytes: 2048,
                physical_capacity_bytes: 1 << 30,
                physical_free_bytes: (1 << 30) - 2048,
                object_count: 3,
                data_record_count: 2,
                blob_count: 1,
            },
            physical: PhysicalMetrics {
                live_bytes: 512,
                dead_indexed_bytes: 0,
                index_hidden_bytes: 0,
                unindexed_bytes: 0,
                torn_bytes: 0,
                zero_padding_bytes: 0,
                format_overhead_bytes: 4,
                unexplained_bytes: 0,
            },
            gc: GcMetrics {
                unreachable_bytes: 0,
            },
            dsfb: DsfbMetrics {
                tracked_chunks: 0,
                steps: 0,
                drift_events: 0,
                slew_events: 0,
                narrowed_searches: 0,
                candidates_evaluated: 0,
            },
            pressure: PressureMetrics {
                pressured: false,
                rans_skips: 0,
                deferred_extents: 0,
                deferred_logical_bytes: 0,
                deferred_age_ms: 0,
            },
            cache: CacheMetrics {
                model_cache_hits: 0,
                model_cache_misses: 0,
            },
            write_path_phases: Vec::new(),
        };
        let json = m.to_json();
        let back: EngineMetrics = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn registry_is_nonempty_and_keyed() {
        assert!(!METRIC_REGISTRY.is_empty());
        for def in METRIC_REGISTRY {
            assert!(!def.key.is_empty());
            assert!(def.key.contains('.') || def.key.contains("[]"));
            assert!(matches!(
                def.kind,
                "snapshot" | "cumulative" | "snapshot+cumulative"
            ));
            assert!(!def.authority.is_empty());
        }
    }
}
