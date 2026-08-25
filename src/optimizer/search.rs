//! DSFB-guided candidate search (ADR-0004, §14, §16).
//!
//! The search is the only place that turns a target chunk into a
//! committed representation:
//!
//! 1. exact dedup (P2) — always first (§12);
//! 2. cheap structural families + rANS + RAW — always (§16 foreground);
//! 3. base+residual channels (P0 prev-version, P1 adjacent, P3 prev-in-
//!    file, P4 family base) and the entropy-universe negative control
//!    (P5) — evaluated in DSFB trust order, bounded by the plan budget.
//!
//! Every candidate is validated byte-exact before it may win (§32); the
//! winner is chosen by exact deterministic cost (ADR-0010). DSFB never
//! decides bytes — it only orders the search and sizes the budget. A
//! poorly predicted DSFB wastes CPU, never data.

#![forbid(unsafe_code)]

use crate::core::candidate::{
    Candidate, CandidateContext, Encoder, pick_cheapest, validate_candidate,
};
use crate::core::extent::ChunkId;
use crate::core::materialize::{DecoderContext, materialize_to_vec};
use crate::core::representation::Representation;
use crate::dsfb::drift::Regime;
use crate::dsfb::features::{Channel, ChunkKey, Features};
use crate::dsfb::selection::{SearchPlan, SearchStrategy};
use crate::optimizer::policy::OptimizeOptions;
use crate::store::{ExtentUpdate, Store, StoreError};

/// How aggressively the search may spend (foreground stays cheap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Write path: dedup + structural + rANS + RAW + in-hand P0, plus at
    /// most one extra high-trust base. Latency-conscious (§16).
    Foreground,
    /// Optimizer pass: full plan-ordered search with the plan budget.
    Background,
}

/// Search inputs for one logical chunk.
#[derive(Debug)]
pub struct GuidedContext<'a> {
    /// File inode (for the DSFB chunk key and file-relative channels).
    pub ino: u64,
    /// Logical offset of the chunk start.
    pub offset: u64,
    /// Target chunk bytes (the logical truth that must be reproduced).
    pub target: &'a [u8],
    /// P0: the previous version of this chunk, when known (write path).
    pub prev_version: Option<crate::core::candidate::BaseChunk>,
    /// The previous same-file chunk (the SequenceDict dictionary,
    /// Phase-9B). Foreground: the batch overlay / RMW bytes (nearly
    /// free); background: the committed previous chunk.
    pub dictionary: Option<crate::core::candidate::BaseChunk>,
    /// The batch's pending state (Phase-8C): chunks already encoded in
    /// this group-commit transaction, so exact dedup can see the batch's
    /// own entries (they are not yet in the committed chunk index).
    /// `None` for single-write and background paths.
    pub pending: Option<&'a PendingBatch>,
    /// Search mode.
    pub mode: SearchMode,
}

/// The in-batch pending state (Phase-8C write aggregation). A group-commit
/// batch stages all its records in one transaction; the dedup lookup reads
/// the committed chunk index, so without this view the batch's own chunks
/// would never dedup against each other.
#[derive(Debug, Default)]
pub struct PendingBatch {
    /// Content id → encoded descriptor of the FIRST occurrence of that
    /// content in the batch. First occurrence wins because the persisted
    /// chunk-index entry is exactly that update's descriptor; EXACT_REF
    /// descriptors are skipped (their content resolves through the
    /// committed index, and a self-referencing pending entry would loop
    /// at validation).
    pub descriptors: std::collections::HashMap<ChunkId, Vec<u8>>,
    /// Object id → payload of every object staged by the batch so far
    /// (needed to materialize a pending descriptor during §32
    /// validation; the objects commit in the same transaction).
    pub objects: std::collections::HashMap<ChunkId, Vec<u8>>,
    /// Content id → reference depth of the FIRST-occurrence descriptor
    /// (Phase-9B): the depth a SequenceDict candidate would inherit if it
    /// used that chunk as its dictionary. Depth 0 for terminal families;
    /// `1 + dictionary_depth` for SEQUENCE_DICT; 1 for EXACT_REF/
    /// BASE_RESIDUAL chains built on committed chunks. Registered with
    /// the descriptor so in-batch dictionary chains stay within
    /// `max_reference_depth`.
    pub depths: std::collections::HashMap<ChunkId, u8>,
}

/// Outcome of a guided search.
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    /// The validated winning extent update (ready to commit).
    pub update: ExtentUpdate,
    /// Number of candidate representations evaluated.
    pub evaluated: usize,
    /// Number of base channels tried (for DSFB evidence).
    pub bases_tried: Vec<(Channel, bool)>,
    /// Winning channel attribution.
    pub winner: Channel,
    /// Reference depth of the winning representation (0 for terminal
    /// families; base/dictionary chains add their depth). Used by the
    /// batch to register in-batch depths for SequenceDict chaining.
    pub depth: u8,
    /// DSFB regime after this observation.
    pub regime: Regime,
    /// The search plan used.
    pub plan: SearchPlan,
}

/// Channels whose evaluation is budgeted by the DSFB plan (the base and
/// universe channels; dedup/structural/rANS/RAW are always evaluated).
pub const BUDGETED_CHANNELS: [Channel; 5] = [
    Channel::PrevVersion,
    Channel::Adjacent,
    Channel::PrevInFile,
    Channel::FamilyBase,
    Channel::Universe,
];

