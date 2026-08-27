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
//!
//! PURPOSE
//!     Re-encode cold extents into strictly cheaper representations when
//!     the filesystem is otherwise idle: the full-search pass
//!     (`optimize_pass`), the cross-file shared-dictionary pass
//!     (`shared_dict_pass`), and the amortized entropy-model pass
//!     (`model_bundle_pass`), driven by an idle-gated daemon worker
//!     (`spawn_background_worker`). The foreground write path defers work
//!     here on purpose (Phase-10B): the background full search recovers
//!     everything the cheap foreground skips.
//!
//! BOUNDARY
//!     This module knows the COMMITTED store (inode list, extent trees,
//!     object/chunk indexes) and the candidate/encoder machinery. It
//!     never defines correctness — §32 byte-exactness lives in candidate
//!     validation — and it never reads or writes the active epoch's
//!     pending state (Phase-10D: the pass flushes the epoch first,
//!     because re-encoding a file the epoch is mid-write to would corrupt
//!     the overlay view). It only proposes; the store's transactional
//!     commit path decides what becomes durable.
//!
//! MODEL
//!     The pass is a reader of committed state that materializes an
//!     extent, searches for a strictly cheaper valid representation, and
//!     commits it only if the extent is unchanged since the read (the CAS
//!     gate, §25) and the candidate is byte-exact (§32). Commits replace
//!     one extent at a time; every pass is idempotent and resumable
//!     (`PassCursor`), so bounded cycles can cover the whole store without
//!     a global lock.
//!
//! PERSISTENT AUTHORITY
//!     Yes — a committed rewrite replaces the extent's on-disk descriptor
//!     and can add objects (encoded payloads, models, shared-dictionary
//!     references). Two consequences are load-bearing:
//!
//!     - a rewritten extent may reference an anchor chunk chosen from
//!       another file; that anchor is then pinned by GC through the
//!       reference closure, so a deleted anchor owner never breaks
//!       surviving members (see `shared_dict_pass`);
//!     - an extent's decode depth is resolved through the chunk index at
//!       materialize time, so a later rewrite of the same content can
//!       deepen an already-committed chain; every pass closes with the
//!       Phase-10E convergence sweep (`Store::rebase_overdepth_extents`).
//!
//! CORRECTNESS INVARIANTS
//!     - every committed replacement is byte-exact against the
//!       materialized incumbent (§32), re-checked through a resolver that
//!       can see the candidate's own staged objects;
//!     - a rewrite commits only when the extent's descriptor is unchanged
//!       since the read (the CAS token, §25) — never overwrite a newer
//!       foreground write;
//!     - only strictly cheaper representations commit (persisted-byte
//!       accounting: descriptor + attributable objects, see
//!       `current_persisted_bytes` / `update_persisted_bytes`);
//!     - the logical content id (`ChunkId::of(bytes)`) is the final gate
//!       before commit;
//!     - no committed extent may exceed `max_reference_depth`: the
//!       Phase-10E sweep enforces it after every pass;
//!     - passes are idempotent: re-running a completed pass commits
//!       nothing.
//!
//! CONCURRENCY
//!     Runs concurrently with foreground readers and writers. Reads at the
//!     store level are lock-free (reads traverse committed snapshots), so
//!     scanning never blocks requests; the worker additionally defers a
//!     cycle while the op counter advances (idle gate). Commits take the
//!     per-inode lock, which closes the CAS check→commit window against
//!     foreground writers for that inode. The three passes run
//!     sequentially per worker cycle; one worker per filesystem instance.
//!
//! DURABILITY
//!     Commits go through the ordinary transactional commit path
//!     (`commit_file_extents` with `CrashHooks::none()`), so a committed
//!     rewrite is as durable as any foreground write. Background savings
//!     are never acknowledged to a user: settling is not a request.
//!
//! RESOURCE BOUNDS
//!     All work is bounded by explicit counts: `max_extents` bounds the
//!     pass slice (the worker uses `WORKER_CYCLE_EXTENTS` = 64 extents per
//!     idle cycle); anchor candidates are capped at `MAX_ANCHOR_CANDIDATES`
//!     = 12 with the pool at `MAX_ANCHOR_POOL` = 4 per directory, each at
//!     least `MIN_ANCHOR_BYTES` = 512 bytes; the model bundle pool is
//!     `MAX_MODEL_POOL` = 4. Depth walks are capped by the store's depth
//!     limits (see `optimizer::rebase`).
//!
//! PERFORMANCE
//!     Shaped as an idle-only, bounded, resumable worker so optimization
//!     CPU lands on cold data and never competes with requests. Measured
//!     rationale (tree court, `evidence/performance/INDEX.md`): post-GC
//!     per-file density rises with the passes — shared dict 2.182× →
//!     2.328× (`campaign-1787679299-8d6e147`), + anchor pool + deep
//!     family 2.194× → 2.354× (`campaign-1787681660-9be6bd3`), + model
//!     bundles 2.813× → 2.881× (`campaign-1787685723-60ecaf2`) — and the
//!     Phase-10B court proves cheap-foreground + background-settle keeps
//!     settled density unchanged (1.994×, evidence `d38f73f`).
//!
//! FAILURE MODES
//!     Expected and counted per extent: decode/materialize/encode errors →
//!     `errors`, CAS miss → `stale_skips`, no strict gain → `no_gain`. A
//!     failing extent never blocks the pass. A corrupting flatten is never
//!     committed (`flatten_if_deep` returns `None` on byte mismatch). The
//!     one state that must never occur is a committed extent whose chain
//!     exceeds the decode cap — the Phase-10E sweep exists to prevent
//!     exactly that (unreadable files became possible before it).
//!
//! HISTORY / EVIDENCE
//!     Phase-9B (`current_persisted_bytes` and staged-object resolution
//!     defects surfaced by SequenceDict), 9C/9D (shared dictionary +
//!     anchor pool), 9E (deep family), 9G (amortized model bundles; oracle
//!     S1/S3/S4 falsified), 10B (foreground/background division of labor),
//!     10D (epoch flush before rewrite), 10E (convergence rebase).
//!     Evidence: the sealed campaigns in `evidence/performance/INDEX.md`.

