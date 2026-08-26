//! Candidate representations: the encoder-side proposal type.
//!
//! An encoder proposes a [`Candidate`]: a representation descriptor, the
//! new objects it needs persisted, and its exact cost. The optimizer
//! pipeline collects candidates, **validates each one** (materialize and
//! compare against the target bytes — §32, non-negotiable), and commits the
//! cheapest valid one.
//!
//! # PURPOSE
//!
//! The proposal type and correctness floor of the encode side: encoders
//! propose, the §32 gate admits, the cost function (ADR-0010) ranks, and
//! the store commits. Also homes the always-available escape-hatch
//! candidates — RAW / ZERO / FILL / INLINE / EXACT_REF — that keep the
//! pipeline total for every input class.
//!
//! # BOUNDARY
//!
//! Pure algebra: no store, no disk format. Encoders are pure (no I/O):
//! bases and dedup hits arrive materialized via [`CandidateContext`].
//! Authority separation (dsfb-selection.md §4): DSFB decides the *search
//! order*, exact cost decides the *winner*, validation decides
//! *admissibility*.
//!
//! # MODEL
//!
//! `Candidate = (representation descriptor, new objects to persist,
//! exact cost, content id of the target bytes)`. `objects` contains only
//! objects this candidate would newly persist — the marginal-bytes rule
//! (existing objects cost zero) is applied by the optimizer at ordering
//! time (cost.rs module doc).
//!
//! # PERSISTENT AUTHORITY
//!
//! Yes: the committed candidate becomes the persisted extent descriptor
//! plus its objects. The §32 gate is the correctness floor that makes
//! every family safe to propose.
//!
//! # CORRECTNESS INVARIANTS
//!
//! - **RAW always exists**: [`raw_candidate`] is total over inputs
//!   `≤ max_chunk_size`. Random data must converge to RAW — a success
//!   condition, not a failure (README; commentary standard §7) — and this
//!   fallback is what bounds worst-case storage at ~1.0× (plus
//!   descriptor/record overhead; the Phase-9A physical floor measured
//!   ~1.00× on urandom). `NoCandidate` must therefore never occur.
//! - **§32 gate**: a candidate is admissible iff it materializes EXACTLY
//!   to the target bytes — length and content
//!   (`materialize(candidate) == X`; ADR-0011). [`validate_candidate`]
//!   checks the content id first (cheap hash pre-filter), then
//!   materializes under `Limits` and byte-compares. A candidate that
//!   fails is a bug in its encoder — the pipeline falls through to RAW.
//! - **P2 exact-dedup semantics**: a [`DedupHit`] is a *verified* existing
//!   identical chunk (length + content id + byte-exact materialization by
//!   the caller); two candidates are proposed for a hit (canonical
//!   descriptor reuse — zero marginal objects — and the EXACT_REF alias)
//!   and the marginally cheapest wins (dsfb-selection.md §7). The alias
//!   is configuration-gated (`allow_exact_ref`) and refuses the ZERO
//!   sentinel as a target.
//! - ZERO / FILL / INLINE guards reject non-matching inputs.
//! - `Candidate.content_id` must equal `ChunkId::of(target)` — identity
//!   is over materialized bytes (ADR-0011).
//!
//! # CONCURRENCY
//!
//! Encoders are stateless and pure, so parallel chunk preparation
//! (Phase-10C) runs candidate search concurrently; nothing here shares
//! mutable state.
//!
//! # RESOURCE BOUNDS
//!
//! `max_chunk_size` gates RAW / ZERO / FILL / INLINE / EXACT_REF;
//! `max_inline_bytes` gates INLINE; validation materializes under
//! `Limits` (decode-work budget, allocation cap, reference depth), so an
//! attacker-shaped candidate cannot spend unbounded CPU or memory in the
//! gate.
//!
//! # FAILURE MODES
//!
//! [`CandidateError`] distinguishes bad chunk classes, missing candidates
//! (impossible by the RAW invariant), validation failures (encoder bugs),
//! materialization errors, and budget exhaustion. The write path treats
//! validation failure of an otherwise-cheap candidate as a hard fall-
//! through to RAW.
//!
//! # HISTORY / EVIDENCE
//!
//! §32 (dsfb-selection.md §4; ADR-0011); Phase-8C (the batch's pending
//! descriptors/objects are visible to the validator, and marginal reuse
//! of committed objects is the write-path rule); Phase-10C (parallel
//! preparation); Phase-9A (~1.00× incompressible floor).

