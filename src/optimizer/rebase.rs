//! Reference-chain flattening (§11: a base chain must not grow unbounded;
//! background optimization periodically flattens expensive chains).
//!
//! A deep chain (BaseResidual over BaseResidual over ...) trades decode
//! cost and λ_depth for space. Flattening materializes the final bytes and
//! re-encodes them at depth 0. The background pass calls this before the
//! guided search; the cheaper valid candidate wins.
//!
//! PURPOSE
//!     Measure and bound the reference depth of descriptors, and flatten
//!     chains that have grown past `REBASE_DEPTH_THRESHOLD` back to
//!     depth 0. Depth is the currency of bounded random access: every
//!     reference hop costs a chunk-index lookup at decode time, and the
//!     format caps total depth at `limits.max_reference_depth` (default
//!     4).
//!
//! BOUNDARY
//!     A pure read-side helper over the committed store's chunk index: it
//!     decodes descriptors and materializes one chunk, but never commits
//!     (the caller in `optimizer::background` owns the commit path and
//!     the CAS gate) and knows nothing about the epoch or the write path.
//!
//! MODEL
//!     The reference graph is a DAG, not a chain: SEQUENCE_SHARED_DICT
//!     points at two dictionaries (file + shared) that may converge on a
//!     common chunk, and EXACT_REF / BASE_RESIDUAL / SEQUENCE_DICT point
//!     at one. Depth is therefore the LONGEST-PATH length through the DAG
//!     (the deepest branch), never the visited-node depth.
//!
//! PERSISTENT AUTHORITY
//!     None directly — no writes happen here. But the depth reported here
//!     gates which descriptors the background passes commit: a chain
//!     deeper than the decode cap is undecodable (`DepthExceeded`), and
//!     depth is resolved through the chunk index at materialize time, so
//!     a chunk-index replacement can deepen an already-committed chain
//!     (Phase-10E).
//!
//! CORRECTNESS INVARIANTS
//!     - `chain_depth` reports the longest path through the DAG: a node
//!       first reached shallowly is re-explored when a deeper path
//!       reaches it, or the reported depth undercounts and the depth gate
//!       admits an undecodable chain;
//!     - `chain_depth_uncapped` must DETECT chains above the cap, so it
//!       is not capped by `max_reference_depth`; only a hard sanity bound
//!       (`MAX_CHAIN_WALK` = 64) guards a corrupt chain from looping;
//!     - `chain_contains` rejects a candidate base whose chain contains
//!       the target's own content id (self-reference is undecodable:
//!       materialization would loop until the depth cap);
//!     - `flatten_if_deep` never returns a corrupting candidate: the
//!       depth-0 re-encode is materialized back through a resolver that
//!       sees the candidate's own staged objects and compared byte-exact.
//!
//! CONCURRENCY
//!     Read-only; no locks. The chunk index may change between the walk
//!     and the caller's commit — the caller re-checks with the CAS gate.
//!
//! DURABILITY
//!     None: this module never persists anything.
//!
//! RESOURCE BOUNDS
//!     `chain_depth` walks are capped by `limits.max_reference_depth`
//!     (the decode cap) plus a per-node visited set; the uncapped walk is
//!     bounded by `MAX_CHAIN_WALK`. Each step decodes at most one
//!     descriptor and follows ≤ 2 children (SEQUENCE_SHARED_DICT), so a
//!     walk is O(cap · branching), trivially bounded. `flatten_if_deep`
//!     re-encodes one chunk (≤ `chunk_class` bytes).
//!
//! PERFORMANCE
//!     Depth is the decode-cost gate: flattening trades a few extra
//!     persisted bytes (the depth-0 re-encode) for bounded random access.
//!     `REBASE_DEPTH_THRESHOLD` = 2 flattens chains long before they
//!     approach the decode cap, and the caller's strictly-cheaper gate
//!     ensures flattening commits only when it also wins on bytes.
//!
//! FAILURE MODES
//!     Corrupt descriptors in the walk are skipped (an undecodable chunk
//!     contributes no children); missing index entries terminate a
//!     branch. The one state that must never occur is a committed chain
//!     past the decode cap — Phase-10E made unreadable files possible
//!     before the deepest-path walk and the post-pass convergence sweep
//!     fixed it.
//!
//! HISTORY / EVIDENCE
//!     Phase-9B: flatten must resolve through the candidate's OWN staged
//!     objects (materializing through the bare store failed on rANS/
//!     sequence model and stream objects). Phase-9C:
//!     SEQUENCE_SHARED_DICT branches the walk into two chains. Phase-10C:
//!     parallel batch encoding defers chain resolution to the serial
//!     assembly phase (`chain_depth_uncapped`). Phase-10E: deepest-path
//!     walks through diamond-shaped DAGs + post-pass convergence sweep
//!     (`Store::rebase_overdepth_extents`); pinned by
//!     `chain_depth_reports_deepest_path_through_a_diamond` and the
//!     hostile-media diamond court.

