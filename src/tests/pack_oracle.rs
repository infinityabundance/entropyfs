//! Phase 12E.12: the physical small-object packing oracle (offline, no
//! format change).
//!
//! # PURPOSE
//!
//! The 12E.12 brief: on contemporary EntropyFS (Phases 10/11), decompose
//! the PHYSICAL cost of a realistic small-file tree, and implement
//! Physical Object Packs only if the oracle demonstrates a meaningful
//! real small-file win. The brief is explicit that the logical
//! representation algebra must NOT be contaminated with a physical
//! placement concern (`INLINE_PACKED` is not a representation), and that
//! no pack format is allowed unless the oracle proves the win.
//!
//! # BOUNDARY
//!
//! KNOWS: the store write/namespace API, the derived object index, the
//! physical report, and the GC mark. NEVER KNOWS: FUSE, the optimizer's
//! internals, or any policy. It changes NO production code — like 12A
//! and 12D, this phase is an oracle whose only output is the sealed
//! decomposition and the gate decision.
//!
//! # MODEL
//!
//! A realistic small-file tree (tiny source files, headers, configs,
//! package metadata, 1–16 KiB, 16–64 KiB — the brief's exact corpus
//! classes) is written through the normal store path and checkpointed
//! (durability barrier). The physical cost is then decomposed:
//!
//! - `live_by_tag` — live record bytes per `RecordTag` (Data = payload
//!   objects, Model = rANS model objects, Inode = inode records,
//!   BtreeNode = extent + directory + inode-index trees, Root, Xattr,
//!   MutationLog = recoverable writeback state);
//! - `record_envelope_bytes` — `live_record_count × HEADER_SIZE` (the
//!   58-byte per-record envelope; every record pays it);
//! - `payload_pure_bytes` — live stored payload bytes (total minus
//!   envelopes);
//! - `padding_bytes` / `format_bytes` — zero padding + torn tails and
//!   segment magics from the physical report;
//! - `dead_before` / `dead_after` — reclaimable bytes before and after
//!   `compact_full` (the write-path dirt term).
//!
//! The cross-check is exact: Σ live `Location::total_size` must equal
//! the physical report's `live_bytes`, and `unexplained_bytes` must be
//! zero on the healthy store.
//!
//! # THE GATE (normative, from the 12E.12 brief)
//!
//! "If physical object/envelope fragmentation is a major remaining term,
//! implement Physical Object Packs below the representation layer … No
//! pack format is allowed unless the oracle demonstrates a meaningful
//! real small-file win."
//!
//! Pack candidates are the DATA + MODEL objects (the objects descriptors
//! reference; packs hold their payloads with ONE envelope per pack). The
//! packable envelope share is therefore:
//!
//! ```text
//! envelope_share = (packable record count × HEADER_SIZE)
//!                  / (packable live total bytes)
//! ```
//!
//! and the structural term (trees + inodes + roots + xattr) is the
//! non-packable metadata cost. The decision logic below classifies the
//! measured tree:
//!
//! - packable envelope share ≥ 20% AND structural + envelope ≥ 30% of
//!   physical used → pack candidates are material → a format-bit
//!   investigation (12E.12 continuation) would be justified;
//! - otherwise → REJECT packs on this evidence (record the numbers).
//!
//! # HISTORY / EVIDENCE
//!
//! Phase 9A (`unreachable_bytes_by_record_tag`) established the per-tag
//! floor diagnosis; Phase 9H physical reconciliation made the
//! index-vs-physical drift surface exact; the 12E.6 metrics surfaced the
//! aggregates. This oracle is the first per-tag LIVE decomposition of a
//! realistic small-file tree.
//!
//! # RUN (the driver tools/court-pack-oracle.sh orchestrates this)
//!
//! ```text
//! cargo test --release --lib pack_oracle -- --nocapture
//! ```
//!
//! Prints `PACK_ORACLE <json>` with the decomposition and the decision.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use tempfile::TempDir;

use crate::format::record::HEADER_SIZE;
use crate::format::version::RecordTag;
use crate::store::gc::{compact_full, mark_live};
use crate::store::physical::physical_report;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};

