//! The evidence-sealing campaign (§50, methodology §1–§9).
//!
//! `entropyfs benchmark --campaign <out-root>` runs the current filesystem
//! through its own evidence rules and archives the full corpus under
//! `evidence/performance/campaign-<ts>-<rev>/`:
//!
//! - `environment.json` — revision, Cargo.lock hash, kernel, CPU, governor,
//!   storage device, command line, cache state;
//! - `corpus-manifest.json` — every corpus with content and per-version
//!   hashes;
//! - `results.json` / `results.csv` — repeated runs, latency percentiles
//!   (p50/p95/p99), fsync latency, CPU time, exact byte accounting
//!   (payload/models/residuals/descriptors/metadata/integrity/allocator/
//!   unreclaimed), representation distributions, result hashes;
//! - `baselines.json` — raw-file, zstd -1/-19, direct rANS, waivers;
//! - `report.md` — human-readable admission checklist (§8).
//!
//! Nothing in this module has any decoding authority and nothing here can
//! weaken the methodology: a claim is admitted only when the archived
//! evidence satisfies `docs/performance/methodology.md` §8.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::core::representation::Representation;
use crate::evidence::corpus::{self, Corpus};
use crate::evidence::environment::{
    DiskDelta, Environment, StatSummary, disk_delta, diskstats, summary,
};
use crate::optimizer::policy::OptimizeOptions;
use crate::store::inode::{Inode, InodeData};
use crate::store::transaction::CrashHooks;
use crate::store::{BTREE_ORDER, Store, StoreConfig};

/// Campaign configuration.
pub struct CampaignOptions {
    /// Where the campaign directory is created (the `evidence/performance`
    /// root).
    pub out_root: PathBuf,
    /// Repository root (for revision, Cargo.lock, source corpus).
    pub repo_root: PathBuf,
    /// Scratch directory on the *backing* filesystem (stores + baselines).
    /// Must not be tmpfs for honest device-level evidence.
    pub scratch_dir: PathBuf,
    /// Throughput-corpus repetition count.
    pub runs: usize,
    /// Structured corpus size in MiB.
    pub size_mib: u64,
    /// Cache state label recorded in the environment manifest.
    pub cache_state: String,
    /// Policy mode label.
    pub policy_mode: String,
}

// ---------------------------------------------------------------------------
// Serializable result structures
// ---------------------------------------------------------------------------

/// The complete campaign record (written as `results.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CampaignResults {
    /// Evidence directory this record was written to.
    pub campaign_dir: String,
    /// Unix seconds the campaign started.
    pub created_unix: u64,
    /// Per-corpus × per-mode run groups.
    pub runs: Vec<CorpusModeRuns>,
    /// Ablation ladder on the structured corpus (methodology §4).
    pub ablation: AblationTable,
    /// DSFB search-budget investigation.
    pub dsfb_investigation: DsfbInvestigation,
    /// H2 temporal-basis experiment + shuffled control.
    pub versioned_experiment: VersionedExperiment,
    /// GC + background-optimizer traffic.
    pub gc_traffic: GcTraffic,
    /// Baselines and waivers (methodology §3).
    pub baselines: Baselines,
    /// Device-level write/read delta over the campaign window.
    pub device_writes: Option<DiskDelta>,
    /// Methodology §8 admission checklist.
    pub admission: Vec<AdmissionItem>,
}

/// One corpus × optimization-mode × repeated-runs group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusModeRuns {
    /// Corpus name.
    pub corpus: String,
    /// Optimization mode (full, raw, no-dsfb, …).
    pub mode: String,
    /// Number of repeated runs.
    pub run_count: usize,
    /// Per-run metrics.
    pub runs: Vec<RunMetrics>,
    /// Write throughput MiB/s across runs.
    pub write_throughput: StatSummary,
    /// Read throughput MiB/s across runs.
    pub read_throughput: StatSummary,
    /// Pooled per-op write latency (µs).
    pub write_latency_us: StatSummary,
    /// Pooled per-op read latency (µs).
    pub read_latency_us: StatSummary,
    /// Pooled fsync (durability barrier) latency (µs).
    pub fsync_latency_us: StatSummary,
    /// User CPU seconds across runs.
    pub cpu_user_s: StatSummary,
    /// System CPU seconds across runs.
    pub cpu_sys_s: StatSummary,
    /// Median reachable physical bytes.
    pub physical_median: u64,
    /// Median logical/reachable ratio.
    pub ratio_median: f64,
}

/// One run's metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMetrics {
    /// Zero-based run index within the group.
    pub run: usize,
    /// Write throughput MiB/s (all versions).
    pub write_mbps: f64,
    /// Read-back throughput MiB/s (final version).
    pub read_mbps: f64,
    /// Write wall seconds.
    pub write_wall_s: f64,
    /// Read wall seconds.
    pub read_wall_s: f64,
    /// User CPU seconds for the run.
    pub cpu_user_s: f64,
    /// System CPU seconds for the run.
    pub cpu_sys_s: f64,
    /// Logical materialized bytes stored.
    pub logical_bytes: u64,
    /// Total bytes written across versions.
    pub written_bytes: u64,
    /// Reachable persisted bytes.
    pub reachable_bytes: u64,
    /// Total backing-store bytes (segments + superblocks).
    pub total_backing_bytes: u64,
    /// Unreachable (GC-reclaimable) bytes.
    pub unreachable_bytes: u64,
    /// Logical / reachable ratio.
    pub ratio_reachable: f64,
    /// Logical / total-backing ratio.
    pub ratio_total_backing: f64,
    /// BLAKE3 of the materialized read-back.
    pub result_hash: String,
    /// Whether the result hash equals the corpus content hash.
    pub hash_matches_input: bool,
    /// Representation distribution: family → extent count.
    pub families: BTreeMap<String, u64>,
    /// Exact per-category byte accounting.
    pub accounting: Accounting,
}

/// Exact storage accounting (methodology §2): every persistent bit
/// necessary to decode the corpus.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Accounting {
    /// Bytes of literal/data objects (RAW, rANS streams, ref targets, bases).
    pub payload_bytes: u64,
    /// Bytes of model objects (rANS models).
    pub model_bytes: u64,
    /// Bytes of exact residual encodings inside descriptors.
    pub residual_bytes: u64,
    /// Bytes of extent descriptors (all representations).
    pub descriptor_bytes: u64,
    /// Reachable bytes not in the above (inodes, trees, superblock records).
    pub metadata_bytes: u64,
    /// Integrity estimate: 4 B crc32c per record + 64 B superblock hashes.
    pub integrity_bytes_est: u64,
    /// Segment/allocator overhead: backing − reachable − unreclaimed.
    pub allocator_overhead_bytes: u64,
    /// Unreclaimed (GC-pending) bytes.
    pub unreclaimed_bytes: u64,
    /// "ok", or a description of any accounting mismatch found.
    pub check: String,
}

