//! Background optimization: deeper, DSFB-guided re-encoding of cold
//! extents while the filesystem is otherwise idle (§16, §44-H4).
//!
//! Correctness rules:
//!
//! - the optimizer never defines correctness; every proposed representation
//!   is validated byte-exact before commit (§32);
//! - a background rewrite commits only when the current extent still holds
//!   the descriptor we materialized from (generation/CAS check, §25) so it
//!   can never overwrite a newer foreground write;
//! - the pass runs on extents in a snapshot; commits replace one extent at
//!   a time.

#![forbid(unsafe_code)]

use std::sync::Arc;

use crate::core::extent::ChunkId;
use crate::core::materialize::materialize_to_vec;
use crate::optimizer::policy::OptimizeOptions;
use crate::optimizer::search::{GuidedContext, SearchMode, encode_guided};
use crate::store::transaction::CrashHooks;
use crate::store::{Store, StoreError};

/// Background-pass statistics (reported by the CLI and `status`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackgroundStats {
    /// Extents examined.
    pub scanned: u64,
    /// Extents rewritten to a cheaper representation.
    pub rewritten: u64,
    /// Persisted bytes saved (current − new, summed).
    pub saved_bytes: u64,
    /// Extents skipped: CAS failed (extent changed under us).
    pub stale_skips: u64,
    /// Extents skipped: no strictly cheaper representation.
    pub no_gain: u64,
    /// Extents skipped: decode/materialize/encode errors.
    pub errors: u64,
}

/// Account the current persisted bytes attributable to an extent
/// (descriptor + directly referenced object payloads, §2 accounting:
/// every persistent bit necessary to decode the extent).
pub fn current_persisted_bytes(
    store: &Store,
    desc: &crate::core::representation::Representation,
) -> u64 {
    let mut total = desc.encoded_size();
    let mut object_ids: Vec<&ChunkId> = Vec::new();
    match desc {
        crate::core::representation::Representation::Raw { obj, .. } => object_ids.push(obj),
        crate::core::representation::Representation::Rans { model, enc_obj, .. } => {
            object_ids.push(model);
            object_ids.push(enc_obj);
        }
        _ => {}
    }
    for id in object_ids {
        if let Some(loc) = store.object_index().get(id) {
            total = total.saturating_add(loc.stored_len);
        }
    }
    total
}

/// Resumable pass cursor: where the last bounded pass stopped. Best-effort
/// (inode numbers can change between passes; the pass is idempotent).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassCursor {
    /// Index into the inode list where the next pass resumes.
    pub ino_index: usize,
    /// Next extent offset to examine within the current inode.
    pub offset: u64,
}