#![forbid(unsafe_code)]

use std::sync::Arc;

use crate::core::extent::ChunkId;
use crate::core::materialize::materialize_to_vec;
use crate::optimizer::policy::OptimizeOptions;
use crate::optimizer::search::{GuidedContext, SearchMode, encode_guided};
use crate::store::transaction::CrashHooks;
use crate::store::{Store, StoreError};

/// Background-pass statistics (reported by the CLI and `status`).
///
/// Role: the per-call accounting of one background pass (or one worker
/// cycle's worth of passes). Counters are CUMULATIVE within the call, not
/// snapshots of store state: `scanned` counts extents examined,
/// `rewritten` counts committed replacements, and `saved_bytes` is the
/// summed (current − new) persisted bytes over the rewritten extents.
///
/// Per extent, exactly one exit is counted (`rewritten` / `stale_skips` /
/// `no_gain` / `errors`); the exceptions are the Phase-10E convergence
/// sweep, which adds rebased extents to `rewritten` without scanning
/// them, and the model pass, which charges a whole rejected cohort to
/// `no_gain` at once.
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
///
/// Units: `ino_index` is an INDEX into the inode list snapshot the next
/// pass takes (not an inode number), so a stale cursor merely resumes at
/// a nearby inode; `offset` is the extent start offset in bytes within
/// that inode's extent tree. A completed (untruncated) pass resets the
/// cursor to default.
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
    crate::perf::trace::span!(
        "optimizer.optimize_pass",
        op = "optimize_pass",
        max_extents = max_extents.unwrap_or(u64::MAX)
    );
    // ---------------------------------------------------------------------
    // Stage 1: Flush the active epoch into the committed store.
    //
    // Phase-10D: the optimizer rewrites COMMITTED state; the active
    // epoch's pending chunks are invisible to it (and re-encoding a file
    // the epoch is mid-write to would corrupt the overlay view). Flush
    // the epoch first.
    // ---------------------------------------------------------------------
    store.ensure_epoch_flushed(&crate::store::transaction::CrashHooks::none())?;
    // ---------------------------------------------------------------------
    // Stage 2: Position the cursor over the inode list snapshot.
    //
    // `inos` is the snapshot of committed inodes this pass walks; a
    // resumable pass starts at `(start_idx, resume_offset)` and advances
    // as it goes. `truncated` records whether `max_extents` stopped the
    // pass before the end.
    // ---------------------------------------------------------------------
    let mut stats = BackgroundStats::default();
    let inos = store.all_inodes()?;
    let start_idx = cursor.as_ref().map(|c| c.ino_index).unwrap_or(0);
    let mut resume_offset = cursor.as_ref().map(|c| c.offset).unwrap_or(0);
    let mut idx = 0usize;
    let mut truncated = false;
    // ---------------------------------------------------------------------
    // Stage 3: Walk inodes and extents, evaluating one extent at a time.
    //
    // Each extent is materialized and re-searched with the full plan;
    // `evaluate_commit_extent` commits only a strictly cheaper valid
    // replacement and only when the extent is unchanged since we read it.
    // When `max_extents` is hit, the cursor is parked at the current
    // (inode index, extent start) so the next pass resumes exactly here.
    // ---------------------------------------------------------------------
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
    // ---------------------------------------------------------------------
    // Stage 4: Reset the cursor when the pass completed.
    //
    // A pass that was not truncated by `max_extents` completed: reset the
    // cursor so the next pass starts fresh.
    // ---------------------------------------------------------------------
    if !truncated {
        if let Some(c) = cursor {
            *c = PassCursor::default();
        }
    }
    // ---------------------------------------------------------------------
    // Stage 5: Phase-10E convergence sweep.
    //
    // Phase-10E convergence: index-entry replacements during this pass may
    // have deepened previously-committed chains past the decode cap; rebase
    // any such extent to a depth-0 encoding so no file becomes unreadable.
    // ---------------------------------------------------------------------
    let rebased = store.rebase_overdepth_extents(&crate::store::transaction::CrashHooks::none())?;
    stats.rewritten = stats.rewritten.saturating_add(rebased);
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
    // ---------------------------------------------------------------------
    // Stage 1: Decode and materialize the incumbent extent.
    //
    // `desc_bytes` is the CAS token — the exact committed descriptor this
    // pass read and materialized from; `bytes` is the logical content any
    // replacement must reproduce byte-exactly (§32). A decode/materialize
    // failure is counted, not fatal: an undecodable extent is never a
    // rewrite target.
    // ---------------------------------------------------------------------
    let limits = *store.limits();
    let desc = match crate::format::descriptor::decode(&desc_bytes, &limits) {
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
    // ---------------------------------------------------------------------
    // Stage 2: Generate candidate replacements.
    //
    // 2a. The rebase candidate: flatten a deep reference chain to a
    //     depth-0 encoding (the λ_depth tradeoff; §11).
    // 2b. The guided search — once without a shared dictionary, or once
    //     per pool anchor (each full search also covers every
    //     anchor-independent family, so the winner is the best across all
    //     anchors).
    // ---------------------------------------------------------------------
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
            semantic: None,
            mode: SearchMode::Background,
        };
        match encode_guided(
            store,
            &ctx,
            options,
            crate::optimizer::foreground::ForegroundPolicy::full(),
        ) {
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
                semantic: None,
                mode: SearchMode::Background,
            };
            match encode_guided(
                store,
                &ctx,
                options,
                crate::optimizer::foreground::ForegroundPolicy::full(),
            ) {
                Ok(o) => searched.push((o.update, 0)),
                Err(_) => stats.errors += 1,
            }
        }
    }
    // ---------------------------------------------------------------------
    // Stage 3: Select the strictly-cheaper best candidate.
    //
    // The persisted-byte total is category-agnostic: descriptor + new
    // object payloads + attributable integrity. `update_persisted_bytes`
    // counts every payload in the update (whether or not the object
    // already exists) — conservative, never phantom savings. The winner
    // must beat the incumbent by strictly fewer bytes, so a rewrite never
    // trades bytes away for density.
    // ---------------------------------------------------------------------
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
    // ---------------------------------------------------------------------
    // Stage 4: CAS gate, content-id gate, and commit.
    //
    // WHY THE CAS TOKEN IS THE INCUMBENT DESCRIPTOR BYTES:
    //
    // The optimizer read a file state (decoded `desc`, materialized
    // `bytes`), then spent real CPU computing a denser representation.
    // Between that read and this commit, a foreground writer may have
    // replaced the extent with NEW bytes. Committing the stale candidate
    // would silently overwrite the newer write's bytes with the old
    // content, re-encoded — a data-loss class, not a density question.
    // The gate therefore commits only when the extent is UNCHANGED since
    // the read, and the read's only witness is the descriptor itself:
    // `desc_bytes` is the exact committed bytes we materialized from, so
    // the compare IS the CAS token. Without the gate a concurrent write's
    // bytes could be silently replaced by stale candidate bytes.
    //
    // `store.inode_lock(ino)` — the same per-inode mutation lock the
    // write path takes (`write_region_with_fg`, `write_region_batch`,
    // `truncate_file`) — then closes the check→commit window against
    // foreground writers, making the comparison and the replacement
    // atomic with respect to that inode's writers (§25 — never overwrite
    // a newer write).
    // ---------------------------------------------------------------------
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
///
/// Units: persisted bytes (physical). Every object payload in the update
/// is counted whether or not it already exists in the object index — a
/// conservative overestimate of the marginal cost, never an
/// underestimate — so the strictly-cheaper gate cannot commit on phantom
/// savings. (The 9G model pass is stricter: its model cost charges only
/// unique payloads absent from the committed CAS.)
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
/// WHY ANCHORS SURVIVE OWNER DELETION:
///
/// An anchor is a member first-chunk: an existing terminal chunk owned by
/// some file in the directory. Once surviving members reference it as
/// their SEQUENCE_SHARED_DICT dictionary, the anchor becomes reachable
/// through those extents' REFERENCE CLOSURE, and GC — which traces from
/// the roots through every referenced object — pins it for as long as any
/// referencing extent survives. A deleted owner's anchor therefore
/// remains a valid dictionary source for surviving members; no separate
/// anchor registration or pin list exists, and none is needed. Evidence:
/// the shared-dict era tree courts sealed the post-GC per-file gains
/// (9C `campaign-1787679299-8d6e147` 2.182× → 2.328×; 9D/9E
/// `campaign-1787681660-9be6bd3` 2.194× → 2.354×; 9G
/// `campaign-1787685723-60ecaf2` 2.813× → 2.881×), all measured AFTER
/// GC ran — surviving references kept their anchors alive.
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
    crate::perf::trace::span!(
        "optimizer.shared_dict_pass",
        op = "shared_dict_pass",
        max_extents = max_extents.unwrap_or(u64::MAX)
    );
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
        let desc = match crate::format::descriptor::decode(&first_bytes, &limits) {
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
    // Phase-10E convergence: rebase any extent whose chain the pass's
    // index-entry replacements pushed past the decode cap (see
    // `Store::rebase_overdepth_extents`).
    let rebased = store.rebase_overdepth_extents(&crate::store::transaction::CrashHooks::none())?;
    stats.rewritten = stats.rewritten.saturating_add(rebased);
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
        let Ok(desc) = crate::format::descriptor::decode(&desc_bytes, &limits) else {
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
                // Phase-9G: the amortized entropy-model pass (directory
                // cohort models; bounded by the same extent slice).
                let _ = model_bundle_pass(&store, options, Some(WORKER_CYCLE_EXTENTS));
                last_ops = ops.load(Ordering::Relaxed);
            }
        })
        .expect("spawn background worker")
}