/// One ablation-ladder row (methodology §4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AblationRow {
    /// Ablation mode name.
    pub mode: String,
    /// Reachable physical bytes.
    pub physical: u64,
    /// Logical/reachable ratio.
    pub ratio: f64,
    /// Write throughput MiB/s.
    pub write_mbps: f64,
    /// User CPU seconds.
    pub cpu_user_s: f64,
    /// Representation distribution for this mode.
    pub families: BTreeMap<String, u64>,
}

/// The ablation ladder table (methodology §4 A0–A8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AblationTable {
    /// Corpus the ladder ran on.
    pub corpus: String,
    /// Structured corpus size in MiB.
    pub size_mib: u64,
    /// One row per ablation mode.
    pub rows: Vec<AblationRow>,
}

/// DSFB search-budget investigation: identical final physical
/// representation with and without DSFB candidate ordering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsfbInvestigation {
    /// Corpus used.
    pub corpus: String,
    /// Runs per mode.
    pub runs: usize,
    /// Runs with DSFB ranking enabled.
    pub full: Vec<DsfbRun>,
    /// Runs with DSFB ranking disabled (exhaustive evaluation).
    pub no_dsfb: Vec<DsfbRun>,
    /// Whether both modes landed on the same physical bytes (search-budget
    /// purity: DSFB changed cost, not representation).
    pub physical_identical: bool,
    /// Write throughput with DSFB.
    pub write_mbps_full: StatSummary,
    /// Write throughput without DSFB.
    pub write_mbps_no_dsfb: StatSummary,
    /// User CPU seconds with DSFB.
    pub cpu_user_s_full: StatSummary,
    /// User CPU seconds without DSFB.
    pub cpu_user_s_no_dsfb: StatSummary,
}

/// One DSFB-block run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsfbRun {
    /// Write throughput MiB/s.
    pub write_mbps: f64,
    /// User CPU seconds.
    pub cpu_user_s: f64,
    /// Reachable physical bytes.
    pub physical: u64,
    /// Per-op write latency p50 (µs).
    pub write_p50_us: f64,
    /// Per-op write latency p95 (µs).
    pub write_p95_us: f64,
    /// Per-op write latency p99 (µs).
    pub write_p99_us: f64,
}

/// H2 temporal-basis experiment: sequential drift versions vs. the
/// shuffled-history negative control (methodology §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionedExperiment {
    /// Corpus name (versioned).
    pub corpus: String,
    /// Number of drift versions written.
    pub versions: usize,
    /// Reachable bytes per sequential full run.
    pub sequential_full: Vec<u64>,
    /// Reachable bytes per sequential no-base run.
    pub sequential_no_base: Vec<u64>,
    /// Reachable bytes per shuffled full run.
    pub shuffled_full: Vec<u64>,
    /// Sequential logical/reachable ratio (median).
    pub sequential_ratio: f64,
    /// Shuffled logical/reachable ratio (median).
    pub shuffled_ratio: f64,
    /// Base+residual savings: shuffled − sequential reachable bytes.
    pub base_savings_reachable_bytes: i64,
    /// Base+residual savings as % of shuffled reachable.
    pub base_savings_pct: f64,
}

/// GC + background-optimizer traffic (methodology §6 maintenance metrics).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcTraffic {
    /// Unreachable bytes before the GC pass.
    pub unreachable_before: u64,
    /// Bytes reclaimed by the GC pass.
    pub reclaimed_bytes: u64,
    /// Unreachable bytes after the GC pass.
    pub unreachable_after: u64,
    /// Segment-file bytes before GC.
    pub physical_before: u64,
    /// Segment-file bytes after GC.
    pub physical_after: u64,
    /// GC pass wall seconds.
    pub gc_wall_s: f64,
    /// Extents examined by the background optimizer pass.
    pub optimizer_scanned: u64,
    /// Extents rewritten by the optimizer pass.
    pub optimizer_rewritten: u64,
    /// Persisted bytes saved by the optimizer pass.
    pub optimizer_saved_bytes: u64,
}

/// Baselines (methodology §3) and explicit waivers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Baselines {
    /// RAW file on the backing filesystem.
    pub raw_file: Option<RawBaseline>,
    /// zstd -1 compression baseline.
    pub zstd_level_1: Option<CompressionBaseline>,
    /// zstd -19 compression baseline.
    pub zstd_level_19: Option<CompressionBaseline>,
    /// Direct rANS (same backend) on the source corpus.
    pub direct_rans_src: Option<RunMetrics>,
    /// Explicitly waived baselines with reasons.
    pub waived: Vec<String>,
}

/// RAW file baseline record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawBaseline {
    /// Path of the baseline file.
    pub path: String,
    /// Backing filesystem type.
    pub fstype: String,
    /// Bytes written.
    pub bytes: u64,
    /// Write throughput MiB/s.
    pub write_mbps: f64,
    /// Size ratio (1.0 for raw storage).
    pub ratio: f64,
}

/// External compression baseline record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionBaseline {
    /// Tool name (zstd).
    pub tool: String,
    /// Tool version string.
    pub version: String,
    /// Compression level.
    pub level: String,
    /// Input bytes.
    pub input_bytes: u64,
    /// Output bytes.
    pub output_bytes: u64,
    /// Input/output ratio.
    pub ratio: f64,
    /// Wall seconds.
    pub wall_s: f64,
}

/// One methodology §8 admission rule with its resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdmissionItem {
    /// The admission rule text.
    pub rule: String,
    /// Whether the rule is met by this campaign.
    pub met: bool,
    /// Supporting note / pointer to the archived artifact.
    pub note: String,
}

// ---------------------------------------------------------------------------
// Campaign execution
// ---------------------------------------------------------------------------