#![forbid(unsafe_code)]

use crate::core::extent::ChunkId;
use crate::core::representation::Representation;
use crate::store::{ExtentUpdate, Store, StoreError};

/// The chain depth at which flattening is worth attempting (format-policy
/// controlled; the decode-time cap is `limits.max_reference_depth`).
pub const REBASE_DEPTH_THRESHOLD: u8 = 2;

/// The reference depth of a descriptor (0 for terminal families; 1 for a
/// direct reference). For the full chain depth use [`chain_depth`].
pub const fn depth_of(desc: &Representation) -> u8 {
    match desc {
        Representation::ExactRef { .. }
        | Representation::BaseResidual { .. }
        | Representation::SequenceDict { .. } => 1,
        _ => 0,
    }
}

/// Resolve the full reference-chain depth of a descriptor by walking its
/// base/target through the chunk index. Bounded by the store's depth cap.
/// Phase-9C: SEQUENCE_SHARED_DICT references two dictionary chunks, so the
/// depth is the deeper of the two chains plus one.
///
/// The chain graph is a DAG (a chunk can be reachable through both the
/// dictionary and the shared branches of a SEQUENCE_SHARED_DICT), so the
/// walk records the DEEPEST depth at which each node was explored: a node
/// first reached via a shallow path must not block a deeper path through
/// it, or the reported depth undercounts and the depth gate would admit a
/// chain whose true length exceeds `max_reference_depth`.
///
/// WHY DEPTH IS LONGEST-PATH, NOT VISITED-NODE DEPTH (Phase-10E):
///
/// Phase-10E made the DAG case concrete on the real tree: when the dict
/// chain and the shared chain of a SEQUENCE_SHARED_DICT converge on a
/// common chunk, the reference graph is diamond-shaped, and a
/// first-reached-wins visited set reports the SHALLOWER convergence
/// depth. Meanwhile the chunk index resolves each reference at
/// materialize time, so a background pass's index-entry replacement can
/// push a previously-committed chain PAST the decode cap while the
/// undercounting walk keeps admitting it — unreadable files became
/// possible before the fix. The depth walks therefore follow the deepest
/// branch through the diamond, and the background passes close with a
/// post-pass convergence sweep (`Store::rebase_overdepth_extents`) that
/// rebases any extent whose chain a chunk-index replacement pushed past
/// the cap. Pinned by `chain_depth_reports_deepest_path_through_a_diamond`
/// (optimizer tests) and the hostile-media diamond court.
pub fn chain_depth(store: &Store, desc: &Representation) -> u8 {
    let limits = *store.limits();
    // Depth of one reference id from the chunk index (capped walk over
    // every branch; returns the deepest chain length).
    //
    // -----------------------------------------------------------------
    // Stage 1: Seed the worklist with the reference id at depth 0 and an
    // empty visited map (node -> deepest depth already explored from it).
    // -----------------------------------------------------------------
    fn walk(store: &Store, limits: &crate::core::limits::Limits, id: ChunkId) -> u8 {
        let mut max_depth = 0u8;
        let mut stack: Vec<(ChunkId, u8)> = vec![(id, 0u8)];
        // Node -> deepest depth already explored from it. Re-explore when
        // the current path reaches it deeper than before; skip otherwise.
        let mut visited: std::collections::HashMap<ChunkId, u8> = std::collections::HashMap::new();
        // -----------------------------------------------------------------
        // Stage 2: Pop a node; prune when at the decode cap or when the
        // node was already explored at ≥ this depth (a shallow first
        // visit must never block a deeper path through the node).
        // -----------------------------------------------------------------
        while let Some((cur, d)) = stack.pop() {
            if d >= limits.max_reference_depth {
                continue;
            }
            match visited.get(&cur) {
                Some(&vd) if vd >= d => continue,
                _ => {
                    visited.insert(cur, d);
                }
            }
            // -----------------------------------------------------------------
            // Stage 3: Decode the node's descriptor and push every
            // referenced chunk one level deeper; `max_depth` tracks the
            // deepest chain observed.
            // -----------------------------------------------------------------
            let Some(desc_bytes) = store.chunk_descriptor(&cur).ok().flatten() else {
                continue;
            };
            let Ok(next_desc) = crate::format::descriptor::decode(&desc_bytes, limits) else {
                continue;
            };
            let mut nexts: Vec<ChunkId> = Vec::new();
            match &next_desc {
                Representation::ExactRef { target, .. } => nexts.push(*target),
                Representation::BaseResidual { base, .. } => nexts.push(*base),
                Representation::SequenceDict { dictionary, .. } => nexts.push(*dictionary),
                Representation::SequenceSharedDict {
                    dictionary, shared, ..
                } => {
                    if !dictionary.is_zero() {
                        nexts.push(*dictionary);
                    }
                    nexts.push(*shared);
                }
                _ => {}
            }
            for n in nexts {
                stack.push((n, d.saturating_add(1)));
                max_depth = max_depth.max(d.saturating_add(1));
            }
        }
        max_depth
    }
    match desc {
        Representation::ExactRef { target, .. } => walk(store, &limits, *target).saturating_add(1),
        Representation::BaseResidual { base, .. } => walk(store, &limits, *base).saturating_add(1),
        Representation::SequenceDict { dictionary, .. } => {
            walk(store, &limits, *dictionary).saturating_add(1)
        }
        Representation::SequenceSharedDict {
            dictionary, shared, ..
        } => {
            let d = if dictionary.is_zero() {
                0
            } else {
                walk(store, &limits, *dictionary)
            };
            let s = walk(store, &limits, *shared);
            d.max(s).saturating_add(1)
        }
        _ => 0,
    }
}