/// Threshold above which a base channel is trusted enough to spend a
/// foreground materialization on it.
const FOREGROUND_BASE_TRUST: f64 = 0.5;

/// Run the guided search for one chunk. `store` is used read-only for
/// validation and base materialization; the DSFB observer is updated
/// (performance-only state).
pub fn encode_guided(
    store: &Store,
    ctx: &GuidedContext<'_>,
    options: OptimizeOptions,
) -> Result<SearchOutcome, StoreError> {
    let limits = *store.limits();
    let policy = *store.policy();
    let chunk_class = limits.chunk_class;
    if ctx.target.is_empty() {
        return Err(StoreError::Invariant("empty target chunk".into()));
    }
    if ctx.target.len() as u64 > limits.max_chunk_size {
        return Err(StoreError::Invariant(
            "target exceeds max chunk size".into(),
        ));
    }
    let index = ctx.offset / chunk_class;
    let cid = ChunkId::of(ctx.target);
    let key = ChunkKey::new(ctx.ino, index, cid);

    let mut candidates: Vec<(Channel, Candidate)> = Vec::new();
    let mut bases_tried: Vec<(Channel, bool)> = Vec::new();

    // --- P2: exact dedup, always first in the write path (§12). The chunk
    // index (or the batch pending state) maps the content id to a
    // descriptor; a hit is accepted only after materializing the existing
    // chunk and comparing exact bytes. Two candidates are proposed for a
    // hit: reusing the canonical descriptor (zero marginal objects — CAS
    // sharing is a store invariant) and the EXACT_REF alias (gated). The
    // marginally cheapest wins.
    // A background REWRITE of the same extent never dedups profitably: the
    // aliased chunk index entry must stay for decodability, so the
    // apparent savings are vacuous (cross-extent dedup already happened
    // in the foreground write path).
    if ctx.mode == SearchMode::Foreground {
        let mut dd = dedup_candidates(store, ctx.target, cid, &limits, ctx.pending, &options)?;
        if !options.allow_exact_ref {
            // Content-addressed object sharing is a store invariant, not a
            // representation: reusing a canonical descriptor of an allowed
            // family stays legal, only the EXACT_REF alias is gated off.
            dd.retain(|c| !matches!(c.representation, Representation::ExactRef { .. }));
        }
        candidates.extend(dd.into_iter().map(|c| (Channel::SharedContent, c)));
    }

    // --- Cheap structural families + rANS + RAW (always evaluated; they
    // are the workhorses of the write path). ZERO/FILL count as
    // configurational for ablation purposes.
    let base_ctx = CandidateContext {
        limits: &limits,
        policy: &policy,
        content_id: cid,
        bases: &[],
        dedup: None,
    };
    if options.allow_configurational {
        if let Some(z) = crate::core::candidate::zero_candidate(ctx.target, cid, &limits) {
            candidates.push((Channel::Raw, z)); // attribution: structural
        }
        if let Some(f) = crate::core::candidate::fill_candidate(ctx.target, cid) {
            candidates.push((Channel::Raw, f));
        }
    }

    // P0 (previous version) is evaluated early in the foreground: its
    // bytes are already in hand (the RMW read), so it is nearly free, and
    // a decisive win lets the search skip the expensive families.
    if ctx.mode == SearchMode::Foreground && options.allow_bases {
        if let Some(b) = &ctx.prev_version {
            if !crate::optimizer::rebase::chain_contains(store, b, &cid) {
                let p0_ctx = CandidateContext {
                    limits: &limits,
                    policy: &policy,
                    content_id: cid,
                    bases: std::slice::from_ref(b),
                    dedup: None,
                };
                let cands =
                    crate::entropy::residual::BaseResidualEncoder.encode(ctx.target, &p0_ctx);
                candidates.extend(cands.into_iter().map(|c| (Channel::PrevVersion, c)));
                // Large diffs may compress well as a rANS-coded residual.
                let rans_cands =
                    crate::rans::residual::RansResidualEncoder.encode(ctx.target, &p0_ctx);
                candidates.extend(rans_cands.into_iter().map(|c| (Channel::PrevVersion, c)));
                // Shift-aware copy/literal deltas (insertions/deletions).
                let delta_cands = crate::rans::delta::DeltaEncoder.encode(ctx.target, &p0_ctx);
                candidates.extend(delta_cands.into_iter().map(|c| (Channel::PrevVersion, c)));
            }
        }
    }

    // Decisive-win early exit (Phase 6): if a cheap candidate already
    // beats RAW by a large margin, the expensive families (rANS,
    // configurational rank/unrank) cannot plausibly win — skip them and
    // keep the write path latency-conscious (§16). The metric is MARGINAL
    // bytes in the foreground (an object that already exists — committed
    // CAS or the batch pending state — costs zero payload bytes; reusing
    // it is the entire point of the content-addressed store). The
    // BACKGROUND optimizer, by contrast, must be able to REPLACE an
    // existing representation with a denser one, so it orders by FULL
    // persisted bytes and never short-circuits on a marginal-cheap
    // incumbent (Phase-9B: a chunk whose RAW object already exists would
    // otherwise look marginally free and block every re-encoding).
    let raw_bytes = ctx.target.len() as u64;
    let metric = |c: &Candidate| candidate_metric(c, store, ctx.pending, ctx.mode);
    let mut decisive = candidates
        .iter()
        .map(|(_, c)| c)
        .min_by_key(|c| metric(c))
        .map(|c| metric(c) <= raw_bytes / 8)
        .unwrap_or(false);
    if !decisive && options.allow_configurational {
        for enc in [
            Box::new(crate::entropy::sparse::SparseEncoder) as Box<dyn Encoder>,
            Box::new(crate::entropy::palette::PaletteEncoder),
            Box::new(crate::entropy::periodic::PeriodicEncoder),
            Box::new(crate::entropy::sparse64::SparseBlock64Encoder),
        ] {
            candidates.extend(
                enc.encode(ctx.target, &base_ctx)
                    .into_iter()
                    .map(|c| (Channel::Raw, c)),
            );
        }
        decisive = candidates
            .iter()
            .map(|(_, c)| c)
            .min_by_key(|c| metric(c))
            .map(|c| metric(c) <= raw_bytes / 8)
            .unwrap_or(false);
    }
    let mut rans_measurement: Option<f64> = None;
    if options.allow_byte_rans && !decisive {
        // P6: conventional byte-level rANS (the original methodology's
        // "rANS" — the pure entropy coder over the raw alphabet).
        let cands = crate::rans::residual::RansEncoder.encode(ctx.target, &base_ctx);
        if let Some(best_floor) = pick_cheapest(&cands, &policy) {
            rans_measurement = Some(measurement_for_ratio(
                best_floor.cost.persisted_bytes() as f64 / ctx.target.len() as f64,
            ));
        }
        candidates.extend(cands.into_iter().map(|c| (Channel::Rans, c)));
        decisive = candidates
            .iter()
            .map(|(_, c)| c)
            .min_by_key(|c| metric(c))
            .map(|c| metric(c) <= raw_bytes / 8)
            .unwrap_or(false);
    }
    if options.allow_sequence_rans && !decisive {
        // E1: the post-registration local-match floor (SequenceRans).
        let cands = crate::rans::sequence::SequenceEncoder.encode(ctx.target, &base_ctx);
        if let Some(best_floor) = pick_cheapest(&cands, &policy) {
            rans_measurement = Some(measurement_for_ratio(
                best_floor.cost.persisted_bytes() as f64 / ctx.target.len() as f64,
            ));
        }
        candidates.extend(cands.into_iter().map(|c| (Channel::Rans, c)));
    }
    if options.allow_sequence_dict && !decisive {
        // E2 (Phase-9B): the cross-chunk dictionary family. The previous
        // same-file chunk's bytes are already in hand (batch overlay / RMW
        // read in the foreground; the committed previous chunk in the
        // background), so this is nearly free; the depth cap (dictionary
        // chain + 1) keeps dictionary references from chaining unboundedly,
        // and the cycle check rejects a dictionary whose chain contains
        // the target chunk itself. DSFB sizes the rest of the search; the
        // in-hand dictionary is cheap enough to always evaluate.
        if let Some(dict) = &ctx.dictionary {
            if dict.depth.saturating_add(1) <= limits.max_reference_depth
                && !crate::optimizer::rebase::chain_contains(store, dict, &cid)
            {
                let enc = crate::rans::sequence::SequenceDictEncoder {
                    dictionary: dict.id,
                    dict_bytes: dict.bytes.clone(),
                    dict_depth: dict.depth,
                };
                let cands = enc.encode(ctx.target, &base_ctx);
                candidates.extend(cands.into_iter().map(|c| (Channel::PrevInFile, c)));
            }
        }
    }
    if let Some(r) = crate::core::candidate::raw_candidate(ctx.target, cid, &limits) {
        candidates.push((Channel::Raw, r));
    }

    // --- DSFB-guided budgeted channels (bases + universe).
    let plan = if options.allow_dsfb_ranking {
        store.dsfb_plan(&key)
    } else {
        SearchPlan {
            ordered_channels: Channel::ALL.to_vec(),
            strategy: SearchStrategy::Balanced,
            budget: BUDGETED_CHANNELS.len(),
        }
    };
    let mut budget_used = 0usize;
    for (position, &channel) in plan.ordered_channels.iter().enumerate() {
        if !BUDGETED_CHANNELS.contains(&channel) {
            continue;
        }
        if !options.channel_allowed(channel) {
            continue;
        }
        if options.allow_dsfb_ranking && !plan.should_evaluate(channel, position) {
            continue;
        }
        // P0 was already evaluated up front in the foreground (its bytes
        // are in hand); the plan loop only re-evaluates it in background
        // mode where the full plan order matters.
        if ctx.mode == SearchMode::Foreground && channel == Channel::PrevVersion {
            continue;
        }
        // Foreground keeps extra-base materialization rare: only P0 (in
        // hand, free) is always tried; adjacent/prev-in-file/family bases
        // need a high trust or a slew-broadened plan. With DSFB ranking
        // disabled (ablation) the trust gate is bypassed so the ablation
        // measures the channels themselves, not the gating.
        if options.allow_dsfb_ranking
            && ctx.mode == SearchMode::Foreground
            && matches!(
                channel,
                Channel::Adjacent | Channel::PrevInFile | Channel::FamilyBase
            )
            && store.dsfb_trust(&key, channel) <= FOREGROUND_BASE_TRUST
            && plan.strategy != SearchStrategy::Broad
        {
            continue;
        }
        if ctx.mode == SearchMode::Foreground && channel == Channel::Universe {
            continue; // universe is a background-only negative control
        }
        let base = match channel {
            Channel::PrevVersion => ctx.prev_version.clone(),
            Channel::Adjacent => store.base_chunk_at(
                ctx.ino,
                ctx.offset.saturating_add(chunk_class),
                ctx.target.len(),
            )?,
            Channel::PrevInFile => {
                if ctx.offset >= chunk_class {
                    store.base_chunk_at(ctx.ino, ctx.offset - chunk_class, ctx.target.len())?
                } else {
                    None
                }
            }
            Channel::FamilyBase => {
                if ctx.offset > 0 {
                    store.base_chunk_at(ctx.ino, 0, ctx.target.len())?
                } else {
                    None
                }
            }
            Channel::Universe => None, // encoded below, no base needed
            _ => None,
        };
        let mut produced = 0usize;
        if let Some(b) = &base {
            // A base whose chain references the target chunk itself would
            // be undecodable (materialization loops until the depth cap);
            // reject it (§51, §32). This is what prevents reference cycles
            // when two chunks reference each other.
            if crate::optimizer::rebase::chain_contains(store, b, &cid) {
                bases_tried.push((channel, false));
                continue;
            }
            let base_ctx = CandidateContext {
                limits: &limits,
                policy: &policy,
                content_id: cid,
                bases: std::slice::from_ref(b),
                dedup: None,
            };
            let cands = crate::entropy::residual::BaseResidualEncoder.encode(ctx.target, &base_ctx);
            produced += cands.len();
            candidates.extend(cands.into_iter().map(|c| (channel, c)));
            // Large diffs may compress well as a rANS-coded residual.
            let rans_cands =
                crate::rans::residual::RansResidualEncoder.encode(ctx.target, &base_ctx);
            produced += rans_cands.len();
            candidates.extend(rans_cands.into_iter().map(|c| (channel, c)));
            // Shift-aware copy/literal deltas (insertions/deletions).
            let delta_cands = crate::rans::delta::DeltaEncoder.encode(ctx.target, &base_ctx);
            produced += delta_cands.len();
            candidates.extend(delta_cands.into_iter().map(|c| (channel, c)));
        }
        if channel == Channel::Universe && options.allow_universe {
            let cands = crate::entropy::universe::UniverseEncoder.encode(ctx.target, &base_ctx);
            produced += cands.len();
            candidates.extend(cands.into_iter().map(|c| (Channel::Universe, c)));
        }
        bases_tried.push((channel, produced > 0));
        if produced > 0 {
            budget_used = budget_used.saturating_add(1);
        }
    }

    // --- Validate (§32) and pick the cheapest valid candidate.
    let (winner_channel, winner) = pick_best_valid(
        store,
        &candidates,
        ctx.target,
        &limits,
        ctx.pending,
        ctx.mode,
    )
    .ok_or_else(|| StoreError::Invariant("no valid candidate (RAW must always work)".into()))?;
    let update = ExtentUpdate {
        offset: ctx.offset,
        descriptor: winner.representation.clone(),
        content_id: cid,
        objects: winner.objects.clone(),
    };

    // --- DSFB observation (performance-only; never affects bytes).
    let mut measurements: Vec<(Channel, f64)> = Vec::new();
    // Re-derive base evidence for the channels we actually tried with a
    // base (cheap: diff summaries are O(n) and bases are already loaded).
    let mut tried: Vec<(Channel, Option<crate::core::candidate::BaseChunk>)> = Vec::new();
    for &(channel, ok) in &bases_tried {
        if !ok {
            continue;
        }
        let owned: Option<crate::core::candidate::BaseChunk> = match channel {
            Channel::PrevVersion => ctx.prev_version.clone(),
            Channel::Adjacent => store.base_chunk_at(
                ctx.ino,
                ctx.offset.saturating_add(chunk_class),
                ctx.target.len(),
            )?,
            Channel::PrevInFile => {
                if ctx.offset >= chunk_class {
                    store.base_chunk_at(ctx.ino, ctx.offset - chunk_class, ctx.target.len())?
                } else {
                    None
                }
            }
            Channel::FamilyBase => {
                if ctx.offset > 0 {
                    store.base_chunk_at(ctx.ino, 0, ctx.target.len())?
                } else {
                    None
                }
            }
            _ => None,
        };
        tried.push((channel, owned));
    }
    for (channel, base) in &tried {
        let f = Features::from_base(*channel, ctx.target, base.as_ref());
        measurements.push((*channel, f.measurement()));
    }
    if let Some(m) = rans_measurement {
        measurements.push((Channel::Rans, m));
    }
    measurements.push((Channel::Raw, 0.5));
    let outcome_quality = outcome_quality(winner_channel, &winner.representation, &measurements);
    let regime = store.dsfb_observe(key, &measurements, winner_channel, outcome_quality);

    Ok(SearchOutcome {
        update,
        evaluated: candidates.len(),
        bases_tried,
        winner: winner_channel,
        depth: winner.cost.depth,
        regime,
        plan,
    })
}

