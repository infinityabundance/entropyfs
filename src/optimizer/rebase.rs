//! Reference-chain flattening (§11: a base chain must not grow unbounded;
//! background optimization periodically flattens expensive chains).
//!
//! A deep chain (BaseResidual over BaseResidual over ...) trades decode
//! cost and λ_depth for space. Flattening materializes the final bytes and
//! re-encodes them at depth 0. The background pass calls this before the
//! guided search; the cheaper valid candidate wins.

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
pub fn chain_depth(store: &Store, desc: &Representation) -> u8 {
    let limits = *store.limits();
    // Depth of one reference id from the chunk index (capped walk over
    // every branch; returns the deepest chain length).
    fn walk(store: &Store, limits: &crate::core::limits::Limits, id: ChunkId) -> u8 {
        let mut max_depth = 0u8;
        let mut stack: Vec<(ChunkId, u8)> = vec![(id, 0u8)];
        // Node -> deepest depth already explored from it. Re-explore when
        // the current path reaches it deeper than before; skip otherwise.
        let mut visited: std::collections::HashMap<ChunkId, u8> = std::collections::HashMap::new();
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