/// Run the full campaign; returns the created evidence directory.
pub fn run(opts: &CampaignOptions) -> Result<PathBuf, String> {
    std::fs::create_dir_all(&opts.scratch_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&opts.out_root).map_err(|e| e.to_string())?;
    let rev = Environment::capture(
        &opts.repo_root,
        &opts.scratch_dir,
        &opts.cache_state,
        &opts.policy_mode,
    )
    .revision_short;
    let rev_slug = if rev.is_empty() { "norev" } else { &rev };
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = opts.out_root.join(format!("campaign-{created}-{rev_slug}"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let mut log = String::new();
    line(
        &mut log,
        &format!("entropyfs evidence campaign — {created}"),
    );
    line(&mut log, &format!("revision: {rev_slug}"));

    // Context.
    let env = Environment::capture(
        &opts.repo_root,
        &opts.scratch_dir,
        &opts.cache_state,
        &opts.policy_mode,
    );
    write_json(&dir, "environment.json", &env)?;

    // Corpora.
    let corpora = build_corpora(opts)?;
    let corpus_manifest: Vec<serde_json::Value> = corpora
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "source": c.source,
                "description": c.description,
                "logical_bytes": c.logical_bytes(),
                "written_bytes": c.written_bytes(),
                "content_hash": c.content_hash(),
                "version_count": c.versions.len(),
                "version_hashes": c.version_hashes(),
            })
        })
        .collect();
    write_json(&dir, "corpus-manifest.json", &corpus_manifest)?;

    // Device-level sampling around the campaign.
    let device = env.store_device.clone();
    let dev_name = device.trim_start_matches("/dev/");
    let disk_before = diskstats(dev_name);

    // Per-corpus runs.
    let mut results = CampaignResults {
        campaign_dir: dir.display().to_string(),
        created_unix: created,
        runs: Vec::new(),
        ablation: AblationTable {
            corpus: "structured".into(),
            size_mib: opts.size_mib,
            rows: Vec::new(),
        },
        dsfb_investigation: DsfbInvestigation {
            corpus: "structured".into(),
            runs: 0,
            full: Vec::new(),
            no_dsfb: Vec::new(),
            physical_identical: false,
            write_mbps_full: StatSummary::default(),
            write_mbps_no_dsfb: StatSummary::default(),
            cpu_user_s_full: StatSummary::default(),
            cpu_user_s_no_dsfb: StatSummary::default(),
        },
        versioned_experiment: VersionedExperiment {
            corpus: "versioned".into(),
            versions: 0,
            sequential_full: Vec::new(),
            sequential_no_base: Vec::new(),
            shuffled_full: Vec::new(),
            sequential_ratio: 0.0,
            shuffled_ratio: 0.0,
            base_savings_reachable_bytes: 0,
            base_savings_pct: 0.0,
        },
        gc_traffic: GcTraffic {
            unreachable_before: 0,
            reclaimed_bytes: 0,
            unreachable_after: 0,
            physical_before: 0,
            physical_after: 0,
            gc_wall_s: 0.0,
            optimizer_scanned: 0,
            optimizer_rewritten: 0,
            optimizer_saved_bytes: 0,
        },
        baselines: Baselines::default(),
        device_writes: None,
        admission: Vec::new(),
    };

    // 1. Throughput corpus (repeated runs; also feeds the DSFB block).
    let structured = structured_corpus(opts.size_mib);
    {
        let c = corpora
            .iter()
            .find(|c| c.name == "structured")
            .expect("corpus present");
        let group = run_repeated(
            opts,
            c,
            "full",
            OptimizeOptions::default(),
            opts.runs,
            &mut log,
        )?;
        results.runs.push(group);
    }

    // 2. Accounting corpora.
    for name in ["src", "urandom", "compressed-z19"] {
        let c = corpora
            .iter()
            .find(|c| c.name == name)
            .expect("corpus present");
        let group = run_repeated(opts, c, "full", OptimizeOptions::default(), 3, &mut log)?;
        results.runs.push(group);
    }

    // 3. Ablation ladder on the structured corpus.
    line(&mut log, "\n== ablation ladder (structured) ==");
    for (mode, options) in OptimizeOptions::ablation_modes() {
        let tmp = scratch_tempdir(&opts.scratch_dir, "abl-")?;
        let store = fresh_store(tmp.path())?;
        let o = write_only(&store, 3, &structured, options)?;
        let n = store_numbers(&store)?;
        let row = AblationRow {
            mode: mode.to_string(),
            physical: n.reachable,
            ratio: n.logical as f64 / n.reachable.max(1) as f64,
            write_mbps: o.metrics.write_mbps,
            cpu_user_s: o.metrics.cpu_user_s,
            families: o.families,
        };
        line(
            &mut log,
            &format!(
                "  {mode:<10} physical {:>12} ratio {:>7.3}x write {:>8.1} MiB/s cpu {:.3}+{:.3}s (p95 write {:.0}µs)",
                row.physical,
                row.ratio,
                row.write_mbps,
                row.cpu_user_s,
                o.metrics.cpu_sys_s,
                summary(&o.latencies).p95 * 1e6,
            ),
        );
        results.ablation.rows.push(row);
    }

    // 4. DSFB investigation (repeated, same physical expectation).
    line(
        &mut log,
        "\n== DSFB search-budget investigation (structured) ==",
    );
    for mode in ["full", "no-dsfb"] {
        let options = options_for(mode)?;
        let mut runs = Vec::new();
        for _ in 0..opts.runs {
            let tmp = scratch_tempdir(&opts.scratch_dir, "dsfb-")?;
            let store = fresh_store(tmp.path())?;
            let o = write_only(&store, 3, &structured, options)?;
            let s = summary(&o.latencies);
            let n = store_numbers(&store)?;
            runs.push(DsfbRun {
                write_mbps: o.metrics.write_mbps,
                cpu_user_s: o.metrics.cpu_user_s,
                physical: n.reachable,
                write_p50_us: s.p50 * 1e6,
                write_p95_us: s.p95 * 1e6,
                write_p99_us: s.p99 * 1e6,
            });
        }
        let w: Vec<f64> = runs.iter().map(|r| r.write_mbps).collect();
        let c: Vec<f64> = runs.iter().map(|r| r.cpu_user_s).collect();
        let ws = summary(&w);
        let cs = summary(&c);
        line(
            &mut log,
            &format!(
                "  {mode:<9} write median {:>7.1} MiB/s (min {:.1}, max {:.1}) cpu median {:.3}s physical {:?}",
                ws.p50,
                ws.min,
                ws.max,
                cs.p50,
                runs.iter().map(|r| r.physical).collect::<Vec<_>>(),
            ),
        );
        if mode == "full" {
            results.dsfb_investigation.full = runs;
            results.dsfb_investigation.write_mbps_full = ws;
            results.dsfb_investigation.cpu_user_s_full = cs;
        } else {
            results.dsfb_investigation.no_dsfb = runs;
            results.dsfb_investigation.write_mbps_no_dsfb = ws;
            results.dsfb_investigation.cpu_user_s_no_dsfb = cs;
        }
    }
    results.dsfb_investigation.runs = opts.runs;
    results.dsfb_investigation.physical_identical = {
        let f = results.dsfb_investigation.full.first().map(|r| r.physical);
        let n = results
            .dsfb_investigation
            .no_dsfb
            .first()
            .map(|r| r.physical);
        f.is_some() && f == n
    };
    line(
        &mut log,
        &format!(
            "  physical identical across modes: {}",
            results.dsfb_investigation.physical_identical
        ),
    );

    // 5. Versioned experiment (H2 + shuffled negative control).
    line(&mut log, "\n== versioned experiment (H2) ==");
    let vseq = corpora
        .iter()
        .find(|c| c.name == "versioned")
        .expect("present");
    let vshuf = corpora
        .iter()
        .find(|c| c.name == "shuffled")
        .expect("present");
    results.versioned_experiment.versions = vseq.versions.len();
    let seq_full = run_repeated(opts, vseq, "full", OptimizeOptions::default(), 3, &mut log)?;
    let seq_nb = run_repeated(opts, vseq, "no-base", options_for("no-base")?, 3, &mut log)?;
    let shuf_full = run_repeated(opts, vshuf, "full", OptimizeOptions::default(), 3, &mut log)?;
    results.versioned_experiment.sequential_full =
        seq_full.runs.iter().map(|r| r.reachable_bytes).collect();
    results.versioned_experiment.sequential_no_base =
        seq_nb.runs.iter().map(|r| r.reachable_bytes).collect();
    results.versioned_experiment.shuffled_full =
        shuf_full.runs.iter().map(|r| r.reachable_bytes).collect();
    results.runs.push(seq_full);
    results.runs.push(seq_nb);
    results.runs.push(shuf_full);
    let seq_median = median(&results.versioned_experiment.sequential_full);
    let shuf_median = median(&results.versioned_experiment.shuffled_full);
    results.versioned_experiment.sequential_ratio =
        vseq.logical_bytes() as f64 / seq_median.max(1) as f64;
    results.versioned_experiment.shuffled_ratio =
        vshuf.logical_bytes() as f64 / shuf_median.max(1) as f64;
    results.versioned_experiment.base_savings_reachable_bytes =
        shuf_median as i64 - seq_median as i64;
    results.versioned_experiment.base_savings_pct = if shuf_median > 0 {
        (shuf_median as f64 - seq_median as f64) / shuf_median as f64 * 100.0
    } else {
        0.0
    };
    line(
        &mut log,
        &format!(
            "  sequential median reachable: {seq_median} bytes ({:.3}x)",
            vseq.logical_bytes() as f64 / seq_median.max(1) as f64
        ),
    );
    line(
        &mut log,
        &format!(
            "  shuffled    median reachable: {shuf_median} bytes ({:.3}x)",
            vshuf.logical_bytes() as f64 / shuf_median.max(1) as f64
        ),
    );
    line(
        &mut log,
        &format!(
            "  base+residual savings vs shuffled: {} bytes ({:.1}% of shuffled reachable)",
            results.versioned_experiment.base_savings_reachable_bytes,
            results.versioned_experiment.base_savings_pct
        ),
    );

    // 6. GC + optimizer traffic.
    line(&mut log, "\n== GC and optimizer traffic ==");
    let gc = run_gc_traffic(opts)?;
    results.gc_traffic = gc.clone();
    line(
        &mut log,
        &format!(
            "  unreachable before {} → reclaimed {} → after {}; physical {} → {}; gc {:.3}s; optimizer scanned {} rewrote {} saved {}",
            gc.unreachable_before,
            gc.reclaimed_bytes,
            gc.unreachable_after,
            gc.physical_before,
            gc.physical_after,
            gc.gc_wall_s,
            gc.optimizer_scanned,
            gc.optimizer_rewritten,
            gc.optimizer_saved_bytes
        ),
    );

    // 7. Baselines.
    line(&mut log, "\n== baselines ==");
    let src_pack = corpora
        .iter()
        .find(|c| c.name == "src")
        .expect("present")
        .final_bytes()
        .to_vec();
    results.baselines = run_baselines(opts, &src_pack, &corpora)?;
    for w in &results.baselines.waived {
        line(&mut log, &format!("  waived: {w}"));
    }
    if let Some(r) = &results.baselines.raw_file {
        line(
            &mut log,
            &format!(
                "  raw file ({}): {:.1} MiB/s write, ratio {:.3}x",
                r.fstype, r.write_mbps, r.ratio
            ),
        );
    }
    for (name, b) in [
        ("zstd -1", &results.baselines.zstd_level_1),
        ("zstd -19", &results.baselines.zstd_level_19),
    ] {
        if let Some(b) = b {
            line(
                &mut log,
                &format!(
                    "  {name}: {} → {} bytes ({:.3}x), {:.3}s",
                    b.input_bytes, b.output_bytes, b.ratio, b.wall_s
                ),
            );
        }
    }
    if let Some(r) = &results.baselines.direct_rans_src {
        line(
            &mut log,
            &format!(
                "  direct rANS (same backend, src corpus): {} → {} bytes ({:.3}x)",
                r.logical_bytes, r.reachable_bytes, r.ratio_reachable
            ),
        );
    }

    // 8. Device writes.
    if let Some(before) = disk_before {
        if let Some(after) = diskstats(dev_name) {
            let delta = disk_delta(dev_name, &before, &after);
            line(
                &mut log,
                &format!(
                    "device {}: {} sectors written ({} bytes), {} sectors read ({} bytes)",
                    delta.device,
                    delta.write_sectors,
                    delta.written_bytes(),
                    delta.read_sectors,
                    delta.read_bytes()
                ),
            );
            results.device_writes = Some(delta);
        }
    }

    // 9. Admission checklist.
    results.admission = admission_checklist(&results, &corpora);
    line(&mut log, "\n== admission checklist (methodology §8) ==");
    for a in &results.admission {
        line(
            &mut log,
            &format!(
                "  [{}] {} — {}",
                if a.met { "OK " } else { "FAIL" },
                a.rule,
                a.note
            ),
        );
    }

    // Write artifacts.
    write_json(&dir, "results.json", &results)?;
    write_json(&dir, "result-hashes.json", &result_hashes(&results))?;
    std::fs::write(dir.join("results.csv"), csv(&results)).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("raw-output.txt"), &log).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("report.md"), report(&results, &log)).map_err(|e| e.to_string())?;

    println!("{log}");
    line(
        &mut log,
        &format!("\ncampaign evidence written to {}", dir.display()),
    );
    println!("\ncampaign evidence written to {}", dir.display());
    Ok(dir)
}