/// One corpus file: a path relative to the store root and its bytes.
struct CorpusFile {
    path: &'static str,
    bytes: Vec<u8>,
}

/// Deterministic realistic small-file corpus (the brief's exact classes).
///
/// Every byte is derived from the file's own index so runs are
/// reproducible; the shapes are realistic enough that the SIZE
/// distribution — not the content — drives the physical decomposition.
fn corpus() -> Vec<CorpusFile> {
    let mut out = Vec::new();
    // A tiny deterministic LCG for pseudo-random-but-reproducible sizes.
    let mut rng: u64 = 0x1234_5678_9ABC_DEF0;
    let mut next = |lo: u64, hi: u64| {
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        lo + (rng >> 16) % (hi - lo + 1)
    };

    let mut push = |path: &'static str, bytes: Vec<u8>| {
        out.push(CorpusFile { path, bytes });
    };

    // Tiny source files (0.2–1 KiB) under src/.
    for i in 0..30 {
        let n = next(200, 1024);
        let mut b = format!(
            "// module {i}: deterministic tiny source\nuse crate::util::{{self, Buf}};\n\npub struct Item{i} {{\n    pub id: u64,\n    pub buf: Buf,\n}}\n\nimpl Item{i} {{\n    pub fn new(id: u64) -> Self {{\n        Self {{ id, buf: Buf::default() }}\n    }}\n\n    pub fn len(&self) -> usize {{\n        self.buf.len()\n    }}\n}}\n"
        )
        .into_bytes();
        while b.len() < n as usize {
            b.extend_from_slice(
                format!(
                    "    // padding line {i}: let value = self.id.wrapping_add({});\n",
                    b.len() % 1000
                )
                .as_bytes(),
            );
        }
        b.truncate(n as usize);
        push(Box::leak(format!("src/mod{i:02}.rs").into_boxed_str()), b);
    }

    // Headers (1–4 KiB) under include/.
    for i in 0..10 {
        let n = next(1024, 4096);
        let mut b = format!(
            "#ifndef EFS_H{i:02}_H\n#define EFS_H{i:02}_H\n\n#include <stdint.h>\n#include <stddef.h>\n\n#define EFS_VERSION_{i:02} {i}\n\ntypedef struct efs_{i:02} {{\n    uint64_t magic;\n    uint32_t flags;\n    size_t len;\n}} efs_{i:02}_t;\n\nint efs_{i:02}_init(efs_{i:02}_t *h);\nssize_t efs_{i:02}_read(const efs_{i:02}_t *h, void *buf, size_t n);\n\n#endif /* EFS_H{i:02}_H */\n"
        )
        .into_bytes();
        while b.len() < n as usize {
            b.extend_from_slice(format!("/* doc {i}: block {len} */\n", len = b.len()).as_bytes());
        }
        b.truncate(n as usize);
        push(
            Box::leak(format!("include/efs_{i:02}.h").into_boxed_str()),
            b,
        );
    }

    // Configuration files (0.5–8 KiB) under etc/.
    for i in 0..15 {
        let n = next(512, 8192);
        let mut b = format!(
            "# entropyfs sample config {i}\n[server]\nhost = \"127.0.0.{i}\"\nport = {}\nworkers = {}\n\n[storage]\npath = \"/var/lib/efs-{i}\"\nsegment_size = {}\n\n[logging]\nlevel = \"info\"\nrotate = true\n"
        ,
        next(1024, 65535),
        next(1, 32),
        next(16, 256) * 1024 * 1024
        ).into_bytes();
        while b.len() < n as usize {
            b.extend_from_slice(format!("option_{} = \"value_{}\"\n", b.len() % 100, i).as_bytes());
        }
        b.truncate(n as usize);
        push(
            Box::leak(format!("etc/server{i:02}.conf").into_boxed_str()),
            b,
        );
    }

    // Package metadata (0.5–4 KiB, JSON-like) under meta/.
    for i in 0..10 {
        let n = next(512, 4096);
        let mut b = format!(
            "{{\n  \"name\": \"pkg-{i}\",\n  \"version\": \"0.{i}.{i}\",\n  \"description\": \"deterministic metadata exhibit {i}\",\n  \"authors\": [\"efs-oracle\"],\n  \"dependencies\": {{\n    \"core\": \"1.0.0\"\n  }},\n  \"checksums\": {{\n    \"sha256\": \"{i}{i}{i}{i}{i}{i}{i}{i}\"\n  }}\n}}\n"
        )
        .into_bytes();
        while b.len() < n as usize {
            b.extend_from_slice(
                format!(
                    "\"extra_field_{}\": {},\n",
                    b.len() % 50,
                    next(0, 1_000_000)
                )
                .as_bytes(),
            );
        }
        b.truncate(n as usize);
        push(
            Box::leak(format!("meta/pkg-{i:02}.json").into_boxed_str()),
            b,
        );
    }

    // Mixed 1–16 KiB files under data/ (structured text + low-entropy).
    for i in 0..20 {
        let n = next(1024, 16384);
        let mut b = format!(
            "== data record {i} ==\nseq: {i}\ntimestamp: 1787799{i:04}\nblocks: {}\n",
            next(2, 64)
        )
        .into_bytes();
        while b.len() < n as usize {
            b.extend_from_slice(
                format!("block {len} payload {i} ...\n", len = b.len() % 2048).as_bytes(),
            );
        }
        b.truncate(n as usize);
        push(
            Box::leak(format!("data/record{i:02}.bin").into_boxed_str()),
            b,
        );
    }

    // Mixed 16–64 KiB files under big/.
    for i in 0..10 {
        let n = next(16384, 65536);
        let mut b = format!(
            "# big structured exhibit {i}\n# rows: {}\n",
            next(500, 2000)
        )
        .into_bytes();
        while b.len() < n as usize {
            b.extend_from_slice(
                format!(
                    "row {}: field_a={} field_b={} field_c={}\n",
                    b.len() % 100000,
                    next(0, 999),
                    next(0, 999),
                    i
                )
                .as_bytes(),
            );
        }
        b.truncate(n as usize);
        push(
            Box::leak(format!("big/exhibit{i:02}.dat").into_boxed_str()),
            b,
        );
    }

    out
}