/// Exact dedup (P2): look up the content id in the chunk index (or the
/// batch's pending state), materialize the existing descriptor, and verify
/// the bytes are identical before proposing either:
///
/// - reusing the CANONICAL descriptor (zero marginal objects), restricted
///   to families this configuration admits (`representation_allowed`), or
/// - the EXACT_REF alias (gated separately by `allow_exact_ref`).
///
/// Both are exact representations of the target (§12 — a candidate dedup
/// hit must verify logical length, content identity, and exact bytes);
/// the marginally cheapest wins.
fn dedup_candidates(
    store: &Store,
    target: &[u8],
    cid: ChunkId,
    limits: &crate::core::limits::Limits,
    pending: Option<&PendingBatch>,
    options: &OptimizeOptions,
) -> Result<Vec<Candidate>, StoreError> {
    let desc_bytes = match pending.and_then(|p| p.descriptors.get(&cid)) {
        Some(b) => Some(b.clone()),
        None => store.chunk_descriptor(&cid)?,
    };
    let Some(desc_bytes) = desc_bytes else {
        return Ok(Vec::new());
    };
    let desc = match crate::format::descriptor::decode(
        &desc_bytes,
        limits.max_descriptor_bytes,
        limits.max_inline_bytes,
        limits.max_palette,
        limits.max_period,
        limits.max_chunk_size,
    ) {
        Ok(d) => d,
        Err(_) => return Ok(Vec::new()), // unreadable index entry: miss
    };
    if desc.len() != target.len() as u64 {
        return Ok(Vec::new());
    }
    // Verify exact bytes. For a committed entry the store resolves
    // everything; for a pending entry the descriptor's objects are staged
    // in the same batch (not yet committed), so validation must see them.
    let resolver = CandidateResolver::new(store, std::collections::HashMap::new(), pending);
    if materialize_to_vec(&desc, &resolver, limits).as_deref() != Ok(target) {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(2);
    // Canonical reuse: the descriptor is already persisted (committed) or
    // staged (pending); its objects cost zero marginal bytes. Only
    // families this configuration admits may be reused (the RAW-only
    // ablation must not smuggle a ZERO/PERIODIC canonical in). For
    // ZERO/FILL/PERIODIC this beats an EXACT_REF alias outright.
    if options.representation_allowed(&desc) {
        out.push(Candidate {
            representation: desc.clone(),
            objects: Vec::new(),
            cost: crate::core::cost::CostBreakdown {
                logical_bytes: desc.len(),
                descriptor_bytes: desc.encoded_size(),
                ..Default::default()
            },
            content_id: cid,
        });
    }
    if let Some(alias) = crate::core::candidate::exact_ref_candidate(
        cid,
        cid,
        target.len() as u64,
        target.len() as u64,
        limits,
    ) {
        out.push(alias);
    }
    Ok(out)
}

/// Marginal persisted bytes of a candidate: the encoded descriptor plus
/// the payloads of objects that do NOT already exist (committed CAS or
/// the batch's pending state). An object that already exists costs zero
/// marginal payload bytes — reusing it is the entire point of a
/// content-addressed store. This is the cost that decides between
/// "reuse the canonical descriptor" and "emit a new representation".
fn marginal_bytes(cand: &Candidate, store: &Store, pending: Option<&PendingBatch>) -> u64 {
    let mut total = cand.representation.encoded_size();
    for o in &cand.objects {
        let exists = pending
            .map(|p| p.objects.contains_key(&o.id))
            .unwrap_or(false)
            || store.object_index().contains(&o.id);
        if !exists {
            total = total.saturating_add(o.payload.len() as u64);
        }
    }
    total
}

/// The cost metric that orders candidate validation: MARGINAL bytes in
/// the foreground (an object that already exists — committed CAS or the
/// batch pending state — costs zero payload bytes, so reuse wins), FULL
/// persisted bytes in the background (the optimizer must be able to
/// REPLACE an existing representation with a denser one even when the
/// incumbent's objects already exist).
fn candidate_metric(
    cand: &Candidate,
    store: &Store,
    pending: Option<&PendingBatch>,
    mode: SearchMode,
) -> u64 {
    match mode {
        SearchMode::Foreground => marginal_bytes(cand, store, pending),
        SearchMode::Background => cand.cost.persisted_bytes(),
    }
}

/// Validate candidates (§32) and return the cheapest valid one with its
/// channel attribution. Validation failure of an otherwise-cheap candidate
/// is a hard error path that must fall through to RAW. Each candidate is
/// validated against a resolver that can see the committed store, the
/// candidate's own new objects, and (Phase-8C) the batch's pending
/// descriptors/objects. Ordering is by the mode-appropriate metric:
/// MARGINAL bytes in the foreground (objects that already exist —
/// committed or pending — cost zero, so canonical reuse of a stored
/// descriptor competes fairly with a fresh encoding) and FULL persisted
/// bytes in the background (the incumbent's already-existing objects must
/// not make a denser replacement look expensive).
fn pick_best_valid<'a>(
    store: &Store,
    candidates: &'a [(Channel, Candidate)],
    target: &[u8],
    limits: &crate::core::limits::Limits,
    pending: Option<&'a PendingBatch>,
    mode: SearchMode,
) -> Option<(Channel, &'a Candidate)> {
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by_key(|&i| candidate_metric(&candidates[i].1, store, pending, mode));
    for &i in &order {
        let (channel, cand) = &candidates[i];
        let resolver = CandidateResolver {
            store,
            objects: cand
                .objects
                .iter()
                .map(|o| (o.id, o.payload.clone()))
                .collect(),
            pending_descriptors: pending.map(|p| &p.descriptors),
            pending_objects: pending.map(|p| &p.objects),
        };
        if validate_candidate(cand, target, &resolver, limits).is_ok() {
            return Some((*channel, cand));
        }
    }
    None
}