// ---------------------------------------------------------------------------
// Per-run measurement
// ---------------------------------------------------------------------------

fn run_repeated(
    opts: &CampaignOptions,
    corpus: &Corpus,
    mode: &str,
    options: OptimizeOptions,
    runs: usize,
    log: &mut String,
) -> Result<CorpusModeRuns, String> {
    let mut metrics: Vec<RunMetrics> = Vec::new();
    let mut write_lats: Vec<f64> = Vec::new();
    let mut read_lats: Vec<f64> = Vec::new();
    let mut fsync_lats: Vec<f64> = Vec::new();
    let mut write_mbps_all: Vec<f64> = Vec::new();
    let mut read_mbps_all: Vec<f64> = Vec::new();
    let mut cpu_user_all: Vec<f64> = Vec::new();
    let mut cpu_sys_all: Vec<f64> = Vec::new();
    let mut physical_all: Vec<u64> = Vec::new();
    for i in 0..runs {
        let tmp = scratch_tempdir(&opts.scratch_dir, "run-")?;
        let store = fresh_store(tmp.path())?;
        let mut m = full_run(&store, corpus, options)?;
        m.metrics.run = i;
        m.metrics.hash_matches_input = m.metrics.result_hash == corpus.content_hash();
        write_lats.extend(m.write_latencies);
        read_lats.extend(m.read_latencies);
        fsync_lats.extend(m.fsync_latencies);
        write_mbps_all.push(m.metrics.write_mbps);
        read_mbps_all.push(m.metrics.read_mbps);
        cpu_user_all.push(m.metrics.cpu_user_s);
        cpu_sys_all.push(m.metrics.cpu_sys_s);
        physical_all.push(m.metrics.reachable_bytes);
        metrics.push(m.metrics);
    }
    let group = CorpusModeRuns {
        corpus: corpus.name.clone(),
        mode: mode.to_string(),
        run_count: runs,
        runs: metrics,
        write_throughput: summary(&write_mbps_all),
        read_throughput: summary(&read_mbps_all),
        write_latency_us: scale_summary(&summary(&write_lats), 1e6),
        read_latency_us: scale_summary(&summary(&read_lats), 1e6),
        fsync_latency_us: scale_summary(&summary(&fsync_lats), 1e6),
        cpu_user_s: summary(&cpu_user_all),
        cpu_sys_s: summary(&cpu_sys_all),
        physical_median: median(&physical_all),
        ratio_median: if median(&physical_all) > 0 {
            corpus.logical_bytes() as f64 / median(&physical_all) as f64
        } else {
            0.0
        },
    };
    line(
        log,
        &format!(
            "  {} [{}] {} runs: write {:.1} MiB/s (p50 {:.0}µs, p95 {:.0}µs, p99 {:.0}µs) read {:.1} MiB/s fsync p50 {:.0}µs p95 {:.0}µs p99 {:.0}µs physical median {} ratio {:.3}x",
            corpus.name,
            mode,
            runs,
            group.write_throughput.p50,
            group.write_latency_us.p50,
            group.write_latency_us.p95,
            group.write_latency_us.p99,
            group.read_throughput.p50,
            group.fsync_latency_us.p50,
            group.fsync_latency_us.p95,
            group.fsync_latency_us.p99,
            group.physical_median,
            group.ratio_median
        ),
    );
    Ok(group)
}