#![forbid(unsafe_code)]

use crate::core::cost::{CostBreakdown, Policy};
use crate::core::extent::ChunkId;
use crate::core::limits::Limits;
use crate::core::materialize::{DecoderContext, MaterializeError, materialize_to_vec};
use crate::core::representation::Representation;

/// Kind of a persisted object record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    /// Arbitrary data payload (raw bytes, rANS stream, residual stream).
    Data,
    /// Encoded rANS model.
    Model,
}

/// A new object a candidate requires the store to persist.
///
/// The id is the content address (BLAKE3 of the payload), so the store
/// CAS-dedups identical payloads — object sharing is a store invariant
/// (Phase-8C attribution: CAS sharing is separate from the gated EXACT_REF
/// alias representation). The kind affects cost accounting: Data payloads
/// are charged via [`account_objects`], model payloads via `estimate`'s
/// `model_bytes` parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRecord {
    /// Content id (BLAKE3 of `payload`).
    pub id: ChunkId,
    /// Object kind.
    pub kind: ObjectKind,
    /// Payload bytes.
    pub payload: Vec<u8>,
}

impl ObjectRecord {
    /// Construct a data object from bytes (content id computed).
    pub fn data(payload: Vec<u8>) -> Self {
        let id = ChunkId::of(&payload);
        Self {
            id,
            kind: ObjectKind::Data,
            payload,
        }
    }

    /// Construct a model object from bytes (content id computed).
    pub fn model(payload: Vec<u8>) -> Self {
        let id = ChunkId::of(&payload);
        Self {
            id,
            kind: ObjectKind::Model,
            payload,
        }
    }
}

/// A candidate representation with its objects and cost.
///
/// `cost` is the FULL per-extent accounting (cost.rs); the foreground's
/// marginal reduction (existing objects cost zero) is applied by the
/// optimizer at ordering time, not here. `objects` holds only the NEW
/// objects this candidate would persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The representation descriptor.
    pub representation: Representation,
    /// New objects to persist for this candidate.
    pub objects: Vec<ObjectRecord>,
    /// Exact cost accounting.
    pub cost: CostBreakdown,
    /// Logical content id of the materialized bytes (== target chunk id).
    pub content_id: ChunkId,
}

/// A candidate family encoder: proposes zero or more candidates for an
/// input chunk. Encoders are pure (no I/O): bases and dedup hits arrive
/// materialized via [`CandidateContext`].
///
/// Encoders must be stateless: the optimizer calls them concurrently
/// (parallel chunk preparation, Phase-10C) and never holds a lock across
/// an encode.
pub trait Encoder {
    /// Encoder name (for explain output and DSFB channel attribution).
    fn name(&self) -> &'static str;

    /// Propose candidates for `input`. Never panics; returns an empty vec
    /// when the family does not apply or cannot represent the chunk.
    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate>;
}

impl Candidate {
    /// Total objective under the policy.
    pub fn total(&self, policy: &Policy) -> u128 {
        self.cost.total(policy)
    }
}

/// A materialized base chunk available to candidate encoders.
#[derive(Debug, Clone)]
pub struct BaseChunk {
    /// Content id of the base.
    pub id: ChunkId,
    /// Materialized base bytes.
    pub bytes: Vec<u8>,
    /// Reference depth the base already contributes.
    pub depth: u8,
}

/// A verified deduplication hit: an existing logical chunk with identical
/// content (length + content id verified, bytes verified by the caller).
///
/// P2 (exact/shared content) semantics: the hit is only as good as the
/// caller's verification — the write path materializes the existing chunk
/// and compares exact bytes before proposing (dsfb-selection.md §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DedupHit {
    /// Content id of the existing identical chunk.
    pub id: ChunkId,
}

/// Context passed to candidate encoders.
#[derive(Debug)]
pub struct CandidateContext<'a> {
    /// Resource limits.
    pub limits: &'a Limits,
    /// Cost policy.
    pub policy: &'a Policy,
    /// Content id of the target bytes.
    pub content_id: ChunkId,
    /// Candidate bases (previous version, adjacent, family base, ...).
    pub bases: &'a [BaseChunk],
    /// Verified deduplication hit, if any.
    pub dedup: Option<DedupHit>,
}

/// Candidate pipeline errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateError {
    /// Target length is not a supported chunk class.
    BadChunkClass(u64),
    /// No candidate could represent the chunk (RAW must always succeed).
    NoCandidate,
    /// Validation failed: candidate materializes to different bytes.
    ValidationFailed,
    /// Materialization error during validation.
    Materialize(MaterializeError),
    /// Internal budget exceeded.
    BudgetExceeded,
}