/// The full reference-chain depth of a descriptor, resolving each
/// referenced id against the batch's in-batch depths first and the
/// committed chunk index otherwise (Phase-10C: parallel batch encoding
/// defers the real chain resolution to the serial assembly phase).
///
/// Unlike [`chain_depth`], the committed walk is NOT capped by
/// `max_reference_depth`: the caller must be able to DETECT a chain that
/// would exceed the decode cap so it can refuse or rebuild it. The
/// in-batch part of the chain is acyclic by construction (in-batch
/// dictionaries point strictly backward through the batch), the committed
/// part is acyclic by the §32 commit gate, and the walk is bounded by a
/// hard sanity cap; a chain deeper than any allowed cap reports a value
/// above it.
pub fn chain_depth_uncapped(
    store: &Store,
    desc: &Representation,
    pending_depths: &std::collections::HashMap<ChunkId, u8>,
) -> u8 {
    /// Hard sanity bound for the depth walk (far above the real decode cap
    /// of 4; only guards a corrupt descriptor chain from looping forever).
    const MAX_CHAIN_WALK: u8 = 64;

    /// Depth of one reference id from the committed chunk index (uncapped
    /// walk over every branch; returns the deepest chain length).
    ///
    /// The same staged walk as `chain_depth` (seed, longest-path prune,
    /// decode + descend), with one difference: Stage 2 prunes only at the
    /// hard `MAX_CHAIN_WALK` bound, never at `max_reference_depth` — the
    /// caller must be able to detect a chain that exceeds the decode cap.
    fn walk(store: &Store, id: ChunkId) -> u8 {
        let limits = *store.limits();
        let mut max_depth = 0u8;
        let mut stack: Vec<(ChunkId, u8)> = vec![(id, 0u8)];
        // Node -> deepest depth already explored from it (the chain graph
        // is a DAG; re-explore when a path reaches the node deeper than
        // before so the longest path is reported).
        let mut visited: std::collections::HashMap<ChunkId, u8> = std::collections::HashMap::new();
        while let Some((cur, d)) = stack.pop() {
            if d >= MAX_CHAIN_WALK {
                continue;
            }
            match visited.get(&cur) {
                Some(&vd) if vd >= d => continue,
                _ => {
                    visited.insert(cur, d);
                }
            }
            let Some(desc_bytes) = store.chunk_descriptor(&cur).ok().flatten() else {
                continue;
            };
            let Ok(next_desc) = crate::format::descriptor::decode(&desc_bytes, &limits) else {
                continue;
            };
            let mut nexts: Vec<ChunkId> = Vec::new();
            match &next_desc {
                Representation::ExactRef { target, .. } => nexts.push(*target),
                Representation::BaseResidual { base, .. } => nexts.push(*base),
                Representation::SequenceDict { dictionary, .. } => nexts.push(*dictionary),
                Representation::SequenceSharedDict {
                    dictionary, shared, ..
                } => {
                    if !dictionary.is_zero() {
                        nexts.push(*dictionary);
                    }
                    nexts.push(*shared);
                }
                _ => {}
            }
            for n in nexts {
                stack.push((n, d.saturating_add(1)));
                max_depth = max_depth.max(d.saturating_add(1));
            }
        }
        max_depth
    }

    fn resolve(store: &Store, pending: &std::collections::HashMap<ChunkId, u8>, id: ChunkId) -> u8 {
        match pending.get(&id) {
            Some(d) => *d,
            None => walk(store, id),
        }
    }

    match desc {
        Representation::ExactRef { target, .. } => {
            resolve(store, pending_depths, *target).saturating_add(1)
        }
        Representation::BaseResidual { base, .. } => {
            resolve(store, pending_depths, *base).saturating_add(1)
        }
        Representation::SequenceDict { dictionary, .. } => {
            resolve(store, pending_depths, *dictionary).saturating_add(1)
        }
        Representation::SequenceSharedDict {
            dictionary, shared, ..
        } => {
            let d = if dictionary.is_zero() {
                0
            } else {
                resolve(store, pending_depths, *dictionary)
            };
            let s = resolve(store, pending_depths, *shared);
            d.max(s).saturating_add(1)
        }
        _ => 0,
    }
}