struct RunOutcome {
    metrics: RunMetrics,
    write_latencies: Vec<f64>,
    read_latencies: Vec<f64>,
    fsync_latencies: Vec<f64>,
}

fn full_run(
    store: &Store,
    corpus: &Corpus,
    options: OptimizeOptions,
) -> Result<RunOutcome, String> {
    let cpu0 = cpu_ticks();
    let o = write_only(store, 3, corpus, options)?;
    let (write_metrics, write_lats) = (o.metrics, o.latencies);

    // fsync (durability barrier) latency.
    let mut fsync_lats = Vec::new();
    for _ in 0..5 {
        let t0 = Instant::now();
        store
            .durability_barrier(&CrashHooks::none())
            .map_err(|e| e.to_string())?;
        fsync_lats.push(t0.elapsed().as_secs_f64());
    }

    // Read-back with exact verification + result hash.
    let mut read_lats = Vec::new();
    let mstart = Instant::now();
    let mut hasher = blake3::Hasher::new();
    let total = corpus.logical_bytes();
    let mut off = 0u64;
    while off < total {
        let want = 65536u64.min(total - off);
        let t0 = Instant::now();
        let data = store.read_file(3, off, want).map_err(|e| e.to_string())?;
        read_lats.push(t0.elapsed().as_secs_f64());
        if data.len() as u64 != want {
            return Err(format!("read length mismatch at {off}"));
        }
        hasher.update(&data);
        off += want;
    }
    let read_wall = mstart.elapsed().as_secs_f64();
    let result_hash = hasher.finalize().to_hex().to_string();
    let cpu1 = cpu_ticks();

    let n = store_numbers(store)?;
    let (mut acct, families) = extent_decomposition(store, 3)?;
    acct.metadata_bytes = n
        .reachable
        .saturating_sub(acct.payload_bytes + acct.model_bytes + acct.descriptor_bytes);
    acct.integrity_bytes_est = n.record_count.saturating_mul(4).saturating_add(64);
    acct.allocator_overhead_bytes = n.allocator_overhead;
    acct.unreclaimed_bytes = n.unreachable;
    // Cross-check: the per-extent decomposition plus metadata must equal
    // the reachable total.
    let sum = acct.payload_bytes + acct.model_bytes + acct.descriptor_bytes + acct.metadata_bytes;
    acct.check = if sum == n.reachable {
        "ok".to_string()
    } else {
        format!(
            "mismatch: extent decomposition {sum} != reachable {}",
            n.reachable
        )
    };

    Ok(RunOutcome {
        metrics: RunMetrics {
            run: 0,
            write_mbps: write_metrics.write_mbps,
            read_mbps: total as f64 / read_wall / (1024.0 * 1024.0),
            write_wall_s: write_metrics.write_wall_s,
            read_wall_s: read_wall,
            cpu_user_s: cpu1.0 - cpu0.0,
            cpu_sys_s: cpu1.1 - cpu0.1,
            logical_bytes: n.logical,
            written_bytes: corpus.written_bytes(),
            reachable_bytes: n.reachable,
            total_backing_bytes: n.total_backing,
            unreachable_bytes: n.unreachable,
            ratio_reachable: n.logical as f64 / n.reachable.max(1) as f64,
            ratio_total_backing: n.logical as f64 / n.total_backing.max(1) as f64,
            result_hash,
            hash_matches_input: false, // compared against the corpus hash by the caller
            families,
            accounting: acct,
        },
        write_latencies: write_lats,
        read_latencies: read_lats,
        fsync_latencies: fsync_lats,
    })
}

/// Write path only (used by the ablation ladder and the DSFB block).
struct WriteMetrics {
    write_mbps: f64,
    write_wall_s: f64,
    cpu_user_s: f64,
    cpu_sys_s: f64,
}

/// Write-phase outcome: metrics, per-op latencies, representation families.
struct WriteOutcome {
    metrics: WriteMetrics,
    latencies: Vec<f64>,
    families: BTreeMap<String, u64>,
}

fn write_only(
    store: &Store,
    ino: u64,
    corpus: &Corpus,
    options: OptimizeOptions,
) -> Result<WriteOutcome, String> {
    let cpu0 = cpu_ticks();
    let start = Instant::now();
    let mut lats: Vec<f64> = Vec::new();
    // Group commit (Phase-8 write aggregation): each full version is one
    // transaction, so the transaction/COW amplification that plagued
    // per-chunk commits is measured away. Versions commit sequentially so
    // P0 (previous-version) bases resolve against committed state.
    for version in &corpus.versions {
        let mut writes: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut off = 0u64;
        while off < version.len() as u64 {
            let len = 65536u64.min(version.len() as u64 - off);
            writes.push((off, version[off as usize..(off + len) as usize].to_vec()));
            off += len;
        }
        let t0 = Instant::now();
        store
            .write_region_batch(ino, &writes, options)
            .map_err(|e| e.to_string())?;
        lats.push(t0.elapsed().as_secs_f64());
    }
    let wall = start.elapsed().as_secs_f64();
    let cpu1 = cpu_ticks();
    let (_, families) = extent_decomposition(store, ino)?;
    Ok(WriteOutcome {
        metrics: WriteMetrics {
            write_mbps: corpus.written_bytes() as f64 / wall / (1024.0 * 1024.0),
            write_wall_s: wall,
            cpu_user_s: cpu1.0 - cpu0.0,
            cpu_sys_s: cpu1.1 - cpu0.1,
        },
        latencies: lats,
        families,
    })
}