/// A `DecoderContext` that resolves a candidate's own new objects first,
/// then (Phase-8C) the batch's pending descriptors/objects, and finally
/// falls back to the committed store (for bases/targets/models that
/// already exist). Used for §32 validation (also by the store's commit
/// gate for unguided `encode_chunk` updates).
pub(crate) struct CandidateResolver<'a> {
    store: &'a Store,
    objects: std::collections::HashMap<ChunkId, Vec<u8>>,
    pending_descriptors: Option<&'a std::collections::HashMap<ChunkId, Vec<u8>>>,
    pending_objects: Option<&'a std::collections::HashMap<ChunkId, Vec<u8>>>,
}

impl<'a> CandidateResolver<'a> {
    /// Build a resolver over the committed store plus the given new
    /// objects (and optionally the batch pending state).
    pub(crate) fn new(
        store: &'a Store,
        objects: std::collections::HashMap<ChunkId, Vec<u8>>,
        pending: Option<&'a PendingBatch>,
    ) -> Self {
        Self {
            store,
            objects,
            pending_descriptors: pending.map(|p| &p.descriptors),
            pending_objects: pending.map(|p| &p.objects),
        }
    }
}

impl DecoderContext for CandidateResolver<'_> {
    fn fetch_object(
        &self,
        id: &ChunkId,
    ) -> Result<Vec<u8>, crate::core::materialize::MaterializeError> {
        if let Some(bytes) = self.objects.get(id) {
            return Ok(bytes.clone());
        }
        if let Some(bytes) = self.pending_objects.and_then(|p| p.get(id)) {
            return Ok(bytes.clone());
        }
        self.store.fetch_object_impl(id)
    }

    fn fetch_descriptor(
        &self,
        id: &ChunkId,
    ) -> Result<Representation, crate::core::materialize::MaterializeError> {
        if let Some(bytes) = self.pending_descriptors.and_then(|p| p.get(id)) {
            let limits = *self.store.limits();
            return crate::format::descriptor::decode(
                bytes,
                limits.max_descriptor_bytes,
                limits.max_inline_bytes,
                limits.max_palette,
                limits.max_period,
                limits.max_chunk_size,
            )
            .map_err(|e| {
                crate::core::materialize::MaterializeError::InvalidDescriptor(e.to_string())
            });
        }
        self.store.fetch_descriptor(id)
    }

    fn decode_rans(
        &self,
        model: &[u8],
        encoded: &[u8],
        scale_bits: u8,
        codec: crate::core::representation::RansCodec,
        out_len: u64,
    ) -> Result<Vec<u8>, crate::core::materialize::MaterializeError> {
        self.store
            .decode_rans(model, encoded, scale_bits, codec, out_len)
    }

    fn universe_bytes(
        &self,
        universe: crate::core::representation::UniverseId,
        seed: [u8; 16],
        coordinate: u64,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>, crate::core::materialize::MaterializeError> {
        self.store.universe_bytes(universe, seed, coordinate, range)
    }
}