/// Per-tag LIVE decomposition from the derived object index + GC mark.
/// Returns (by_tag_total, by_tag_stored, live_record_count,
/// live_packable_total, live_packable_stored, live_packable_count).
fn decompose(
    store: &Store,
) -> (
    BTreeMap<String, u64>,
    BTreeMap<String, u64>,
    u64,
    u64,
    u64,
    u64,
) {
    let live = mark_live(store).expect("mark");
    let mut by_total: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_stored: BTreeMap<String, u64> = BTreeMap::new();
    let mut records = 0u64;
    let (mut pack_total, mut pack_stored, mut pack_count) = (0u64, 0u64, 0u64);
    for (id, loc) in store.object_index().iter() {
        if !live.contains(&id) {
            continue;
        }
        let tag = format!("{:?}", loc.tag);
        let total = loc.total_size();
        let stored = loc.stored_len;
        *by_total.entry(tag.clone()).or_insert(0) += total;
        *by_stored.entry(tag).or_insert(0) += stored;
        records += 1;
        if matches!(loc.tag, RecordTag::Data | RecordTag::Model) {
            pack_total += total;
            pack_stored += stored;
            pack_count += 1;
        }
    }
    (
        by_total,
        by_stored,
        records,
        pack_total,
        pack_stored,
        pack_count,
    )
}

