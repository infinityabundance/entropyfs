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
/// (descriptor + the objects it references, §2 accounting: every
/// persistent bit necessary to decode the extent).
///
/// Phase-9B fix: this previously counted only RAW/RANS object ids, so
/// SEQUENCE_RANS / SPARSE_BLOCK64 / SEQUENCE_DICT model+enc objects and
/// residual streams were invisible to the incumbent cost — the optimizer
/// then refused every densification of an object-backed extent because
/// the incumbent looked nearly free.
pub fn current_persisted_bytes(
    store: &Store,
    desc: &crate::core::representation::Representation,
) -> u64 {
    let mut total = desc.encoded_size();
    for id in crate::store::transaction::descriptor_objects(desc, &store.config().limits) {
        if let Some(loc) = store.object_index().get(&id) {
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
        let limits = *store.limits();
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
            evaluate_commit_extent(store, at, start, desc_bytes, &[], options, &mut stats)?;
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

/// Evaluate one extent against the full search (plus an optional shared
/// dictionary POOL, Phase-9C/9D) and commit a strictly cheaper valid
/// replacement with the CAS gate. Shared by `optimize_pass` (no shared
/// dictionary) and `shared_dict_pass` (directory anchor pool), so the
/// commit path, validation, and bookkeeping can never diverge between the
/// two passes. Each pool anchor runs the full guided search (which
/// includes every anchor-independent family), so the winner is the best
/// across all anchors.
fn evaluate_commit_extent(
    store: &Store,
    ino: u64,
    start: u64,
    desc_bytes: Vec<u8>,
    shared_pool: &[crate::core::candidate::BaseChunk],
    options: OptimizeOptions,
    stats: &mut BackgroundStats,
) -> Result<(), StoreError> {
    let limits = *store.limits();
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
            return Ok(());
        }
    };
    let bytes = match materialize_to_vec(&desc, store, &limits) {
        Ok(b) => b,
        Err(_) => {
            stats.errors += 1;
            return Ok(());
        }
    };
    let cid = ChunkId::of(&bytes);
    // Rebasing: a deep reference chain may be worth flattening to a
    // depth-0 encoding even when the guided search prefers the chain
    // (λ_depth tradeoff; §11).
    let rebased = crate::optimizer::rebase::flatten_if_deep(store, start, &desc, &bytes, &cid)?;
    let mut searched: Vec<(crate::store::ExtentUpdate, u64)> = Vec::new();
    if shared_pool.is_empty() {
        let ctx = GuidedContext {
            ino,
            offset: start,
            target: &bytes,
            prev_version: None,
            // Phase-9B: the previous same-file chunk is the SequenceDict
            // dictionary (background: from the committed store).
            dictionary: if start >= limits.chunk_class {
                store.base_chunk_at(ino, start - limits.chunk_class, bytes.len())?
            } else {
                None
            },
            shared: None,
            pending: None,
            mode: SearchMode::Background,
        };
        match encode_guided(store, &ctx, options) {
            Ok(o) => searched.push((o.update, 0)),
            Err(_) => stats.errors += 1,
        }
    } else {
        for anchor in shared_pool {
            let ctx = GuidedContext {
                ino,
                offset: start,
                target: &bytes,
                prev_version: None,
                dictionary: if start >= limits.chunk_class {
                    store.base_chunk_at(ino, start - limits.chunk_class, bytes.len())?
                } else {
                    None
                },
                // Phase-9D: the pool anchor (each full search also covers
                // every anchor-independent family).
                shared: Some(anchor.clone()),
                pending: None,
                mode: SearchMode::Background,
            };
            match encode_guided(store, &ctx, options) {
                Ok(o) => searched.push((o.update, 0)),
                Err(_) => stats.errors += 1,
            }
        }
    }
    // Choose the cheapest guided outcome (any anchor) and the rebased
    // candidate, whichever is strictly cheaper than current. The
    // persisted-byte total is category-agnostic: descriptor + new object
    // payloads + attributable integrity.
    let current_bytes = current_persisted_bytes(store, &desc);
    let mut best: Option<crate::store::ExtentUpdate> = None;
    let mut best_bytes = u64::MAX;
    for (update, _) in &searched {
        if update.descriptor != desc {
            let b = update_persisted_bytes(update);
            if b < best_bytes {
                best_bytes = b;
                best = Some(update.clone());
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
        return Ok(());
    };
    if best_bytes >= current_bytes {
        stats.no_gain += 1;
        return Ok(());
    }
    // CAS: the extent must still hold the descriptor we read (§25 — never
    // overwrite a newer write). The per-inode lock closes the
    // check→commit window against foreground writers.
    let _lock = store.inode_lock(ino);
    let current_desc = store.extent_descriptor(ino, start)?;
    let stale = match current_desc {
        Some(cur) => cur != desc_bytes,
        None => true,
    };
    if stale {
        stats.stale_skips += 1;
        return Ok(());
    }
    // Byte-exactness was validated inside the search (§32); keep the
    // logical content id as the final gate.
    if update.content_id != cid {
        stats.errors += 1;
        return Ok(());
    }
    store.commit_file_extents(ino, vec![update], None, &CrashHooks::none())?;
    stats.rewritten += 1;
    stats.saved_bytes = stats.saved_bytes.saturating_add(current_bytes - best_bytes);
    Ok(())
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

/// Phase-9C/9D: the shared amortized dictionary pass.
///
/// For each directory with ≥ 2 candidate chunks, select a *pool* of
/// anchors (Phase-9D): member first-chunks chosen greedily so that each
/// added anchor maximizes the group's marginal SAVINGS against the
/// members' incumbent persisted bytes (exact deterministic cost,
/// deterministic tie-break). Each anchor is an existing terminal chunk —
/// its persisted state is accounted where it is materialized, so the
/// group pays only reference + read cost, which the strict-cheaper commit
/// gate enforces. Then rewrite every member extent with the cheapest
/// pool anchor as the SEQUENCE_SHARED_DICT dictionary when strictly
/// cheaper (same commit path as `optimize_pass`, incl. the CAS gate and
/// byte-exact validation).
///
/// v1 anchors are terminal descriptors (reference depth 0), so every
/// rewritten extent has depth ≤ 1 and shared-dictionary chains cannot
/// form. `max_extents` bounds the rewrite slice (anchor selection is
/// per-directory and bounded separately).
pub fn shared_dict_pass(
    store: &Store,
    options: OptimizeOptions,
    max_extents: Option<u64>,
) -> Result<BackgroundStats, StoreError> {
    shared_dict_pass_pool(store, options, max_extents, MAX_ANCHOR_POOL)
}

/// `shared_dict_pass` with an explicit pool-size bound (tests compare the
/// single-anchor control against the default pool).
pub(crate) fn shared_dict_pass_pool(
    store: &Store,
    options: OptimizeOptions,
    max_extents: Option<u64>,
    max_pool: usize,
) -> Result<BackgroundStats, StoreError> {
    let mut stats = BackgroundStats::default();
    if !options.allow_shared_dict {
        return Ok(stats);
    }
    // 1. Inode → directory map (batched reverse scan, like `parent_of`).
    let dir_of = build_dir_map(store)?;
    // 2. Group member first-chunks by directory, carrying the incumbent
    //    persisted bytes so anchor selection optimizes the ACTUAL rewrite
    //    objective (strictly-cheaper vs the current representation), not a
    //    raw-bytes comparison.
    let mut by_dir: std::collections::BTreeMap<u64, Vec<MemberChunk>> =
        std::collections::BTreeMap::new();
    for ino in store.all_inodes()? {
        let Some(inode) = store.get_inode(ino)? else {
            continue;
        };
        let extent_root = match inode.data {
            crate::store::inode::InodeData::File { extent_root } => extent_root,
            _ => continue,
        };
        if extent_root.is_zero() {
            continue;
        }
        let limits = *store.limits();
        let Some(first_bytes) = store.extent_descriptor(ino, 0)? else {
            continue;
        };
        let desc = match crate::format::descriptor::decode(
            &first_bytes,
            limits.max_descriptor_bytes,
            limits.max_inline_bytes,
            limits.max_palette,
            limits.max_period,
            limits.max_chunk_size,
        ) {
            Ok(d) => d,
            Err(_) => continue,
        };
        if desc.len() > limits.chunk_class {
            continue;
        }
        let bytes = match materialize_to_vec(&desc, store, &limits) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let dir = dir_of.get(&ino).copied().unwrap_or(1);
        let incumbent = current_persisted_bytes(store, &desc);
        by_dir
            .entry(dir)
            .or_default()
            .push(MemberChunk { bytes, incumbent });
    }
    // 3. Anchor-pool selection per directory.
    let mut pools: std::collections::BTreeMap<u64, Vec<crate::core::candidate::BaseChunk>> =
        std::collections::BTreeMap::new();
    for (dir, members) in &by_dir {
        if members.len() < 2 {
            continue;
        }
        let pool = select_anchor_pool(store, members, max_pool)?;
        if !pool.is_empty() {
            pools.insert(*dir, pool);
        }
    }
    // 4. Rewrite loop (the same per-extent evaluation + CAS + commit path
    //    as `optimize_pass`, with the directory's anchor pool supplied).
    let inos = store.all_inodes()?;
    let mut scanned = 0u64;
    'ino: for ino in &inos {
        let at = *ino;
        let Some(inode) = store.get_inode(at)? else {
            continue;
        };
        let extent_root = match inode.data {
            crate::store::inode::InodeData::File { extent_root } => extent_root,
            _ => continue,
        };
        if extent_root.is_zero() {
            continue;
        }
        let Some(dir) = dir_of.get(&at).copied() else {
            continue;
        };
        let Some(pool) = pools.get(&dir) else {
            continue;
        };
        let limits = *store.limits();
        let entries = crate::store::extent_tree::scan_all(
            extent_root,
            crate::store::BTREE_ORDER,
            limits.max_fanout,
            store,
        )?;
        for (start, desc_bytes) in entries {
            if let Some(max) = max_extents {
                if scanned >= max {
                    break 'ino;
                }
            }
            scanned += 1;
            stats.scanned += 1;
            evaluate_commit_extent(store, at, start, desc_bytes, pool, options, &mut stats)?;
        }
    }
    Ok(stats)
}

/// Inode → parent-directory map over all directories (one batched reverse
/// scan; v1 has no parent pointers, see `Store::parent_of`).
fn build_dir_map(store: &Store) -> Result<std::collections::HashMap<u64, u64>, StoreError> {
    let fanout = store.config().limits.max_fanout;
    let mut map: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
    let root_dir = store.current_root().root_dir_ino;
    for dir_ino in store.all_inodes()? {
        let Some(inode) = store.get_inode(dir_ino)? else {
            continue;
        };
        let dir_root = match inode.data {
            crate::store::inode::InodeData::Directory { dir_root } => dir_root,
            _ => continue,
        };
        if dir_root.is_zero() {
            continue;
        }
        let entries =
            crate::store::index::scan_all(dir_root, crate::store::BTREE_ORDER, fanout, store)?;
        for (_, v) in entries {
            if let Ok(e) = crate::store::directory::DirEntry::decode(&v) {
                map.entry(e.ino).or_insert(dir_ino);
            }
        }
    }
    map.entry(root_dir).or_insert(root_dir);
    Ok(map)
}

/// Per-directory anchor selection: the member first-chunk that maximizes
/// the group's total SAVINGS against the members' incumbent persisted
/// bytes when used as the shared dictionary (exact deterministic cost;
/// ties broken by ChunkId bytes). The self-member of a candidate is
/// excluded from its score (a file can never use itself as its own
/// dictionary), so selection cannot over-count a self-match.
///
/// Candidates are bounded: distinct first-chunks, largest first, at most
/// `MAX_ANCHOR_CANDIDATES`, each at least `MIN_ANCHOR_BYTES`, and each
/// must be a terminal descriptor (reference depth 0) so rewritten extents
/// stay at depth ≤ 1. Returns an empty pool when no candidate saves
/// anything (the group must save more than reference + read cost).
struct MemberChunk {
    /// Materialized first-chunk bytes.
    bytes: Vec<u8>,
    /// Incumbent persisted bytes of the first extent.
    incumbent: u64,
}

/// Per-directory anchor-POOL selection (Phase-9D): instead of one anchor,
/// pick up to `MAX_ANCHOR_POOL` member first-chunks greedily so each added
/// anchor maximizes the group's MARGINAL savings (members already covered
/// by a selected anchor contribute only the improvement). A member picks
/// its best pool anchor during the rewrite, so heterogeneous directories
/// (mixed styles/content classes) get per-file dictionary choice.
///
/// Deterministic: savings are exact candidate costs; ties break by ChunkId
/// bytes. The self-member of each candidate is excluded from its score (a
/// file can never use itself as its own dictionary).
fn select_anchor_pool(
    store: &Store,
    members: &[MemberChunk],
    max_pool: usize,
) -> Result<Vec<crate::core::candidate::BaseChunk>, StoreError> {
    use crate::core::candidate::{CandidateContext, Encoder};
    let limits = *store.limits();
    let policy = *store.policy();
    // Distinct candidate first-chunks, largest first.
    let mut seen = std::collections::HashSet::new();
    let mut cands: Vec<&[u8]> = Vec::new();
    let mut sorted: Vec<&[u8]> = members.iter().map(|m| m.bytes.as_slice()).collect();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.len()));
    for b in sorted {
        if b.len() < MIN_ANCHOR_BYTES {
            continue;
        }
        let id = ChunkId::of(b);
        if seen.insert(id) {
            cands.push(b);
        }
        if cands.len() >= MAX_ANCHOR_CANDIDATES {
            break;
        }
    }
    // Candidate metadata: chunk id, terminal check, savings per member.
    struct Cand {
        cid: ChunkId,
        bytes: Vec<u8>,
        saved: Vec<u64>, // per-member savings (0 for the self-member)
    }
    let mut pool_cands: Vec<Cand> = Vec::new();
    for c in cands {
        // Terminal anchors only (v1: no shared-dict chains).
        let Some(desc_bytes) = store.chunk_descriptor(&ChunkId::of(c))? else {
            continue;
        };
        let Ok(desc) = crate::format::descriptor::decode(
            &desc_bytes,
            limits.max_descriptor_bytes,
            limits.max_inline_bytes,
            limits.max_palette,
            limits.max_period,
            limits.max_chunk_size,
        ) else {
            continue;
        };
        if crate::core::cost::reference_depth(&desc) != 0 {
            continue;
        }
        let cid = ChunkId::of(c);
        let mut saved = Vec::with_capacity(members.len());
        for m in members {
            if m.bytes == c {
                saved.push(0); // a file cannot reference itself
                continue;
            }
            let ctx = CandidateContext {
                limits: &limits,
                policy: &policy,
                content_id: ChunkId::of(&m.bytes),
                bases: &[],
                dedup: None,
            };
            let enc = crate::rans::sequence::SequenceSharedDictEncoder {
                dictionary: crate::core::extent::ChunkId::ZERO,
                dict_bytes: Vec::new(),
                dict_depth: 0,
                shared: cid,
                shared_bytes: c.to_vec(),
                shared_depth: 0,
            };
            let cost = enc
                .encode(&m.bytes, &ctx)
                .into_iter()
                .map(|cand| cand.cost.persisted_bytes())
                .min()
                .unwrap_or(m.incumbent);
            saved.push(m.incumbent.saturating_sub(cost));
        }
        pool_cands.push(Cand {
            cid,
            bytes: c.to_vec(),
            saved,
        });
    }
    // Greedy selection: repeatedly take the candidate with the largest
    // marginal savings over the already-covered members, up to the pool
    // size or until nothing more is gained.
    let mut pool: Vec<crate::core::candidate::BaseChunk> = Vec::new();
    let mut covered: Vec<u64> = vec![0; members.len()];
    let mut selected: Vec<bool> = vec![false; pool_cands.len()];
    for _ in 0..max_pool {
        let mut best_idx: Option<usize> = None;
        let mut best_gain: u64 = 0;
        let mut best_id: ChunkId = ChunkId::ZERO;
        for (i, cand) in pool_cands.iter().enumerate() {
            if selected[i] {
                continue;
            }
            let mut gain = 0u64;
            for (m, &s) in cand.saved.iter().enumerate() {
                gain = gain.saturating_add(s.saturating_sub(covered[m]));
            }
            let better = match best_idx {
                None => gain > 0,
                Some(_) => {
                    gain > best_gain
                        || (gain == best_gain && cand.cid.as_bytes() < best_id.as_bytes())
                }
            };
            if better && gain > 0 {
                best_idx = Some(i);
                best_gain = gain;
                best_id = cand.cid;
            }
        }
        let Some(idx) = best_idx else {
            break; // nothing more to gain
        };
        let cand = &pool_cands[idx];
        for (m, &s) in cand.saved.iter().enumerate() {
            covered[m] = covered[m].max(s);
        }
        selected[idx] = true;
        pool.push(crate::core::candidate::BaseChunk {
            id: cand.cid,
            bytes: cand.bytes.clone(),
            depth: 0,
        });
    }
    Ok(pool)
}

/// Anchor-candidate bound per directory (largest first).
const MAX_ANCHOR_CANDIDATES: usize = 12;
/// Pool size bound (greedy marginal selection).
const MAX_ANCHOR_POOL: usize = 4;
/// Anchors smaller than this cannot amortize the reference cost.
const MIN_ANCHOR_BYTES: usize = 512;

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
                // Phase-9C: the shared amortized dictionary pass (whole
                // directories per cycle; bounded by the same extent slice).
                let _ = shared_dict_pass(&store, options, Some(WORKER_CYCLE_EXTENTS));
                last_ops = ops.load(Ordering::Relaxed);
            }
        })
        .expect("spawn background worker")
}