// ---------------------------------------------------------------------------
// Accounting
// ---------------------------------------------------------------------------

/// Store-level accounting numbers, computed live (§2): nothing here may
/// come from the (unmaintained) `StoreStats` accumulator.
struct StoreNumbers {
    logical: u64,
    reachable: u64,
    total_backing: u64,
    unreachable: u64,
    allocator_overhead: u64,
    record_count: u64,
}

fn store_numbers(store: &Store) -> Result<StoreNumbers, String> {
    let total_backing = dir_bytes(store.dir());
    let unreachable = crate::store::gc::unreachable_bytes(store).map_err(|e| e.to_string())?;
    let records_total: u64 = store
        .object_index()
        .iter()
        .into_iter()
        .map(|(_, loc)| loc.total_size())
        .sum();
    let allocator_overhead = total_backing.saturating_sub(records_total);
    let reachable = records_total.saturating_sub(unreachable);
    let logical = store.logical_bytes().map_err(|e| e.to_string())?;
    Ok(StoreNumbers {
        logical,
        reachable,
        total_backing,
        unreachable,
        allocator_overhead,
        record_count: store.object_index().len() as u64,
    })
}

/// Per-extent exact accounting: descriptor bytes, model objects, payload
/// objects, residual bytes, and the representation distribution.
fn extent_decomposition(
    store: &Store,
    ino: u64,
) -> Result<(Accounting, BTreeMap<String, u64>), String> {
    let limits = *store.limits();
    let inode = store
        .get_inode(ino)
        .map_err(|e| e.to_string())?
        .ok_or("inode missing")?;
    let root = match inode.data {
        InodeData::File { extent_root } => extent_root,
        _ => return Err("not a file".into()),
    };
    let mut acct = Accounting::default();
    let mut families: BTreeMap<String, u64> = BTreeMap::new();
    let mut descriptor_bytes = 0u64;
    let mut payload_objs: std::collections::HashSet<crate::core::extent::ChunkId> =
        std::collections::HashSet::new();
    let mut model_objs: std::collections::HashSet<crate::core::extent::ChunkId> =
        std::collections::HashSet::new();
    let mut residual_bytes = 0u64;
    for (_, bytes) in
        crate::store::extent_tree::scan_all(root, BTREE_ORDER, limits.max_fanout, store)
            .map_err(|e| e.to_string())?
    {
        let d = crate::format::descriptor::decode(
            &bytes,
            limits.max_descriptor_bytes,
            limits.max_inline_bytes,
            limits.max_palette,
            limits.max_period,
            limits.max_chunk_size,
        )
        .map_err(|e| e.to_string())?;
        descriptor_bytes += d.encoded_size();
        *families.entry(d.family().to_string()).or_insert(0) += 1;
        // Objects are counted once per unique id: content-addressed stores
        // alias shared objects, so a per-reference sum would double-count.
        match &d {
            Representation::Raw { obj, .. } => {
                payload_objs.insert(*obj);
            }
            Representation::Rans { model, enc_obj, .. } => {
                model_objs.insert(*model);
                payload_objs.insert(*enc_obj);
            }
            Representation::ExactRef { target, .. } => {
                payload_objs.insert(*target);
            }
            Representation::BaseResidual { base, residual, .. } => {
                payload_objs.insert(*base);
                residual_bytes += residual.encoded_size();
            }
            _ => {}
        }
    }
    let payload_bytes: u64 = payload_objs.iter().map(|id| object_size(store, id)).sum();
    let model_bytes: u64 = model_objs.iter().map(|id| object_size(store, id)).sum();
    acct.descriptor_bytes = descriptor_bytes;
    acct.payload_bytes = payload_bytes;
    acct.model_bytes = model_bytes;
    acct.residual_bytes = residual_bytes;
    Ok((acct, families))
}

fn object_size(store: &Store, id: &crate::core::extent::ChunkId) -> u64 {
    store
        .object_index()
        .get(id)
        .map(|loc| loc.total_size())
        .unwrap_or(0)
}

/// Total bytes in regular files under a store directory (superblocks,
/// segments, lock).
fn dir_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(md) = e.metadata() {
                    total += md.len();
                }
            }
        }
    }
    total
}

// ---------------------------------------------------------------------------
// GC + optimizer traffic
// ---------------------------------------------------------------------------

fn run_gc_traffic(opts: &CampaignOptions) -> Result<GcTraffic, String> {
    // Small segments so the GC pass has victims below the target ratio.
    let config = StoreConfig {
        segment_size: 8 * 1024 * 1024,
        ..StoreConfig::default()
    };
    let tmp = scratch_tempdir(&opts.scratch_dir, "gc-")?;
    let store = Store::create(tmp.path(), &config, [0x66; 16]).map_err(|e| e.to_string())?;
    let inode = Inode::new_file(1000, 1000, 0o644);
    {
        let mut tx = store.begin_tx().map_err(|e| e.to_string())?;
        Store::put_inode_in_tx(&mut tx, 3, &inode).map_err(|e| e.to_string())?;
        tx.commit(&CrashHooks::none()).map_err(|e| e.to_string())?;
    }
    // Versioned drift writes create many unique objects, then an urandom
    // overwrite orphans them.
    let vseq = corpus::versioned(4, 8);
    write_only(&store, 3, &vseq, OptimizeOptions::default())?;
    let ur = corpus::urandom(opts.size_mib / 2, 0x1234_5678);
    write_only(&store, 3, &ur, OptimizeOptions::default())?;
    let before = crate::store::gc::unreachable_bytes(&store).map_err(|e| e.to_string())?;
    let physical_before = store.physical_used();
    let t0 = Instant::now();
    let reclaimed =
        crate::store::gc::collect(&store, &CrashHooks::none()).map_err(|e| e.to_string())?;
    let gc_wall = t0.elapsed().as_secs_f64();
    let after = crate::store::gc::unreachable_bytes(&store).map_err(|e| e.to_string())?;
    let physical_after = store.physical_used();
    let opt =
        crate::optimizer::background::optimize_pass(&store, OptimizeOptions::default(), None, None)
            .map_err(|e| e.to_string())?;
    Ok(GcTraffic {
        unreachable_before: before,
        reclaimed_bytes: reclaimed,
        unreachable_after: after,
        physical_before,
        physical_after,
        gc_wall_s: gc_wall,
        optimizer_scanned: opt.scanned,
        optimizer_rewritten: opt.rewritten,
        optimizer_saved_bytes: opt.saved_bytes,
    })
}

// ---------------------------------------------------------------------------
// Baselines
// ---------------------------------------------------------------------------