/// The gate decision (see module doc for the normative rule).
fn decide(
    physical_used_before: u64,
    physical_used_after: u64,
    logical: u64,
    live_total: u64,
    by_total: &BTreeMap<String, u64>,
    pack_total: u64,
    pack_count: u64,
) -> (String, String) {
    let envelope = pack_count * HEADER_SIZE;
    let envelope_share = if pack_total == 0 {
        0.0
    } else {
        envelope as f64 / pack_total as f64
    };
    let structural: u64 = by_total
        .iter()
        .filter(|(k, _)| {
            matches!(
                k.as_str(),
                "BtreeNode" | "Inode" | "Root" | "Xattr" | "MutationLog"
            )
        })
        .map(|(_, v)| *v)
        .sum();
    let phys = physical_used_after.max(1);
    let struct_share = structural as f64 / phys as f64;
    let overhead_before = if logical == 0 {
        0.0
    } else {
        physical_used_before as f64 / logical as f64
    };
    let overhead_after = if logical == 0 {
        0.0
    } else {
        physical_used_after as f64 / logical as f64
    };

    let pack_candidate =
        envelope_share >= 0.20 && (structural as f64 + envelope as f64) >= 0.30 * phys as f64;
    let (verdict, rationale) = if pack_candidate {
        (
            "PACK-CANDIDATE",
            format!(
                "packable envelope share {:.1}% >= 20% and structural+envelope {:.1}% of physical >= 30%",
                envelope_share * 100.0,
                (structural as f64 + envelope as f64) / phys as f64 * 100.0
            ),
        )
    } else {
        (
            "REJECT-PACKS",
            format!(
                "packable envelope share {:.1}% < 20% (or structural+envelope {:.1}% < 30% of physical); \
                 envelope fragmentation is not a major term on this realistic tree",
                envelope_share * 100.0,
                (structural as f64 + envelope as f64) / phys as f64 * 100.0
            ),
        )
    };

    let _ = (overhead_before, overhead_after, live_total);
    (verdict.to_string(), rationale)
}

