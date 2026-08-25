//! `entropyfs explain <store> <path>`: the full representation breakdown
//! of a file (§49) — per-family byte share, alternatives for a focused
//! extent, and honest total accounting.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::core::candidate::{CandidateContext, Encoder, pick_cheapest};
use crate::core::materialize::materialize_to_vec;
use crate::store::{Store, StoreConfig};

/// Options for explain.
#[derive(Debug, Clone, clap::Args)]
pub struct ExplainArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
    /// File path inside the store (absolute).
    #[arg(value_name = "PATH")]
    pub path: String,
}

/// Run explain.
pub fn run(args: &ExplainArgs) -> Result<(), String> {
    let config = StoreConfig::default();
    let store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;
    let ino = store
        .resolve_path(args.path.as_bytes())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no such path: {}", args.path))?;
    let inode = store
        .get_inode(ino)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("inode {ino} missing"))?;
    let extent_root = match inode.data {
        crate::store::inode::InodeData::File { extent_root } => extent_root,
        _ => return Err("not a regular file".into()),
    };
    let entries = crate::store::extent_tree::scan_all(
        extent_root,
        crate::store::BTREE_ORDER,
        store.limits().max_fanout,
        &store,
    )
    .map_err(|e| e.to_string())?;

    let mut by_family: BTreeMap<&'static str, u64> = BTreeMap::new(); // logical bytes
    let mut logical_total = 0u64;
    let mut descriptor_total = 0u64;
    for (_, bytes) in &entries {
        let desc = crate::format::descriptor::decode(
            bytes,
            store.limits().max_descriptor_bytes,
            store.limits().max_inline_bytes,
            store.limits().max_palette,
            store.limits().max_period,
            store.limits().max_chunk_size,
        )
        .map_err(|e| format!("descriptor decode: {e:?}"))?;
        *by_family
            .entry(crate::cli::inspect::family_name(&desc))
            .or_insert(0) += desc.len();
        logical_total += desc.len();
        descriptor_total += bytes.len() as u64;
    }

    println!("file: {}", args.path);
    println!(
        "logical: {} bytes ({} extents)",
        logical_total,
        entries.len()
    );
    println!("descriptor bytes (extent metadata): {}", descriptor_total);
    println!();
    println!("representation:");
    for (family, bytes) in &by_family {
        let pct = if logical_total > 0 {
            *bytes as f64 * 100.0 / logical_total as f64
        } else {
            0.0
        };
        println!("  {family:>12}  {bytes:>12} bytes  ({pct:.1}%)");
    }
    // Physical reachable estimate: extent descriptors + referenced objects.
    let live = crate::store::gc::mark_live(&store).unwrap_or_default();
    let mut reachable = 0u64;
    for (id, loc) in store.object_index().iter() {
        if live.contains(id) {
            reachable += loc.total_size();
        }
    }
    println!();
    println!("physical reachable: {reachable} bytes (segments + superblock)");
    if logical_total > 0 {
        println!(
            "effective ratio: {:.3}x (logical / physical reachable)",
            logical_total as f64 / reachable.max(1) as f64
        );
    }
    println!();
    println!("note: effective ratio is workload-dependent and never a guarantee of");
    println!("capacity (docs/theory/information-accounting.md).");
    Ok(())
}

/// Re-encode a materialized extent and print the alternative candidates
/// with their exact costs (used by `inspect --offset`).
pub fn print_alternatives(store: &Store, desc: &crate::core::representation::Representation) {
    let limits = store.limits();
    let policy = store.policy();
    let bytes = match materialize_to_vec(desc, store, limits) {
        Ok(b) => b,
        Err(e) => {
            println!("  (materialization failed: {e})");
            return;
        }
    };
    let cid = crate::core::extent::ChunkId::of(&bytes);
    let ctx = CandidateContext {
        limits,
        policy,
        content_id: cid,
        bases: &[],
        dedup: None,
    };
    let mut cands = Vec::new();
    if let Some(z) = crate::core::candidate::zero_candidate(&bytes, cid, limits) {
        cands.push(z);
    }
    if let Some(f) = crate::core::candidate::fill_candidate(&bytes, cid) {
        cands.push(f);
    }
    for enc in [
        Box::new(crate::entropy::sparse::SparseEncoder) as Box<dyn Encoder>,
        Box::new(crate::entropy::palette::PaletteEncoder),
        Box::new(crate::entropy::periodic::PeriodicEncoder),
        Box::new(crate::rans::residual::RansEncoder),
    ] {
        cands.extend(enc.encode(&bytes, &ctx));
    }
    if let Some(r) = crate::core::candidate::raw_candidate(&bytes, cid, limits) {
        cands.push(r);
    }
    let selected = desc.encoded_size();
    let best = pick_cheapest(&cands, policy);
    println!("  alternative candidates:");
    for c in &cands {
        let mark = if best == Some(c) { "<- selected" } else { "" };
        println!(
            "    {:>12} {:>10} bytes (descriptor+objects: {})  {}",
            crate::cli::inspect::family_name(&c.representation),
            c.cost.persisted_bytes(),
            c.representation.encoded_size(),
            mark
        );
    }
    let _ = selected;
    let _ = policy;
}