impl std::fmt::Display for CandidateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for CandidateError {}

/// Validate a candidate by materializing it and comparing against the
/// target bytes (§32). The candidate's own objects plus any context bases
/// and dedup targets must be resolvable through `ctx`.
///
/// # Stages
///
/// 1. Content-id pre-check: `candidate.content_id == ChunkId::of(target)`
///    — a cheap hash filter that catches most lying candidates before any
///    materialization work.
/// 2. Materialize the candidate's representation under `limits` (its own
///    new objects plus context bases/dedup targets resolve through
///    `resolver`).
/// 3. Byte-exact compare: length AND bytes must equal `target`. Anything
///    else is [`CandidateError::ValidationFailed`] — an encoder bug — and
///    the write path falls through to RAW.
pub fn validate_candidate(
    candidate: &Candidate,
    target: &[u8],
    resolver: &dyn DecoderContext,
    limits: &Limits,
) -> Result<(), CandidateError> {
    if candidate.content_id != ChunkId::of(target) {
        return Err(CandidateError::ValidationFailed);
    }
    let out = materialize_to_vec(&candidate.representation, resolver, limits)
        .map_err(CandidateError::Materialize)?;
    if out.len() != target.len() || out != target {
        return Err(CandidateError::ValidationFailed);
    }
    Ok(())
}

/// Pick the cheapest candidate by the policy objective.
///
/// Returns `None` for an empty input (the caller must ensure RAW exists).
pub fn pick_cheapest<'a>(candidates: &'a [Candidate], policy: &Policy) -> Option<&'a Candidate> {
    candidates.iter().min_by_key(|c| c.total(policy))
}

/// The always-available RAW candidate for arbitrary bytes.
///
/// The raw payload becomes a Data object; the descriptor references it.
///
/// RAW is the escape hatch that makes the candidate pipeline total
/// (`docs/adr/0005-representation-set.md`: "RAW — literal bytes (universal
/// escape hatch)"). Random/encrypted/incompressible data must converge to
/// RAW — a success condition, not a failure (README; commentary standard
/// §7) — and this fallback is what bounds worst-case storage at ~1.0×
/// (plus descriptor/record overhead; the Phase-9A physical floor measured
/// ~1.00× on urandom). `NoCandidate` must therefore never occur: for any
/// input `≤ max_chunk_size` this function returns a valid candidate.
pub fn raw_candidate(input: &[u8], content_id: ChunkId, limits: &Limits) -> Option<Candidate> {
    if input.len() as u64 > limits.max_chunk_size {
        return None;
    }
    let obj = ObjectRecord::data(input.to_vec());
    let rep = Representation::Raw {
        obj: obj.id,
        len: input.len() as u64,
    };
    let split = crate::core::cost::ByteSplit {
        reference: 32,
        ..Default::default()
    };
    let cost = account_objects(
        crate::core::cost::estimate(&rep, &split, 0),
        std::slice::from_ref(&obj),
    );
    Some(Candidate {
        representation: rep,
        objects: vec![obj],
        cost,
        content_id,
    })
}

/// The ZERO candidate for all-zero input.
pub fn zero_candidate(input: &[u8], content_id: ChunkId, limits: &Limits) -> Option<Candidate> {
    if input.len() as u64 > limits.max_chunk_size {
        return None;
    }
    if input.iter().any(|&b| b != 0) {
        return None;
    }
    let rep = Representation::Zero {
        len: input.len() as u64,
    };
    let cost = crate::core::cost::estimate(&rep, &Default::default(), 0);
    Some(Candidate {
        representation: rep,
        objects: Vec::new(),
        cost,
        content_id,
    })
}

/// The FILL candidate for constant-byte input.
pub fn fill_candidate(input: &[u8], content_id: ChunkId) -> Option<Candidate> {
    let value = *input.first()?;
    if input.iter().any(|&b| b != value) {
        return None;
    }
    let rep = Representation::Fill {
        value,
        len: input.len() as u64,
    };
    let cost = crate::core::cost::estimate(&rep, &Default::default(), 0);
    Some(Candidate {
        representation: rep,
        objects: Vec::new(),
        cost,
        content_id,
    })
}

