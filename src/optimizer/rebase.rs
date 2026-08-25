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
        Representation::ExactRef { .. } | Representation::BaseResidual { .. } => 1,
        _ => 0,
    }
}

/// Resolve the full reference-chain depth of a descriptor by walking its
/// base/target through the chunk index. Bounded by the store's depth cap.
pub fn chain_depth(store: &Store, desc: &Representation) -> u8 {
    let limits = *store.limits();
    let mut depth = 0u8;
    // Owned descriptors along the chain (each references the next).
    let mut chain: Vec<Representation> = vec![desc.clone()];
    while depth < limits.max_reference_depth {
        let cur = &chain[chain.len() - 1];
        let next_id = match cur {
            Representation::ExactRef { target, .. } => Some(*target),
            Representation::BaseResidual { base, .. } => Some(*base),
            _ => None,
        };
        let Some(id) = next_id else { break };
        let Some(desc_bytes) = store.chunk_descriptor(&id).ok().flatten() else {
            break; // unresolvable: the extent is corrupt; not our call here
        };
        let Ok(next) = crate::format::descriptor::decode(
            &desc_bytes,
            limits.max_descriptor_bytes,
            limits.max_inline_bytes,
            limits.max_palette,
            limits.max_period,
            limits.max_chunk_size,
        ) else {
            break;
        };
        depth = depth.saturating_add(1);
        chain.push(next);
    }
    depth
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
    let mut visited: Vec<ChunkId> = Vec::new();
    let mut cur_id = base.id;
    for _ in 0..limits.max_reference_depth {
        if &cur_id == target {
            return true;
        }
        if visited.contains(&cur_id) {
            return false; // cycle without the target: decodable (capped)
        }
        visited.push(cur_id);
        let Some(desc_bytes) = store.chunk_descriptor(&cur_id).ok().flatten() else {
            return false;
        };
        let Ok(desc) = crate::format::descriptor::decode(
            &desc_bytes,
            limits.max_descriptor_bytes,
            limits.max_inline_bytes,
            limits.max_palette,
            limits.max_period,
            limits.max_chunk_size,
        ) else {
            return false;
        };
        let next = match &desc {
            Representation::ExactRef { target: t, .. } => Some(*t),
            Representation::BaseResidual { base: b, .. } => Some(*b),
            _ => None,
        };
        let Some(next) = next else {
            return false;
        };
        cur_id = next;
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
    // The unguided encoder guarantees exactness; verify anyway (§32 gate
    // for every committed representation).
    let back = crate::core::materialize::materialize_to_vec(&update.descriptor, store, &limits)
        .map_err(|e| StoreError::Descriptor(e.to_string()))?;
    if back != bytes {
        return Ok(None); // never commit a corrupting flatten
    }
    Ok(Some(update))
}