// ---------------------------------------------------------------------------
// Phase-9G: the amortized entropy-model background pass.
// ---------------------------------------------------------------------------
//
// The 9F decomposition measured that per-extent multi-stream rANS model
// objects were a large fraction of the sequence families' footprint, and
// the model-sharing oracle (src/tests/model_oracle.rs) decided the design:
//
// - S1 (share one model across an extent's streams) is FALSIFIED: forcing
//   a model to cover several differently-distributed streams costs more
//   in encoded bytes than it saves in model bytes. No ModelBundle v2
//   intra-extent partition format.
// - S2 (one aggregate model per stream TYPE per directory cohort) is
//   VALIDATED, and it needs NO format change: the model object is already
//   content-addressed, a descriptor already references it by ChunkId, and
//   the store CAS-dedups identical objects — N extents referencing the
//   same model object persist it once.
// - S3/S4 (bundle pools) lose to the single aggregate on the measured
//   corpus: pool model bytes exceed the marginal enc gains.
//
// The pass therefore: (1) collects a directory cohort's sequence-family
// extents and decodes their raw streams; (2) trains one aggregate model
// per stream type on the cohort's summed histograms; (3) greedily selects
// a small candidate pool (the aggregate + distinct member bundles) by
// MODEL-COST-AWARE marginal savings; (4) re-encodes each member's streams
// against its best bundle (per-stream RAW fallback; the model is
// amortized — its bytes are counted once for the cohort, never per
// member); (5) rewrites members only when the cohort's total persisted
// bytes strictly fall, through the same CAS + byte-exact gate as every
// other background pass.
//
// Accounting invariant (no phantom savings): a member's incumbent "pinned"
// bytes are descriptor + enc object ONLY. Model objects are amortized and
// never claimed as removable by one member; the new amortized model
// objects are charged once per unique payload (marginal against the
// committed CAS). Old exclusive models become unreachable after their last
// referencer rewrites and are reclaimed by GC — the post-GC footprint
// measures that, conservatively beyond the pass's claimed savings.