#[test]
fn pack_oracle() {
    let tmp = TempDir::new().expect("tmp");
    let dir = tmp.path().join("store");
    let config = StoreConfig::default();
    let store = Store::create(&dir, &config, [0x66; 16]).expect("create");

    // --- build the realistic small-file tree ------------------------------
    let hooks = CrashHooks::none();
    let mut n_dirs = 0u64;
    let mut n_files = 0u64;
    let mut lens: Vec<u64> = Vec::new();
    let mut ino_by_path: BTreeMap<String, u64> = BTreeMap::new();
    ino_by_path.insert("/".to_string(), 1);
    let mut logical = 0u64;
    for cf in corpus() {
        let parts: Vec<&str> = cf.path.split('/').collect();
        // Ensure every parent directory exists: walk the path components,
        // creating missing dirs under the previous component's inode.
        let mut parent_ino = 1u64;
        let mut cur = String::from("/");
        for part in &parts[..parts.len() - 1] {
            cur = format!("{cur}{part}/");
            if let Some(&existing) = ino_by_path.get(&cur) {
                parent_ino = existing;
            } else {
                let din = store
                    .create_entry(
                        parent_ino,
                        part.as_bytes(),
                        NewEntry::dir(0o755, 1000, 1000),
                        &hooks,
                    )
                    .unwrap_or_else(|e| panic!("dir {cur}: {e}"));
                ino_by_path.insert(cur.clone(), din);
                parent_ino = din;
                n_dirs += 1;
            }
        }
        let fin = store
            .create_entry(
                parent_ino,
                parts.last().expect("name").as_bytes(),
                NewEntry::file(0o644, 1000, 1000),
                &hooks,
            )
            .unwrap_or_else(|e| panic!("file {}: {e}", cf.path));
        store.write_region(fin, 0, &cf.bytes).expect("write");
        n_files += 1;
        logical += cf.bytes.len() as u64;
        lens.push(cf.bytes.len() as u64);
    }

    // Settle the epoch so the physical census is the checkpointed state.
    store.durability_barrier(&hooks).expect("barrier");

    // --- BEFORE decomposition ----------------------------------------------
    let phys_before = physical_report(&store).expect("physical before");
    let used_before = store.physical_used();
    let (by_total_b, by_stored_b, records_b, pack_total_b, pack_stored_b, pack_count_b) =
        decompose(&store);
    let live_total_b: u64 = by_total_b.values().sum();

    // Cross-check: Σ live Location::total_size == physical live_bytes.
    assert_eq!(
        live_total_b, phys_before.live_bytes,
        "index-live must equal physical live (cross-check)"
    );
    assert_eq!(
        phys_before.unexplained(),
        0,
        "unexplained must be 0 on a healthy store"
    );

    // --- compaction (the write-path dirt term) ------------------------------
    compact_full(&store, &hooks).expect("compact");
    let used_after = store.physical_used();
    let phys_after = physical_report(&store).expect("physical after");
    let (by_total_a, _, _, _, _, _) = decompose(&store);
    let dead_before = phys_before
        .segments
        .iter()
        .map(|s| s.dead_indexed_bytes + s.index_hidden_bytes + s.unindexed_bytes)
        .sum::<u64>()
        + phys_before.torn_bytes
        + phys_before.zero_padding_bytes;
    let dead_after = phys_after
        .segments
        .iter()
        .map(|s| s.dead_indexed_bytes + s.index_hidden_bytes + s.unindexed_bytes)
        .sum::<u64>()
        + phys_after.torn_bytes
        + phys_after.zero_padding_bytes;

    let envelope_b = records_b * HEADER_SIZE;
    let (verdict, rationale) = decide(
        used_before,
        used_after,
        logical,
        live_total_b,
        &by_total_b,
        pack_total_b,
        pack_count_b,
    );

    // Per-file size distribution (for the tiny-cohort context).
    lens.sort_unstable();
    let pct = |q: f64| {
        let i = ((lens.len() - 1) as f64 * q).round() as usize;
        lens[i]
    };

    let result = serde_json::json!({
        "schema": "pack-oracle-v1",
        "corpus": {
            "files": n_files,
            "dirs": n_dirs,
            "logical_payload_bytes": logical,
            "file_size_bytes": {
                "min": lens.first().copied().unwrap_or(0),
                "p25": pct(0.25),
                "p50": pct(0.50),
                "p75": pct(0.75),
                "max": lens.last().copied().unwrap_or(0),
            },
        },
        "before": {
            "physical_used_bytes": used_before,
            "live_total_bytes": live_total_b,
            "live_by_tag_total": by_total_b,
            "live_by_tag_stored": by_stored_b,
            "live_record_count": records_b,
            "record_envelope_bytes": envelope_b,
            "packable_live_total_bytes": pack_total_b,
            "packable_live_stored_bytes": pack_stored_b,
            "packable_record_count": pack_count_b,
            "padding_bytes": phys_before.zero_padding_bytes + phys_before.torn_bytes,
            "format_bytes": phys_before.format_overhead_bytes,
            "dead_before_bytes": dead_before,
            "unexplained_bytes": phys_before.unexplained(),
        },
        "after": {
            "physical_used_bytes": used_after,
            "dead_after_bytes": dead_after,
            "unexplained_bytes": phys_after.unexplained(),
        },
        "ratios": {
            "physical_over_logical_before": used_before as f64 / logical as f64,
            "physical_over_logical_after": used_after as f64 / logical as f64,
            "reachable_over_logical": live_total_b as f64 / logical as f64,
            "packable_envelope_share": (pack_count_b * HEADER_SIZE) as f64 / pack_total_b as f64,
            "record_envelope_share_of_live": envelope_b as f64 / live_total_b as f64,
        },
        "decision": { "verdict": verdict, "rationale": rationale },
    });
    println!("PACK_ORACLE {}", result);
    eprintln!(
        "pack-oracle: {} files, {} dirs, logical {} B, physical {:.1} KiB -> {:.1} KiB after compact, \
         live {:.1} KiB, envelopes {} B ({} recs), packable {:.1} KiB in {} objects, dead {:.1} KiB -> {:.1} KiB",
        n_files,
        n_dirs,
        logical,
        used_before as f64 / 1024.0,
        used_after as f64 / 1024.0,
        live_total_b as f64 / 1024.0,
        envelope_b,
        records_b,
        pack_total_b as f64 / 1024.0,
        pack_count_b,
        dead_before as f64 / 1024.0,
        dead_after as f64 / 1024.0
    );
}
