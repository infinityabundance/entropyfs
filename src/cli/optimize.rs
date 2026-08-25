//! `entropyfs optimize <store>`: foreground re-encoding pass.
//!
//! v1 scope: re-encode every extent whose current representation is
//! RAW/FILL/INLINE through the cheap candidate pipeline and commit the
//! cheaper valid replacement (byte-exact validation is mandatory before
//! commit, §32). DSFB-guided background optimization lands in Phase 4.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use crate::core::candidate::{CandidateContext, Encoder, pick_cheapest};
use crate::core::extent::ChunkId;
use crate::core::materialize::materialize_to_vec;
use crate::store::transaction::CrashHooks;
use crate::store::{ExtentUpdate, Store, StoreConfig};

/// Options for optimize.
#[derive(Debug, Clone, clap::Args)]
pub struct OptimizeArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
}

/// Run optimize.
pub fn run(args: &OptimizeArgs) -> Result<(), String> {
    crate::fsck::ensure_unmounted(&args.store)?;
    let config = StoreConfig::default();
    let mut store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;
    let limits = *store.limits();
    let policy = *store.policy();

    let mut rewritten = 0u64;
    let mut saved = 0u64;
    let inos = store.all_inodes().map_err(|e| e.to_string())?;
    for ino in inos {
        let inode = match store.get_inode(ino).map_err(|e| e.to_string())? {
            Some(i) => i,
            None => continue,
        };
        let extent_root = match inode.data {
            crate::store::inode::InodeData::File { extent_root } => extent_root,
            _ => continue,
        };
        if extent_root.is_zero() {
            continue;
        }
        let entries = crate::store::extent_tree::scan_all(
            extent_root,
            crate::store::BTREE_ORDER,
            limits.max_fanout,
            &store,
        )
        .map_err(|e| e.to_string())?;
        for (start, desc_bytes) in entries {
            let desc = match crate::format::descriptor::decode(
                &desc_bytes,
                limits.max_descriptor_bytes,
                limits.max_inline_bytes,
                limits.max_palette,
                limits.max_period,
                limits.max_chunk_size,
            ) {
                Ok(d) => d,
                Err(_) => continue,
            };
            // Only attempt re-encoding of literal-ish families.
            if !matches!(
                desc,
                crate::core::representation::Representation::Raw { .. }
                    | crate::core::representation::Representation::Fill { .. }
                    | crate::core::representation::Representation::Inline { .. }
            ) {
                continue;
            }
            let bytes = match materialize_to_vec(&desc, &store, &limits) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let cid = ChunkId::of(&bytes);
            let ctx = CandidateContext {
                limits: &limits,
                policy: &policy,
                content_id: cid,
                bases: &[],
                dedup: None,
            };
            let mut cands = Vec::new();
            if let Some(z) = crate::core::candidate::zero_candidate(&bytes, cid, &limits) {
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
            if let Some(r) = crate::core::candidate::raw_candidate(&bytes, cid, &limits) {
                cands.push(r);
            }
            let best = match pick_cheapest(&cands, &policy) {
                Some(b) => b,
                None => continue,
            };
            let current_cost = desc.encoded_size()
                + if matches!(
                    desc,
                    crate::core::representation::Representation::Raw { .. }
                ) {
                    bytes.len() as u64
                } else {
                    0
                };
            if best.cost.persisted_bytes() >= current_cost {
                continue; // no win
            }
            // Validation: materialize the candidate and compare exact
            // bytes before commit (§32).
            if materialize_to_vec(&best.representation, &store, &limits)
                .map(|b| b == bytes)
                .unwrap_or(false)
            {
                let update = ExtentUpdate {
                    offset: start,
                    descriptor: best.representation.clone(),
                    content_id: cid,
                    objects: best.objects.clone(),
                };
                if let Ok(()) =
                    store.commit_file_extents(ino, vec![update], None, &CrashHooks::none())
                {
                    rewritten += 1;
                    saved += current_cost.saturating_sub(best.cost.persisted_bytes());
                }
            }
        }
    }
    println!("rewritten {rewritten} extents, saved ~{saved} persisted bytes");
    Ok(())
}