/// Measurement for an encoded-size ratio (0 = free, 1 = raw-sized).
fn measurement_for_ratio(ratio: f64) -> f64 {
    (1.0 - ratio.clamp(0.0, 1.0)).clamp(0.0, 1.0)
}

/// The outcome-quality scalar fed to the regime tracker: 1.0 for perfect
/// structural/generated wins, the channel measurement for
/// base/rans/raw-driven wins.
fn outcome_quality(winner: Channel, rep: &Representation, measurements: &[(Channel, f64)]) -> f64 {
    match rep {
        Representation::Zero { .. }
        | Representation::Fill { .. }
        | Representation::Sparse { .. }
        | Representation::Palette { .. }
        | Representation::Periodic { .. }
        | Representation::Inline { .. }
        | Representation::ExactRef { .. }
        | Representation::EntropyRef { .. } => 1.0,
        _ => measurements
            .iter()
            .find(|(c, _)| *c == winner)
            .map(|(_, v)| *v)
            .unwrap_or(0.5),
    }
    .clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::candidate::BaseChunk;
    use crate::core::representation::Residual;
    use crate::store::transaction::CrashHooks;
    use crate::store::{NewEntry, Store, StoreConfig};
    use tempfile::TempDir;

    fn create_store(dir: &TempDir) -> Store {
        let cfg = StoreConfig {
            segment_size: 1024 * 1024,
            ..Default::default()
        };
        Store::create(dir.path(), &cfg, [0x44; 16]).unwrap()
    }

    fn ino(store: &Store) -> u64 {
        store
            .create_entry(
                1,
                b"f",
                NewEntry::file(0o644, 1000, 1000),
                &CrashHooks::none(),
            )
            .unwrap()
    }

    fn write(store: &Store, ino: u64, data: &[u8]) {
        store.write_region(ino, 0, data).unwrap();
    }

    fn search(store: &Store, ino: u64, target: &[u8], prev: Option<BaseChunk>) -> SearchOutcome {
        let ctx = GuidedContext {
            ino,
            offset: 0,
            target,
            prev_version: prev,
            dictionary: None,
            pending: None,
            mode: SearchMode::Foreground,
        };
        encode_guided(store, &ctx, OptimizeOptions::default()).unwrap()
    }

    /// Cryptographically uniform deterministic bytes (BLAKE3 of a counter):
    /// no exploitable 4-byte repeats, entropy ≈ 8 bits/symbol. The honest
    /// H6 negative control.
    fn noise(n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n);
        let mut i: u64 = 0;
        while out.len() < n {
            let h = blake3::hash(&i.to_le_bytes());
            let take = (n - out.len()).min(32);
            out.extend_from_slice(&h.as_bytes()[..take]);
            i += 1;
        }
        out
    }

    #[test]
    fn guided_search_matches_exact_bytes() {
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir);
        let f = ino(&store);
        let data: Vec<u8> = (0..65536u32).map(|i| (i % 61) as u8).collect();
        write(&store, f, &data);
        let out = search(&store, f, &data, None);
        // materialize the chosen representation and compare
        let limits = *store.limits();
        let back = materialize_to_vec(&out.update.descriptor, &store, &limits).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn dedup_wins_for_duplicate_content() {
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir);
        let f = ino(&store);
        // Crypto-uniform (incompressible): no structural family can beat
        // the dedup layer for a second copy of the same chunk. The winner
        // is either the canonical-descriptor reuse (a RAW descriptor is
        // object-id + length — cheaper than the alias's own metadata) or
        // the EXACT_REF alias; both carry zero new objects (CAS sharing
        // is the store invariant, EXACT_REF is the gated representation).
        let data = noise(65536);
        write(&store, f, &data);
        let out = search(&store, f, &data, None);
        assert_eq!(out.winner, Channel::SharedContent);
        assert!(
            out.update.objects.is_empty(),
            "dedup must not stage new objects"
        );
        let limits = *store.limits();
        let back = materialize_to_vec(&out.update.descriptor, &store, &limits).unwrap();
        assert_eq!(back, data, "dedup winner must be byte-exact");
        let fresh = materialize_to_vec(&out.update.descriptor, &store, &limits).unwrap();
        assert_eq!(fresh, data);
    }

    #[test]
    fn prev_version_base_wins_for_tiny_edit() {
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir);
        let f = ino(&store);
        // Base: 64 KiB of a repeating pattern (not compressible by rANS to
        // near-zero, so the sparse patch has a chance to win).
        let mut base = Vec::with_capacity(65536);
        for i in 0..65536u32 {
            base.push(((i * 7) % 251) as u8);
        }
        write(&store, f, &base);
        // Target: three single-byte edits.
        let mut target = base.clone();
        target[10] ^= 0x01;
        target[32000] ^= 0x02;
        target[65530] ^= 0x03;
        let prev = BaseChunk {
            id: crate::core::extent::ChunkId::of(&base),
            bytes: base.clone(),
            depth: 0,
        };
        let out = search(&store, f, &target, Some(prev));
        assert!(
            matches!(out.update.descriptor, Representation::BaseResidual { .. }),
            "expected BASE_RESIDUAL, got {:?}",
            out.update.descriptor.family()
        );
        let limits = *store.limits();
        let back = materialize_to_vec(&out.update.descriptor, &store, &limits).unwrap();
        assert_eq!(back, target);
    }

    #[test]
    fn random_data_has_no_fake_density() {
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir);
        let f = ino(&store);
        // H6 negative control: crypto-uniform data must fall back toward
        // RAW — the winner's persisted bytes must be ~raw-sized, and never
        // a structural/configurational/generated family (those would be
        // fabricated density).
        let data = noise(65536);
        let out = search(&store, f, &data, None);
        let persisted = out.update.descriptor.encoded_size()
            + out
                .update
                .objects
                .iter()
                .map(|o| o.payload.len() as u64)
                .sum::<u64>();
        let raw = data.len() as u64 + 41; // payload + descriptor + crc
        assert!(
            persisted >= (raw as f64 * 0.98) as u64,
            "random data must not show fake density: persisted {persisted} vs raw {raw} ({:?})",
            out.update.descriptor.family()
        );
        assert!(
            !matches!(
                out.update.descriptor,
                Representation::Zero { .. }
                    | Representation::Fill { .. }
                    | Representation::Sparse { .. }
                    | Representation::Palette { .. }
                    | Representation::Periodic { .. }
                    | Representation::EntropyRef { .. }
                    | Representation::ExactRef { .. }
            ),
            "structural/generated family on random data: {:?}",
            out.update.descriptor.family()
        );
    }

    #[test]
    fn ablation_raw_only_never_dedups_or_compresses() {
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir);
        let f = ino(&store);
        let zeros = vec![0u8; 65536];
        write(&store, f, &zeros);
        let ctx = GuidedContext {
            ino: f,
            offset: 0,
            target: &zeros,
            prev_version: None,
            dictionary: None,
            pending: None,
            mode: SearchMode::Foreground,
        };
        let out = encode_guided(&store, &ctx, OptimizeOptions::raw_only()).unwrap();
        assert!(matches!(out.update.descriptor, Representation::Raw { .. }));
    }

    #[test]
    fn oversized_descriptor_candidate_is_rejected() {
        // A BaseResidual with a huge RangeReplace residual can beat RAW on
        // byte cost while exceeding max_descriptor_bytes — it must be
        // rejected by validation (a committed oversized descriptor is
        // undecodable: EIO on read, fsck errors). Found by the Phase 6
        // cargo-build SIGBUS investigation.
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir);
        let f = ino(&store);
        // Base: 64 KiB of pattern A.
        let base: Vec<u8> = (0..65536u64)
            .map(|i| (((i.wrapping_mul(7 * 2654435761)) >> 8) % 251) as u8)
            .collect();
        store.write_region(f, 0, &base).unwrap();
        // Target: pattern B (a large, non-compressible diff — RangeReplace
        // literals would exceed the descriptor limit).
        let target: Vec<u8> = (0..65536u64)
            .map(|i| (((i.wrapping_mul(11 * 2654435761)) >> 8) % 251) as u8)
            .collect();
        store.write_region(f, 0, &target).unwrap();
        // The committed representation must be decodable (never an
        // oversized descriptor), and fsck must be clean.
        let limits = *store.limits();
        let inode = store.get_inode(f).unwrap().unwrap();
        let root = match inode.data {
            crate::store::inode::InodeData::File { extent_root } => extent_root,
            _ => unreachable!(),
        };
        let entries =
            crate::store::extent_tree::scan_all(root, 64, limits.max_fanout, &store).unwrap();
        for (_, bytes) in entries {
            assert!(
                bytes.len() as u64 <= limits.max_descriptor_bytes,
                "descriptor exceeds the format limit"
            );
            let d = crate::format::descriptor::decode(
                &bytes,
                limits.max_descriptor_bytes,
                limits.max_inline_bytes,
                limits.max_palette,
                limits.max_period,
                limits.max_chunk_size,
            )
            .unwrap();
            assert!(d.validate(&limits).is_ok());
        }
        let read = store.read_file(f, 0, 65536).unwrap();
        assert_eq!(read, target);
        let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
        assert!(report.is_clean(), "fsck: {}", report.render());
    }

    #[test]
    fn base_depth_accounted_in_costs() {
        // A BaseResidual candidate built on a depth-1 base must carry
        // depth 2 in its cost, so deep chains are penalized by λ_depth.
        let limits = crate::core::limits::Limits::default();
        let policy = crate::core::cost::Policy::default();
        let target: Vec<u8> = (0..64u32).map(|i| (i % 7) as u8).collect();
        let cid = ChunkId::of(&target);
        let base = BaseChunk {
            id: ChunkId::of(&[0xAB; 64]),
            bytes: vec![0xAB; 64],
            depth: 1,
        };
        let ctx = CandidateContext {
            limits: &limits,
            policy: &policy,
            content_id: cid,
            bases: std::slice::from_ref(&base),
            dedup: None,
        };
        let cands = crate::entropy::residual::BaseResidualEncoder.encode(&target, &ctx);
        assert!(!cands.is_empty());
        for c in &cands {
            assert!(c.cost.depth >= 2, "depth should include the base chain");
        }
    }

    #[test]
    fn self_referential_base_is_rejected() {
        // Two chunks that reference each other would be undecodable
        // (materialization loops to the depth cap). The cycle check must
        // reject a base whose chain contains the target chunk's own id.
        let dir = TempDir::new().unwrap();
        let store = create_store(&dir);
        let f = ino(&store);
        let a: Vec<u8> = (0..65536u64)
            .map(|i| (((i.wrapping_mul(11 * 2654435761)) >> 8) % 251) as u8)
            .collect();
        let b: Vec<u8> = (0..65536u64)
            .map(|i| (((i.wrapping_mul(13 * 2654435761)) >> 8) % 251) as u8)
            .collect();
        store.write_region(f, 0, &a).unwrap();
        store.write_region(f, 65536, &b).unwrap();
        // Force chunk A to reference B (as the background pass would for a
        // similar pair): A's logical bytes are `b` with one XOR edit.
        let mut a_bytes = b.clone();
        a_bytes[1] ^= 0x5A;
        let cid_a = ChunkId::of(&a_bytes);
        let cid_b = ChunkId::of(&b);
        let br_a = Representation::BaseResidual {
            base: cid_b,
            base_len: a.len() as u64,
            residual: Residual::XorSparse {
                len: a.len() as u64,
                edits: vec![crate::core::representation::Edit { pos: 1, val: 0x5A }],
            },
            len: a.len() as u64,
        };
        store
            .commit_file_extents(
                f,
                vec![ExtentUpdate {
                    offset: 0,
                    descriptor: br_a.clone(),
                    content_id: cid_a,
                    objects: Vec::new(),
                }],
                None,
                &CrashHooks::none(),
            )
            .unwrap();
        // Now chunk B's base (A) transitively references B: the cycle
        // check must reject it.
        let base = store.base_chunk_at(f, 0, b.len()).unwrap().expect("base");
        assert_eq!(base.id, cid_a);
        assert!(crate::optimizer::rebase::chain_contains(
            &store, &base, &cid_b
        ));
        // An unrelated content id is not in A's chain.
        let unrelated = ChunkId::of(b"unrelated-bytes-for-the-check");
        assert!(!crate::optimizer::rebase::chain_contains(
            &store, &base, &unrelated
        ));
        // And the store stays fully decodable.
        let limits = *store.limits();
        let got_a = materialize_to_vec(&br_a, &store, &limits).unwrap();
        assert_eq!(got_a, a_bytes);
        let got_b = store.read_file(f, 65536, b.len() as u64).unwrap();
        assert_eq!(got_b, b);
    }
}