/// Pool-size bound for the greedy model-bundle selection.
const MAX_MODEL_POOL: usize = 4;

/// One sequence-family extent member of a directory cohort.
struct ModelMember {
    ino: u64,
    start: u64,
    /// Incumbent descriptor bytes (the CAS token).
    desc_bytes: Vec<u8>,
    /// Incumbent descriptor (a sequence family).
    desc: crate::core::representation::Representation,
    /// Decoded raw streams in family order.
    streams: Vec<Vec<u8>>,
    /// Stream-type tags: 0 commands, 1 literals, 2 offsets, 3 dict
    /// sources, 4 deep lengths.
    types: Vec<u8>,
    /// Pinned persisted bytes: descriptor + enc object (model amortized).
    pinned: u64,
}

/// A candidate model bundle: one `RansModel` per stream type.
type ModelBundle = std::collections::BTreeMap<u8, crate::rans::model::RansModel>;

/// The re-encode trial of one member against one bundle.
struct BundleEncode {
    /// New descriptor.
    descriptor: crate::core::representation::Representation,
    /// Enc object payload.
    enc_payload: Vec<u8>,
    /// Model object payload.
    model_payload: Vec<u8>,
    /// New pinned bytes: descriptor + enc payload + integrity (the model
    /// is amortized; counted once per cohort in the group gate).
    pinned: u64,
}