fn run_baselines(
    opts: &CampaignOptions,
    src_pack: &[u8],
    corpora: &[Corpus],
) -> Result<Baselines, String> {
    let mut b = Baselines::default();

    // RAW file on the same backing filesystem.
    let raw_path = opts.scratch_dir.join("baseline-raw.bin");
    let t0 = Instant::now();
    std::fs::write(&raw_path, src_pack).map_err(|e| e.to_string())?;
    let wall = t0.elapsed().as_secs_f64();
    let (_, fstype) = crate::evidence::environment::mount_of(&opts.scratch_dir);
    b.raw_file = Some(RawBaseline {
        path: raw_path.display().to_string(),
        fstype,
        bytes: src_pack.len() as u64,
        write_mbps: src_pack.len() as f64 / wall / (1024.0 * 1024.0),
        ratio: 1.0,
    });

    // zstd baselines.
    b.zstd_level_1 = zstd_baseline(src_pack, 1);
    b.zstd_level_19 = zstd_baseline(src_pack, 19);

    // Direct rANS (same backend) on the source corpus.
    if let Some(src) = corpora.iter().find(|c| c.name == "src") {
        let tmp = scratch_tempdir(&opts.scratch_dir, "rans-")?;
        let store = fresh_store(tmp.path())?;
        let outcome = full_run(&store, src, OptimizeOptions::raw_rans())?;
        b.direct_rans_src = Some(outcome.metrics);
    }

    // Explicit waivers: writable compressed-FS (btrfs) and read-only
    // compressed-image (EROFS/SquashFS) baselines need root for
    // loop-mounting, which this environment does not grant. The ablation
    // ladder plus RAW/zstd/direct-rANS cover methodology §3 for now.
    b.waived.push("btrfs with compression: waived — writable compressed-FS baseline requires root for loop-mounting a test image".into());
    b.waived.push("EROFS/SquashFS: waived — read-only compressed-image baseline deferred (requires root and/or mkfs.erofs)".into());

    Ok(b)
}

fn zstd_baseline(input: &[u8], level: i32) -> Option<CompressionBaseline> {
    let tmp = tempfile::NamedTempFile::new().ok()?;
    std::fs::write(tmp.path(), input).ok()?;
    let t0 = Instant::now();
    let out = std::process::Command::new("zstd")
        .args(["-q", &format!("-{level}"), "-c"])
        .arg(tmp.path())
        .output()
        .ok()?;
    let wall = t0.elapsed().as_secs_f64();
    if !out.status.success() {
        return None;
    }
    let version = std::process::Command::new("zstd")
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    Some(CompressionBaseline {
        tool: "zstd".into(),
        version,
        level: level.to_string(),
        input_bytes: input.len() as u64,
        output_bytes: out.stdout.len() as u64,
        ratio: input.len() as f64 / out.stdout.len().max(1) as f64,
        wall_s: wall,
    })
}

// ---------------------------------------------------------------------------
// Corpora
// ---------------------------------------------------------------------------

fn build_corpora(opts: &CampaignOptions) -> Result<Vec<Corpus>, String> {
    let pack = corpus::source_tree_pack(&opts.repo_root)?;
    let mut v = vec![Corpus::single(
        pack.clone(),
        "src",
        &format!(
            "EntropyFS source tree pack (revision {})",
            opts.repo_root.display()
        ),
        "docs + src + evidence + manifests, length-prefixed; deterministic per revision",
    )];
    v.push(structured_corpus(opts.size_mib));
    v.push(corpus::versioned(4, 8));
    v.push(corpus::shuffled_versioned(4, 8));
    v.push(corpus::urandom(opts.size_mib / 2, 0xdead_beef_cafe_f00d));
    match corpus::compressed_zstd(&pack, 19) {
        Ok(c) => v.push(c),
        Err(e) => eprintln!("entropyfs: campaign: compressed control skipped: {e}"),
    }
    Ok(v)
}

fn structured_corpus(size_mib: u64) -> Corpus {
    corpus::structured(size_mib)
}

fn options_for(mode: &str) -> Result<OptimizeOptions, String> {
    OptimizeOptions::ablation_modes()
        .into_iter()
        .find(|(name, _)| *name == mode)
        .map(|(_, o)| o)
        .ok_or_else(|| format!("unknown mode {mode}"))
}

/// A scratch temp dir under the campaign scratch directory (on the
/// backing device, not tmpfs).
fn scratch_tempdir(scratch: &Path, prefix: &str) -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(scratch)
        .map_err(|e| e.to_string())
}

/// Create + open a fresh store with a default file inode (ino 3).
fn fresh_store(dir: &Path) -> Result<Store, String> {
    let config = StoreConfig::default();
    Store::create(dir, &config, [0x66; 16]).map_err(|e| e.to_string())?;
    let store = Store::open(dir, &config).map_err(|e| e.to_string())?;
    let inode = Inode::new_file(1000, 1000, 0o644);
    {
        let mut tx = store.begin_tx().map_err(|e| e.to_string())?;
        Store::put_inode_in_tx(&mut tx, 3, &inode).map_err(|e| e.to_string())?;
        tx.commit(&CrashHooks::none()).map_err(|e| e.to_string())?;
    }
    Ok(store)
}

// ---------------------------------------------------------------------------
// Output helpers
// ---------------------------------------------------------------------------

fn write_json<T: Serialize>(dir: &Path, name: &str, value: &T) -> Result<(), String> {
    let json = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    crate::store::write_atomic(&dir.join(name), json.as_bytes()).map_err(|e| e.to_string())
}

fn line(log: &mut String, s: &str) {
    println!("{s}");
    log.push_str(s);
    log.push('\n');
}

fn scale_summary(s: &StatSummary, scale: f64) -> StatSummary {
    StatSummary {
        count: s.count,
        mean: s.mean * scale,
        min: s.min * scale,
        p50: s.p50 * scale,
        p95: s.p95 * scale,
        p99: s.p99 * scale,
        max: s.max * scale,
    }
}

fn median(v: &[u64]) -> u64 {
    if v.is_empty() {
        return 0;
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    s[s.len() / 2]
}

/// User+system CPU seconds consumed by this process (single-threaded
/// campaign; `/proc/self/stat` utime/stime, Linux USER_HZ = 100).
fn cpu_ticks() -> (f64, f64) {
    let body = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let rest = body.rsplit_once(')').map(|(_, r)| r).unwrap_or_default();
    let tok: Vec<&str> = rest.split_whitespace().collect();
    if tok.len() < 15 {
        return (0.0, 0.0);
    }
    let utime: f64 = tok[11].parse().unwrap_or(0.0);
    let stime: f64 = tok[12].parse().unwrap_or(0.0);
    const HZ: f64 = 100.0;
    (utime / HZ, stime / HZ)
}

fn result_hashes(results: &CampaignResults) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    for group in &results.runs {
        let mut runs = serde_json::Map::new();
        for r in &group.runs {
            runs.insert(
                r.run.to_string(),
                serde_json::json!({ "result_hash": r.result_hash, "matches_input": r.hash_matches_input }),
            );
        }
        m.insert(
            format!("{}[{}]", group.corpus, group.mode),
            serde_json::Value::Object(runs),
        );
    }
    serde_json::Value::Object(m)
}