/// Run a background optimization pass over file extents.
///
/// `max_extents` bounds the pass (None = all). `cursor` makes the pass
/// resumable for the daemon worker: it starts at the cursor position and
/// advances it; a completed pass resets it to default. Each extent is
/// materialized, re-searched with the full plan, and replaced only when a
/// strictly cheaper valid representation exists and the extent is
/// unchanged since we read it.
pub fn optimize_pass(
    store: &Store,
    options: OptimizeOptions,
    max_extents: Option<u64>,
    mut cursor: Option<&mut PassCursor>,
) -> Result<BackgroundStats, StoreError> {
    let limits = *store.limits();
    let mut stats = BackgroundStats::default();
    let inos = store.all_inodes()?;
    let start_idx = cursor.as_ref().map(|c| c.ino_index).unwrap_or(0);
    let mut resume_offset = cursor.as_ref().map(|c| c.offset).unwrap_or(0);
    let mut idx = 0usize;
    let mut truncated = false;
    'ino: for ino in &inos {
        let at = *ino;
        if idx < start_idx {
            idx += 1;
            continue;
        }
        let inode = match store.get_inode(at)? {
            Some(i) => i,
            None => {
                idx += 1;
                continue;
            }
        };
        let extent_root = match inode.data {
            crate::store::inode::InodeData::File { extent_root } => extent_root,
            _ => {
                idx += 1;
                continue;
            }
        };
        if extent_root.is_zero() {
            idx += 1;
            continue;
        }
        let entries = crate::store::extent_tree::scan_all(
            extent_root,
            crate::store::BTREE_ORDER,
            limits.max_fanout,
            store,
        )?;
        for (start, desc_bytes) in entries {
            if start < resume_offset {
                continue; // resume within the inode
            }
            if let Some(max) = max_extents {
                if stats.scanned >= max {
                    if let Some(c) = cursor.as_mut() {
                        c.ino_index = idx;
                        c.offset = start;
                    }
                    truncated = true;
                    break 'ino;
                }
            }
            stats.scanned += 1;
            resume_offset = 0; // next inode starts from its first extent
            let desc = match crate::format::descriptor::decode(
                &desc_bytes,
                limits.max_descriptor_bytes,
                limits.max_inline_bytes,
                limits.max_palette,
                limits.max_period,
                limits.max_chunk_size,
            ) {
                Ok(d) => d,
                Err(_) => {
                    stats.errors += 1;
                    continue;
                }
            };
            let bytes = match materialize_to_vec(&desc, store, &limits) {
                Ok(b) => b,
                Err(_) => {
                    stats.errors += 1;
                    continue;
                }
            };
            let cid = ChunkId::of(&bytes);
            // Rebasing: a deep reference chain may be worth flattening to
            // a depth-0 encoding even when the guided search prefers the
            // chain (λ_depth tradeoff; §11).
            let rebased =
                crate::optimizer::rebase::flatten_if_deep(store, start, &desc, &bytes, &cid)?;
            let ctx = GuidedContext {
                ino: at,
                offset: start,
                target: &bytes,
                prev_version: None,
                pending: None,
                mode: SearchMode::Background,
            };
            let searched = match encode_guided(store, &ctx, options) {
                Ok(o) => Some(o),
                Err(_) => {
                    stats.errors += 1;
                    None
                }
            };
            // Choose the cheaper of the guided outcome and the rebased
            // candidate (whichever is strictly cheaper than current). The
            // persisted-byte total is category-agnostic: descriptor +
            // new object payloads + attributable integrity.
            let current_bytes = current_persisted_bytes(store, &desc);
            let mut best: Option<crate::store::ExtentUpdate> = None;
            let mut best_bytes = u64::MAX;
            if let Some(outcome) = &searched {
                if outcome.update.descriptor != desc {
                    let b = update_persisted_bytes(&outcome.update);
                    if b < best_bytes {
                        best_bytes = b;
                        best = Some(outcome.update.clone());
                    }
                }
            }
            if let Some(u) = rebased {
                if u.descriptor != desc {
                    let b = update_persisted_bytes(&u);
                    if b < best_bytes {
                        best_bytes = b;
                        best = Some(u);
                    }
                }
            }
            let Some(update) = best else {
                stats.no_gain += 1;
                continue;
            };
            if best_bytes >= current_bytes {
                stats.no_gain += 1;
                continue;
            }
            // CAS: the extent must still hold the descriptor we read
            // (§25 — never overwrite a newer write). The per-inode lock
            // closes the check→commit window against foreground writers.
            let _lock = store.inode_lock(at);
            let current_desc = store.extent_descriptor(at, start)?;
            let stale = match current_desc {
                Some(cur) => cur != desc_bytes,
                None => true,
            };
            if stale {
                stats.stale_skips += 1;
                continue;
            }
            // Byte-exactness was validated inside the search (§32); keep
            // the logical content id as the final gate.
            if update.content_id != cid {
                stats.errors += 1;
                continue;
            }
            store.commit_file_extents(at, vec![update], None, &CrashHooks::none())?;
            stats.rewritten += 1;
            stats.saved_bytes = stats.saved_bytes.saturating_add(current_bytes - best_bytes);
        }
        idx += 1;
    }
    // A pass that was not truncated by `max_extents` completed: reset the
    // cursor so the next pass starts fresh.
    if !truncated {
        if let Some(c) = cursor {
            *c = PassCursor::default();
        }
    }
    Ok(stats)
}

/// Persisted bytes attributable to an extent update: descriptor + the
/// payloads of the new objects it requires + attributable integrity.
fn update_persisted_bytes(update: &crate::store::ExtentUpdate) -> u64 {
    let mut total = update.descriptor.encoded_size();
    for o in &update.objects {
        total = total.saturating_add(o.payload.len() as u64);
    }
    total.saturating_add(4) // attributable integrity
}

/// Extents per idle worker cycle (bounded slice; the cursor resumes).
const WORKER_CYCLE_EXTENTS: u64 = 64;
/// Seconds the store must be silent before a worker cycle runs.
const WORKER_IDLE_SECS: u64 = 3;

/// Spawn the background optimizer worker. It runs a bounded, resumable
/// pass only when the store has been idle (`ops` unchanged since the last
/// cycle) and exits when `stop` is set (tied to the filesystem instance's
/// drop, so the store's advisory lock is always released).
pub fn spawn_background_worker(
    store: Arc<Store>,
    ops: Arc<std::sync::atomic::AtomicU64>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    options: OptimizeOptions,
) -> std::thread::JoinHandle<()> {
    use std::sync::atomic::Ordering;
    std::thread::Builder::new()
        .name("entropyfs-optimizer".into())
        .spawn(move || {
            let mut cursor = PassCursor::default();
            let mut last_ops = ops.load(Ordering::Relaxed);
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_secs(WORKER_IDLE_SECS));
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let now = ops.load(Ordering::Relaxed);
                if now != last_ops {
                    // The filesystem is active; defer to the next cycle.
                    last_ops = now;
                    continue;
                }
                // Reads and writes are lock-free at the store level (reads
                // never block), so the worker can run without disturbing
                // requests; the idle gate keeps CPU on cold data.
                let _ = optimize_pass(
                    &store,
                    options,
                    Some(WORKER_CYCLE_EXTENTS),
                    Some(&mut cursor),
                );
                last_ops = ops.load(Ordering::Relaxed);
            }
        })
        .expect("spawn background worker")
}