/// The INLINE candidate for small inputs stored inside the descriptor.
pub fn inline_candidate(input: &[u8], content_id: ChunkId, limits: &Limits) -> Option<Candidate> {
    if input.len() as u64 > limits.max_inline_bytes || input.is_empty() {
        return None;
    }
    let rep = Representation::Inline {
        data: input.to_vec(),
    };
    let cost = crate::core::cost::estimate(&rep, &Default::default(), 0);
    Some(Candidate {
        representation: rep,
        objects: Vec::new(),
        cost,
        content_id,
    })
}

/// The EXACT_REF candidate for a verified deduplication hit.
///
/// P2 (exact/shared content) semantics: the hit is a *verified* existing
/// logical chunk (length + content id + byte-exact materialization by the
/// caller; see [`DedupHit`]). For a hit the search proposes two
/// candidates — canonical descriptor reuse (zero marginal objects) and
/// this alias — and the marginally cheapest wins (dsfb-selection.md §7).
/// The alias is configuration-gated (`allow_exact_ref`); the ZERO
/// sentinel is refused as a target (never a real chunk); `len` must not
/// exceed the target's length. EXACT_REF contributes one reference-depth
/// level (cost.rs), so alias chains are capped by `max_reference_depth`.
pub fn exact_ref_candidate(
    target: ChunkId,
    content_id: ChunkId,
    len: u64,
    target_len: u64,
    limits: &Limits,
) -> Option<Candidate> {
    if target.is_zero() || len > limits.max_chunk_size || len > target_len {
        return None;
    }
    let rep = Representation::ExactRef {
        target,
        off: 0,
        len,
    };
    let split = crate::core::cost::ByteSplit {
        reference: 32,
        ..Default::default()
    };
    let cost = crate::core::cost::estimate(&rep, &split, 0);
    Some(Candidate {
        representation: rep,
        objects: Vec::new(),
        cost,
        content_id,
    })
}

/// Include the persisted payload bytes of a candidate's new objects in its
/// cost (§15: every persistent bit necessary to decode the extent is
/// accounted). Data payloads count as [`CostBreakdown::object_payload_bytes`];
/// model payloads are accounted separately by the encoders.
///
/// This charges the FULL payload of the candidate's own NEW objects. The
/// marginal rule — an object that already exists (committed CAS or batch
/// pending) costs zero — is applied by the optimizer at ordering time in
/// the foreground; the background regime uses the full total. Both regimes
/// build on this honest per-candidate accounting (cost.rs module doc).
pub fn account_objects(mut cost: CostBreakdown, objects: &[ObjectRecord]) -> CostBreakdown {
    for o in objects {
        if o.kind == ObjectKind::Data {
            cost.object_payload_bytes = cost
                .object_payload_bytes
                .saturating_add(o.payload.len() as u64);
        }
    }
    cost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_candidate_always_valid() {
        let limits = Limits::default();
        let policy = Policy::default();
        let data: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
        let cid = ChunkId::of(&data);
        let cand = raw_candidate(&data, cid, &limits).unwrap();
        assert_eq!(cand.representation.len(), 1024);
        assert_eq!(cand.total(&policy), cand.cost.total(&policy));
        // The candidate's own object must resolve; build a tiny resolver.
        let map: std::collections::HashMap<ChunkId, Vec<u8>> = cand
            .objects
            .iter()
            .map(|o| (o.id, o.payload.clone()))
            .collect();
        let resolver = crate::tests::helpers::MemResolver::from_map(map);
        validate_candidate(&cand, &data, &resolver, &limits).unwrap();
    }

    #[test]
    fn zero_candidate_only_for_zeros() {
        let limits = Limits::default();
        let zeros = vec![0u8; 4096];
        let cid = ChunkId::of(&zeros);
        let cand = zero_candidate(&zeros, cid, &limits).unwrap();
        assert_eq!(cand.representation.len(), 4096);
        let not_zeros = vec![1u8; 4096];
        let cid2 = ChunkId::of(&not_zeros);
        assert!(zero_candidate(&not_zeros, cid2, &limits).is_none());
    }

    #[test]
    fn pick_cheapest_prefers_zero() {
        let limits = Limits::default();
        let policy = Policy::default();
        let zeros = vec![0u8; 4096];
        let cid = ChunkId::of(&zeros);
        let z = zero_candidate(&zeros, cid, &limits).unwrap();
        let r = raw_candidate(&zeros, cid, &limits).unwrap();
        let cands = [r.clone(), z.clone()];
        let best = pick_cheapest(&cands, &policy).unwrap();
        assert!(matches!(best.representation, Representation::Zero { .. }));
    }
}