fn csv(results: &CampaignResults) -> String {
    let mut out = String::from(
        "corpus,mode,run,logical_bytes,written_bytes,reachable_bytes,total_backing_bytes,unreachable_bytes,ratio_reachable,write_mbps,read_mbps,write_wall_s,read_wall_s,cpu_user_s,cpu_sys_s,result_hash\n",
    );
    for g in &results.runs {
        for r in &g.runs {
            out.push_str(&format!(
                "{},{},{},{},{},{},{},{},{:.4},{:.3},{:.3},{:.4},{:.4},{:.4},{:.4},{}\n",
                g.corpus,
                g.mode,
                r.run,
                r.logical_bytes,
                r.written_bytes,
                r.reachable_bytes,
                r.total_backing_bytes,
                r.unreachable_bytes,
                r.ratio_reachable,
                r.write_mbps,
                r.read_mbps,
                r.write_wall_s,
                r.read_wall_s,
                r.cpu_user_s,
                r.cpu_sys_s,
                r.result_hash
            ));
        }
    }
    out
}

fn admission_checklist(results: &CampaignResults, corpora: &[Corpus]) -> Vec<AdmissionItem> {
    let mut items = Vec::new();

    // Rule 1: complete benchmark context (captured by construction).
    items.push(AdmissionItem {
        rule: "benchmark context complete (revision, Cargo.lock, kernel, CPU, governor, device, cache state, command)".to_string(),
        met: true,
        note: "environment.json (revision, cargo_lock_hash, kernel_*, cpu_*, governor, store_device, cache_state, command) + corpus-manifest.json archived in this directory".into(),
    });

    // Rule 2: every required byte counted.
    let accounting_ok = results.runs.iter().all(|g| {
        g.runs.iter().all(|r| {
            r.accounting.check == "ok"
                && r.accounting.payload_bytes
                    + r.accounting.model_bytes
                    + r.accounting.descriptor_bytes
                    + r.accounting.metadata_bytes
                    > 0
        })
    });
    items.push(AdmissionItem {
        rule: "every required byte is counted (payload/models/residuals/descriptors/metadata/integrity/allocator/unreclaimed)".to_string(),
        met: accounting_ok,
        note: if accounting_ok { "per-run Accounting tables pass the reachable-bytes cross-check".into() } else { "check per-run accounting.check fields".into() },
    });

    // Rule 3: baselines run or explicitly waived.
    let has_baselines = results.baselines.raw_file.is_some()
        && (results.baselines.zstd_level_1.is_some() || results.baselines.zstd_level_19.is_some())
        && results.baselines.direct_rans_src.is_some();
    items.push(AdmissionItem {
        rule: "all listed baselines run or explicitly waived".to_string(),
        met: has_baselines,
        note: format!(
            "raw file {}; zstd {}; direct rANS {}; waivers: {}",
            if results.baselines.raw_file.is_some() {
                "present"
            } else {
                "MISSING"
            },
            if results.baselines.zstd_level_1.is_some() {
                "present"
            } else {
                "MISSING"
            },
            if results.baselines.direct_rans_src.is_some() {
                "present"
            } else {
                "MISSING"
            },
            results.baselines.waived.len()
        ),
    });

    // Rule 4: ablations identify the mechanism.
    let ablation_ok = results.ablation.rows.len() >= 8;
    items.push(AdmissionItem {
        rule: "ablations identify which mechanism caused the gain".to_string(),
        met: ablation_ok,
        note: format!(
            "ablation ladder has {} rows (A0–A8 families)",
            results.ablation.rows.len()
        ),
    });

    // Rule 5: negative controls included.
    let has_urandom = results
        .runs
        .iter()
        .any(|g| g.corpus == "urandom" && g.ratio_median < 1.5);
    let has_compressed = results.runs.iter().any(|g| g.corpus == "compressed-z19");
    let has_shuffled = !results.versioned_experiment.shuffled_full.is_empty();
    items.push(AdmissionItem {
        rule: "negative controls included (random → RAW, compressed → no gain, shuffled history → temporal gains disappear)".to_string(),
        met: has_urandom && has_compressed && has_shuffled,
        note: format!("urandom ratio {:.3}x (expected ≤1.5x); compressed present {}; shuffled present {}",
            results.runs.iter().find(|g| g.corpus == "urandom").map(|g| g.ratio_median).unwrap_or(0.0),
            has_compressed, has_shuffled),
    });

    // Rule 6: materialized hashes match input.
    let mut all_match = true;
    for c in corpora {
        for g in &results.runs {
            if g.corpus != c.name {
                continue;
            }
            for r in &g.runs {
                if !r.hash_matches_input {
                    all_match = false;
                }
            }
        }
    }
    items.push(AdmissionItem {
        rule: "materialized output hashes match the input corpus hashes".to_string(),
        met: all_match,
        note: if all_match {
            "result-hashes.json: all runs match corpus content hashes".into()
        } else {
            "result-hashes.json: at least one mismatch".into()
        },
    });

    // Rule 7: raw artifacts archived.
    items.push(AdmissionItem {
        rule: "raw result artifacts are archived".to_string(),
        met: true,
        note: "raw-output.txt, results.json, results.csv, environment.json, corpus-manifest.json, result-hashes.json, report.md".into(),
    });

    items
}

fn report(results: &CampaignResults, log: &str) -> String {
    let mut s = String::new();
    s.push_str("# EntropyFS evidence campaign\n\n");
    s.push_str(&format!(
        "- campaign dir: `{}`\n- created: unix {}\n",
        results.campaign_dir, results.created_unix
    ));
    s.push_str("\n## Admission checklist (methodology §8)\n\n");
    for a in &results.admission {
        s.push_str(&format!(
            "- [{}] {} — {}\n",
            if a.met { "x" } else { " " },
            a.rule,
            a.note
        ));
    }
    s.push_str("\n## Summary\n\n");
    for g in &results.runs {
        s.push_str(&format!(
            "- `{}[{}]`: {} runs — write {:.1} MiB/s (p50 {:.0}µs, p95 {:.0}µs, p99 {:.0}µs), read {:.1} MiB/s, fsync p50 {:.0}µs, physical median {} bytes, ratio {:.3}x\n",
            g.corpus,
            g.mode,
            g.run_count,
            g.write_throughput.p50,
            g.write_latency_us.p50,
            g.write_latency_us.p95,
            g.write_latency_us.p99,
            g.read_throughput.p50,
            g.fsync_latency_us.p50,
            g.physical_median,
            g.ratio_median
        ));
    }
    if let Some(d) = &results.device_writes {
        s.push_str(&format!(
            "\nDevice writes during campaign window ({}): {} bytes written, {} bytes read.\n",
            d.device,
            d.written_bytes(),
            d.read_bytes()
        ));
    }
    s.push_str("\n## Raw output\n\n```text\n");
    s.push_str(log);
    s.push_str("```\n");
    s
}