/// The bundle's persisted model bytes (each type model once).
fn bundle_model_bytes(b: &ModelBundle) -> u64 {
    b.values()
        .map(|m| crate::rans::metadata::encode_model(m).len() as u64)
        .sum()
}

/// The bundle's dedup key (encoded model bytes in type order).
fn bundle_key(b: &ModelBundle) -> Vec<u8> {
    b.values()
        .flat_map(crate::rans::metadata::encode_model)
        .collect()
}

/// Collect one sequence-family member (decode descriptor + streams).
fn collect_model_member(
    store: &Store,
    ino: u64,
    start: u64,
    desc_bytes: Vec<u8>,
    limits: &crate::core::limits::Limits,
) -> Result<Option<ModelMember>, StoreError> {
    use crate::core::representation::Representation;
    let desc = match crate::format::descriptor::decode(&desc_bytes, limits) {
        Ok(d) => d,
        Err(_) => return Ok(None),
    };
    let (scale_bits, codec) = crate::rans::sequence::sequence_scale_codec();
    let (streams, types, enc_id): (Vec<Vec<u8>>, Vec<u8>, ChunkId) = match &desc {
        Representation::SequenceRans {
            model,
            enc_obj,
            scale_bits: sb,
            codec: c,
            seq_len,
            lit_len,
            off_len,
            cmds,
            lit_out,
            ..
        } => {
            if *sb != scale_bits || *c != codec {
                return Ok(None);
            }
            let v = match crate::rans::sequence::decode_streams_n(
                store,
                limits,
                crate::rans::sequence::StreamRefs {
                    model: *model,
                    enc_obj: *enc_obj,
                    scale_bits: *sb,
                    codec: *c,
                },
                &[*seq_len, *lit_len, *off_len],
                *cmds as u64,
                *lit_out as u64,
                None,
                2,
            ) {
                Ok(v) => v,
                Err(_) => return Ok(None),
            };
            (v, vec![0, 1, 2], *enc_obj)
        }
        Representation::SequenceDeep {
            model,
            enc_obj,
            scale_bits: sb,
            codec: c,
            seq_len,
            lit_len,
            off_len,
            len_len,
            cmds,
            lit_out,
            ..
        } => {
            if *sb != scale_bits || *c != codec {
                return Ok(None);
            }
            let d = match crate::rans::sequence::decode_deep_streams(
                store,
                limits,
                crate::rans::sequence::StreamRefs {
                    model: *model,
                    enc_obj: *enc_obj,
                    scale_bits: *sb,
                    codec: *c,
                },
                crate::rans::sequence::DeepLens {
                    seq_len: *seq_len,
                    lit_len: *lit_len,
                    off_len: *off_len,
                    len_len: *len_len,
                    cmds: *cmds,
                    lit_out: *lit_out,
                },
            ) {
                Ok(d) => d,
                Err(_) => return Ok(None),
            };
            (
                vec![d.commands, d.literals, d.offsets, d.lengths],
                vec![0, 1, 2, 4],
                *enc_obj,
            )
        }
        Representation::SequenceDict {
            dictionary: _,
            dictionary_len: _,
            model,
            enc_obj,
            scale_bits: sb,
            codec: c,
            seq_len,
            lit_len,
            off_len,
            src_len,
            cmds,
            lit_out,
            ..
        }
        | Representation::SequenceSharedDict {
            dictionary: _,
            dictionary_len: _,
            shared: _,
            shared_len: _,
            model,
            enc_obj,
            scale_bits: sb,
            codec: c,
            seq_len,
            lit_len,
            off_len,
            src_len,
            cmds,
            lit_out,
            ..
        } => {
            if *sb != scale_bits || *c != codec {
                return Ok(None);
            }
            let d = match crate::rans::sequence::decode_four_streams(
                store,
                limits,
                crate::rans::sequence::StreamRefs {
                    model: *model,
                    enc_obj: *enc_obj,
                    scale_bits: *sb,
                    codec: *c,
                },
                crate::rans::sequence::FourStreams {
                    seq_len: *seq_len,
                    lit_len: *lit_len,
                    off_len: *off_len,
                    src_len: *src_len,
                    cmds: *cmds,
                    lit_out: *lit_out,
                },
            ) {
                Ok(d) => d,
                Err(_) => return Ok(None),
            };
            (
                vec![d.commands, d.literals, d.offsets, d.sources],
                vec![0, 1, 2, 3],
                *enc_obj,
            )
        }
        _ => return Ok(None),
    };
    let pinned = desc.encoded_size().saturating_add(
        store
            .object_index()
            .get(&enc_id)
            .map(|l| l.stored_len)
            .unwrap_or(0),
    );
    Ok(Some(ModelMember {
        ino,
        start,
        desc_bytes,
        desc,
        streams,
        types,
        pinned,
    }))
}