/// Whether the reference chain of `base` transitively references `target`
/// (a self-referencing chain is undecodable: materialization would loop
/// until the depth cap). Cycle-safe: the walk is bounded by the depth cap
/// and a visited set. A candidate base whose chain contains the target
/// chunk's own content id must be rejected (§32 exactness, §51 resource
/// bounds).
pub fn chain_contains(
    store: &Store,
    base: &crate::core::candidate::BaseChunk,
    target: &ChunkId,
) -> bool {
    let limits = *store.limits();
    // Bounded worklist: `base` may reference several chunks (Phase-9C
    // SequenceSharedDict references both a file dictionary and a shared
    // dictionary), so every chain branch is walked, each capped by the
    // depth bound and a visited set.
    let mut stack: Vec<(ChunkId, u8)> = vec![(base.id, 0)];
    let mut visited: std::collections::HashSet<ChunkId> = std::collections::HashSet::new();
    while let Some((cur_id, depth)) = stack.pop() {
        if &cur_id == target {
            return true;
        }
        if depth >= limits.max_reference_depth || !visited.insert(cur_id) {
            continue;
        }
        let Some(desc_bytes) = store.chunk_descriptor(&cur_id).ok().flatten() else {
            continue;
        };
        let Ok(desc) = crate::format::descriptor::decode(&desc_bytes, &limits) else {
            continue;
        };
        let mut nexts: Vec<ChunkId> = Vec::new();
        match &desc {
            Representation::ExactRef { target: t, .. } => nexts.push(*t),
            Representation::BaseResidual { base: b, .. } => nexts.push(*b),
            Representation::SequenceDict { dictionary: d, .. } => nexts.push(*d),
            Representation::SequenceSharedDict {
                dictionary, shared, ..
            } => {
                if !dictionary.is_zero() {
                    nexts.push(*dictionary);
                }
                nexts.push(*shared);
            }
            _ => {}
        }
        for n in nexts {
            stack.push((n, depth.saturating_add(1)));
        }
    }
    false
}

/// Flatten a deep chain: when `desc` carries references deeper than the
/// threshold, materialize the final logical bytes and re-encode them at
/// depth 0 through the cheap unguided path. Returns the depth-0 update
/// (byte-exact by construction of `encode_chunk`'s candidates and the
/// materialize-and-compare gate here).
///
/// Units: `start` is the extent byte offset (the re-encode's write
/// offset); `bytes` is the materialized logical content (the CAS ground
/// truth); `cid` is `ChunkId::of(bytes)`. The return is `Ok(None)` when
/// the chain is not deeper than `REBASE_DEPTH_THRESHOLD` or when the
/// re-encode fails the byte-exact gate — never a corrupting candidate.
/// `Err` is reserved for store-level failures (index/materialize errors
/// on the incumbent itself).
pub fn flatten_if_deep(
    store: &Store,
    start: u64,
    desc: &Representation,
    bytes: &[u8],
    cid: &ChunkId,
) -> Result<Option<ExtentUpdate>, StoreError> {
    if chain_depth(store, desc) < REBASE_DEPTH_THRESHOLD {
        return Ok(None);
    }
    let limits = *store.limits();
    let policy = *store.policy();
    // Re-encode through the unguided cheap path (no bases → depth 0).
    let update = Store::encode_chunk(bytes, start, *cid, &limits, &policy)?;
    // §32 gate: the unguided encoder guarantees exactness; verify anyway
    // through a resolver that sees the update's OWN new objects (they are
    // staged, not yet committed — materializing through the bare store
    // would fail on rANS/sequence model and stream objects; found by the
    // SequenceDict background chain, Phase-9B).
    let resolver = crate::optimizer::search::CandidateResolver::new(
        store,
        update
            .objects
            .iter()
            .map(|o| (o.id, o.payload.clone()))
            .collect(),
        None,
    );
    let back = crate::core::materialize::materialize_to_vec(&update.descriptor, &resolver, &limits)
        .map_err(|e| StoreError::Descriptor(e.to_string()))?;
    if back != bytes {
        return Ok(None); // never commit a corrupting flatten
    }
    Ok(Some(update))
}