/// Rebuild the member's descriptor in its own family with a new model/enc
/// pair and the re-encoded stream lengths (the parse is preserved: the
/// decoded streams are re-encoded byte-identically, so `cmds`/`lit_out`
/// and `len` are invariant).
fn rebuild_descriptor(
    incumbent: &crate::core::representation::Representation,
    streams: &[Vec<u8>],
    enc: &crate::rans::sequence::EncodedStreams,
    model_id: ChunkId,
    enc_id: ChunkId,
) -> Option<crate::core::representation::Representation> {
    use crate::core::representation::Representation;
    let (scale_bits, codec) = crate::rans::sequence::sequence_scale_codec();
    match incumbent {
        Representation::SequenceRans { len, .. } => Some(Representation::SequenceRans {
            model: model_id,
            enc_obj: enc_id,
            scale_bits,
            codec,
            seq_len: enc.lens[0],
            lit_len: enc.lens[1],
            off_len: enc.lens[2],
            cmds: streams[0].len() as u32,
            lit_out: streams[1].len() as u32,
            len: *len,
        }),
        Representation::SequenceDeep { len, .. } => Some(Representation::SequenceDeep {
            model: model_id,
            enc_obj: enc_id,
            scale_bits,
            codec,
            seq_len: enc.lens[0],
            lit_len: enc.lens[1],
            off_len: enc.lens[2],
            len_len: enc.lens[3],
            cmds: streams[0].len() as u32,
            lit_out: streams[1].len() as u32,
            len: *len,
        }),
        Representation::SequenceDict {
            dictionary,
            dictionary_len,
            len,
            ..
        } => Some(Representation::SequenceDict {
            dictionary: *dictionary,
            dictionary_len: *dictionary_len,
            model: model_id,
            enc_obj: enc_id,
            scale_bits,
            codec,
            seq_len: enc.lens[0],
            lit_len: enc.lens[1],
            off_len: enc.lens[2],
            src_len: enc.lens[3],
            cmds: streams[0].len() as u32,
            lit_out: streams[1].len() as u32,
            len: *len,
        }),
        Representation::SequenceSharedDict {
            dictionary,
            dictionary_len,
            shared,
            shared_len,
            len,
            ..
        } => Some(Representation::SequenceSharedDict {
            dictionary: *dictionary,
            dictionary_len: *dictionary_len,
            shared: *shared,
            shared_len: *shared_len,
            model: model_id,
            enc_obj: enc_id,
            scale_bits,
            codec,
            seq_len: enc.lens[0],
            lit_len: enc.lens[1],
            off_len: enc.lens[2],
            src_len: enc.lens[3],
            cmds: streams[0].len() as u32,
            lit_out: streams[1].len() as u32,
            len: *len,
        }),
        _ => None,
    }
}

/// Re-encode one member against one bundle (per-stream forced models with
/// RAW fallback; `None` on any encode failure or when the result is not
/// strictly cheaper than the member's pinned bytes).
fn encode_member_against(member: &ModelMember, bundle: &ModelBundle) -> Option<BundleEncode> {
    let mut forced: Vec<Option<&crate::rans::model::RansModel>> = vec![None; member.types.len()];
    for (si, &t) in member.types.iter().enumerate() {
        forced[si] = bundle.get(&t);
    }
    let enc = crate::rans::sequence::encode_streams_n_with_models(&member.streams, &forced)?;
    let enc_obj = crate::core::candidate::ObjectRecord::data(enc.enc_obj.clone());
    let model_obj = crate::core::candidate::ObjectRecord::model(enc.model_obj.clone());
    let descriptor = rebuild_descriptor(
        &member.desc,
        &member.streams,
        &enc,
        model_obj.id,
        enc_obj.id,
    )?;
    let pinned = descriptor
        .encoded_size()
        .saturating_add(enc_obj.payload.len() as u64)
        .saturating_add(4); // attributable integrity
    if pinned >= member.pinned {
        return None;
    }
    Some(BundleEncode {
        descriptor,
        enc_payload: enc_obj.payload,
        model_payload: model_obj.payload,
        pinned,
    })
}

/// Phase-9G: the amortized entropy-model background pass (see the module
/// comment above for the full design and the oracle evidence).
///
/// `max_extents` bounds the number of cohort members COLLECTED per call
/// (a partial cohort is a valid cohort: its aggregate is trained on what
/// the cycle saw and the group gate still requires strict savings; later
/// cycles re-evaluate the same members cheaply — they are idempotent).
pub fn model_bundle_pass(
    store: &Store,
    options: OptimizeOptions,
    max_extents: Option<u64>,
) -> Result<BackgroundStats, StoreError> {
    let mut stats = BackgroundStats::default();
    // The pass re-encodes sequence-family extents' statistical layer; with
    // no sequence family enabled the cohorts are empty.
    if !(options.allow_sequence_rans
        || options.allow_sequence_rans_deep
        || options.allow_sequence_dict
        || options.allow_shared_dict)
    {
        return Ok(stats);
    }
    let dir_of = build_dir_map(store)?;
    let limits = *store.limits();
    // 1. Collect sequence-family members per directory (bounded).
    let mut by_dir: std::collections::BTreeMap<u64, Vec<ModelMember>> =
        std::collections::BTreeMap::new();
    let mut collected = 0u64;
    'ino: for ino in store.all_inodes()? {
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
        let entries = crate::store::extent_tree::scan_all(
            extent_root,
            crate::store::BTREE_ORDER,
            limits.max_fanout,
            store,
        )?;
        for (start, desc_bytes) in entries {
            if let Some(max) = max_extents {
                if collected >= max {
                    break 'ino;
                }
            }
            let Some(dir) = dir_of.get(&ino).copied() else {
                continue;
            };
            let Some(member) = collect_model_member(store, ino, start, desc_bytes, &limits)? else {
                continue;
            };
            collected += 1;
            stats.scanned = stats.scanned.saturating_add(1);
            by_dir.entry(dir).or_default().push(member);
        }
    }
    // 2-5. Per directory: aggregate models, candidates, greedy
    //      model-cost-aware selection, group gate, rewrite.
    for members in by_dir.values() {
        if members.len() < 2 {
            continue;
        }
        // Aggregate histograms per stream type.
        let mut agg_hist: std::collections::BTreeMap<u8, [u32; 256]> = Default::default();
        for m in members {
            for (si, &t) in m.types.iter().enumerate() {
                let h = agg_hist.entry(t).or_insert([0u32; 256]);
                for &b in &m.streams[si] {
                    h[b as usize] = h[b as usize].saturating_add(1);
                }
            }
        }
        let mut agg_bundle: ModelBundle = Default::default();
        for (t, h) in &agg_hist {
            if let Some(model) = crate::rans::sequence::aggregate_model(h) {
                agg_bundle.insert(*t, model);
            }
        }
        if agg_bundle.is_empty() {
            continue;
        }
        // Candidate bundles: the aggregate + distinct member bundles.
        let mut cands: Vec<ModelBundle> = Vec::new();
        let mut seen: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
        seen.insert(bundle_key(&agg_bundle));
        cands.push(agg_bundle);
        for m in members {
            let mut b: ModelBundle = Default::default();
            for (si, &t) in m.types.iter().enumerate() {
                if b.contains_key(&t) {
                    continue;
                }
                let mut h = [0u32; 256];
                for &x in &m.streams[si] {
                    h[x as usize] += 1;
                }
                if let Some(model) = crate::rans::sequence::aggregate_model(&h) {
                    b.insert(t, model);
                }
            }
            if seen.insert(bundle_key(&b)) {
                cands.push(b);
            }
        }
        let bundle_costs: Vec<u64> = cands.iter().map(bundle_model_bytes).collect();
        // gains[i][m]: member savings under bundle i (0 when not cheaper).
        let mut gains: Vec<Vec<u64>> = Vec::with_capacity(cands.len());
        let mut encodes: Vec<Vec<Option<BundleEncode>>> = Vec::with_capacity(cands.len());
        for bundle in &cands {
            let mut g = Vec::with_capacity(members.len());
            let mut es = Vec::with_capacity(members.len());
            for m in members {
                let trial = encode_member_against(m, bundle);
                match &trial {
                    Some(e) => {
                        g.push(m.pinned - e.pinned);
                        es.push(trial);
                    }
                    _ => {
                        g.push(0);
                        es.push(None);
                    }
                }
            }
            gains.push(g);
            encodes.push(es);
        }
        // Greedy pool selection: each added bundle must pay for its own
        // persisted model bytes (marginal enc savings over the members
        // already covered, minus the bundle's model cost).
        let mut selected: Vec<usize> = Vec::new();
        let mut covered: Vec<u64> = vec![0; members.len()];
        for _ in 0..MAX_MODEL_POOL {
            let mut best_idx: Option<usize> = None;
            let mut best_marginal = 0u64;
            for i in 0..cands.len() {
                if selected.contains(&i) {
                    continue;
                }
                let mut marginal = 0u64;
                for m in 0..members.len() {
                    marginal = marginal.saturating_add(gains[i][m].saturating_sub(covered[m]));
                }
                if marginal > bundle_costs[i] && marginal - bundle_costs[i] > best_marginal {
                    best_marginal = marginal - bundle_costs[i];
                    best_idx = Some(i);
                }
            }
            let Some(idx) = best_idx else {
                break;
            };
            selected.push(idx);
            for m in 0..members.len() {
                covered[m] = covered[m].max(gains[idx][m]);
            }
        }
        if selected.is_empty() {
            continue;
        }
        // Group gate: unique new model payloads are charged once (marginal
        // against the committed CAS); the covered savings must pay them.
        let mut rewrite: Vec<(usize, usize)> = Vec::new(); // (member, bundle)
        let mut model_payloads: Vec<Vec<u8>> = Vec::new();
        for m in 0..members.len() {
            if covered[m] == 0 {
                continue;
            }
            let mut best_i = selected[0];
            let mut best_gain = gains[selected[0]][m];
            for &i in &selected[1..] {
                if gains[i][m] > best_gain {
                    best_gain = gains[i][m];
                    best_i = i;
                }
            }
            let e = encodes[best_i][m]
                .as_ref()
                .expect("gain > 0 implies an encode");
            let mp = e.model_payload.clone();
            if !model_payloads.contains(&mp) {
                model_payloads.push(mp);
            }
            rewrite.push((m, best_i));
        }
        if rewrite.is_empty() {
            continue;
        }
        let mut model_cost = 0u64;
        for p in &model_payloads {
            if !store.object_index().contains(&ChunkId::of(p)) {
                model_cost = model_cost.saturating_add(p.len() as u64);
            }
        }
        let group_savings: u64 = covered.iter().sum();
        if group_savings <= model_cost {
            stats.no_gain = stats.no_gain.saturating_add(members.len() as u64);
            continue;
        }
        let group_gain = group_savings - model_cost;
        // Rewrite every selected member through the CAS + byte-exact gate.
        for (m, i) in rewrite {
            let member = &members[m];
            let e = encodes[i][m]
                .as_ref()
                .expect("rewrite set implies an encode");
            let bytes = match materialize_to_vec(&member.desc, store, &limits) {
                Ok(b) => b,
                Err(_) => {
                    stats.errors += 1;
                    continue;
                }
            };
            let cid = ChunkId::of(&bytes);
            let objects = vec![
                crate::core::candidate::ObjectRecord::data(e.enc_payload.clone()),
                crate::core::candidate::ObjectRecord::model(e.model_payload.clone()),
            ];
            // §32: the new descriptor must materialize byte-exactly to the
            // incumbent bytes through a resolver that can see its own new
            // objects (pending-aware).
            let candidate = crate::core::candidate::Candidate {
                representation: e.descriptor.clone(),
                objects: objects.clone(),
                cost: Default::default(),
                content_id: cid,
            };
            let resolver = crate::optimizer::search::CandidateResolver::new(
                store,
                objects.iter().map(|o| (o.id, o.payload.clone())).collect(),
                None,
            );
            if crate::core::candidate::validate_candidate(&candidate, &bytes, &resolver, &limits)
                .is_err()
            {
                stats.errors += 1;
                continue;
            }
            // CAS: the extent must still hold the descriptor we read.
            let _lock = store.inode_lock(member.ino);
            let current = store.extent_descriptor(member.ino, member.start)?;
            if current.as_deref() != Some(member.desc_bytes.as_slice()) {
                stats.stale_skips += 1;
                continue;
            }
            store.commit_file_extents(
                member.ino,
                vec![crate::store::ExtentUpdate {
                    offset: member.start,
                    descriptor: e.descriptor.clone(),
                    content_id: cid,
                    objects,
                }],
                None,
                &CrashHooks::none(),
            )?;
            stats.rewritten = stats.rewritten.saturating_add(1);
        }
        stats.saved_bytes = stats.saved_bytes.saturating_add(group_gain);
    }
    // Phase-10E convergence: rebase any extent whose chain the pass's
    // index-entry replacements pushed past the decode cap (see
    // `Store::rebase_overdepth_extents`).
    let rebased = store.rebase_overdepth_extents(&CrashHooks::none())?;
    stats.rewritten = stats.rewritten.saturating_add(rebased);
    Ok(stats)
}
