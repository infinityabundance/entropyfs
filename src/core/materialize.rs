//! The bounded materializer: `X = Materialize(D)`.
//!
//! A deterministic interpreter over representation descriptors. Every loop
//! is length-bounded by persisted lengths validated *before* allocation;
//! a deterministic operation budget bounds CPU; reference depth is capped
//! (ADR-0005, `docs/security/resource-bounds.md`).
//!
//! PURPOSE
//!     Turn a representation descriptor (the persisted program) into its
//!     exact bytes, under hard resource bounds. This is the read path's
//!     materialization engine and the §32 byte-exactness authority: the
//!     bytes a descriptor produces are what every read, fsck check,
//!     validation comparison, and dedup verification compares against.
//!
//! BOUNDARY
//!     The materializer knows ONLY the descriptor algebra (`Representation`,
//!     `Residual`), the `DecoderContext` contract, and `Limits`. It never
//!     touches the store, never writes, never selects representations (that
//!     is `optimizer::search`), and never parses descriptor bytes itself:
//!     it receives already-decoded, already-structurally-validated
//!     descriptors (`format::descriptor::decode` validates internally since
//!     Phase-11A — decode-OK implies validate-OK). Its job is the
//!     INDEPENDENT runtime enforcement of the resource bounds (allocation
//!     size, work budget, reference depth) on top of that structural
//!     contract.
//!
//! MODEL
//!     `X = Materialize(D)`. The descriptor is the authoritative program;
//!     `DecoderContext` resolves the external operands (objects, referenced
//!     chunk descriptors, rANS model/streams, entropy universes). The
//!     result is a pure function of the descriptor plus the context bytes:
//!     same descriptor, same resolved bytes, same output, on every machine
//!     and every run. Units: `output` is `desc.len()` bytes exactly.
//!
//! PERSISTENT AUTHORITY
//!     None directly — the materializer writes nothing. But its behavior
//!     IS the meaning of every persisted descriptor: changing what
//!     `Materialize(D)` returns changes what every existing descriptor
//!     decodes to. Descriptor semantics are therefore format-stable
//!     (ADR compatibility rules); the hostile-media corpus pins valid
//!     seeds' materialized bytes so a future change cannot silently
//!     re-interpret the on-disk format.
//!
//! CORRECTNESS INVARIANTS
//!     - `output.len() == desc.len()` exactly, or a typed error (never a
//!       partial silent write);
//!     - determinism: no wall clock, no RNG, no iteration order that
//!       affects bytes;
//!     - every allocation is bounded against `max_alloc_bytes` /
//!       `max_chunk_size` BEFORE it happens — a hostile length field can
//!       never drive an over-budget `vec!`;
//!     - every step spends the operation budget (`spend`), so total CPU is
//!       bounded by `max_decode_work` even for adversarial command
//!       streams;
//!     - reference hops (EXACT_REF, BASE_RESIDUAL, SequenceDict,
//!       SequenceSharedDict) increment `depth` and enforce
//!       `max_reference_depth` — the depth cap is what makes cross-chunk
//!       references bounded random access (Phase-9B/9C) and prevents
//!       reference cycles from looping;
//!     - structural consistency of the descriptor (internal length fields
//!       matching `desc.len()`, palette/count agreement, residual length
//!       matching the representation length) is guaranteed by the
//!       validate-before-allocation pipeline (Phase-11A); the materializer
//!       re-checks the runtime-relevant subset independently — it never
//!       trusts a descriptor merely because it parsed.
//!
//! CONCURRENCY
//!     The materializer itself holds no locks and touches no shared state;
//!     `budget` is a caller-owned counter and `ctx` is caller-supplied.
//!     Callers may therefore run it concurrently (the Phase-10C parallel
//!     chunk prefill, the Phase-11E worker pool) provided the `DecoderContext`
//!     implementation is thread-safe — the store's context is (read-only
//!     committed-state access; see `store::Store`'s concurrency notes).
//!
//! DURABILITY
//!     None: materialization only READS persisted bytes. A successful
//!     return means the bytes were produced from what is on disk; a typed
//!     error means the descriptor is undecodable given the current backing
//!     state. Persistence itself is the write path's job.
//!
//! RESOURCE BOUNDS
//!     Attacker-controlled sizes that can reach this code: the descriptor's
//!     declared `len` (checked against `max_chunk_size` and the caller's
//!     output), every allocation derived from it (checked against
//!     `max_alloc_bytes`), model objects (checked against
//!     `max_model_bytes`), command counts (bounded by `cmds <= len` — every
//!     command writes ≥ 1 byte), stream lengths (bounded by the sequence
//!     decoders' own `Limits` checks), and reference depth (checked against
//!     `max_reference_depth` per hop). Each bound is enforced before the
//!     allocation or loop it guards.
//!
//! PERFORMANCE
//!     Decode cost is O(desc.len()) per chunk with a per-step budget;
//!     bulk families (ZERO/FILL) are charged once up front
//!     (`spend(desc.len() / 8 + 1)`) instead of per byte. The Phase-10F
//!     `read_many` transport batches a materialization's object/model/stream
//!     dependencies into one submission; the Phase-11D oracle shows the
//!     resulting decode time is dominated by useful CPU, and the 11E pool
//!     makes decode work task-level fair across concurrent requests.
//!
//! FAILURE MODES
//!     Every failure is a typed `MaterializeError` (see the enum); none
//!     panics. The states that must NEVER occur are panic, OOM, unbounded
//!     CPU, unbounded recursion, and silent wrong bytes — the Phase-11A
//!     hostile-media materialization-graph court asserts exactly this over
//!     fuzz-defined descriptor graphs (`src/tests/hostile_media/graph_court.rs`;
//!     sealed evidence `evidence/hostile-media/court-1787750784-a2983dc/`).
//!
//! HISTORY / EVIDENCE
//!     ADR-0005 defined the resource limits; `docs/security/resource-bounds.md`
//!     documents their enforcement points. Phase-10E found the diamond-depth
//!     bug class (locally valid descriptors composing into globally
//!     undecodable graphs) — the depth cap and the longest-path depth
//!     accounting in `optimizer::rebase` exist because of it. Phase-11A
//!     closed the read-path layering gap (decode now validates internally)
//!     and sealed the graph court. Phase-9B/9C introduced the dictionary
//!     families whose depth-capped references make bounded random access
//!     safe here.

#![forbid(unsafe_code)]

use std::ops::Range;

use crate::core::extent::ChunkId;
use crate::core::limits::Limits;
use crate::core::representation::{RansCodec, Representation, Residual, TransformId, UniverseId};

/// External services the materializer needs: object fetch, chunk-descriptor
/// resolution (for EXACT_REF / dictionary bases), rANS decode, and universe
/// materialization.
///
/// Implemented by the store layer (`store::Store`), by the search path's
/// candidate validator (`optimizer::search::CandidateResolver`), and by the
/// hostile-media court's in-memory `HostileResolver`; `core` only defines
/// the contract. The materializer never knows WHICH implementation it is
/// talking to — that is what lets the same interpreter run against a real
/// store, a staged candidate, and a fuzz-defined hostile graph.
///
/// CONTRACT (per method):
/// - `fetch_object` / `fetch_descriptor`: resolve by content id; return the
///   object bytes / decoded+validated descriptor, or a typed error
///   (`MissingObject` / `MissingChunk` / `InvalidDescriptor`). The context
///   owns the authenticity check at its layer (the store verifies the
///   authenticated-bytes binding; the hostile resolver decodes through the
///   real codec with limits).
/// - `decode_rans`: decode a rANS stream with the given model bytes to
///   exactly `out_len` bytes, or a typed error. The model/stream bytes are
///   untrusted here — the decoder is the hostile-media court's deep-parse
///   target.
/// - `universe_bytes`: materialize `range` bytes from an entropy universe
///   (deterministic XOF output; `EntropyRef`'s generated operand).
///
/// RESOURCE BOUNDS: fetched object sizes are bounded by the implementing
/// layer (the store's physical record caps, the court's input caps); the
/// materializer bounds every allocation it makes itself before making it,
/// so a hostile context cannot drive an over-budget `vec!` here.
pub trait DecoderContext {
    /// Fetch a persisted object's bytes by content id.
    fn fetch_object(&self, id: &ChunkId) -> Result<Vec<u8>, MaterializeError>;

    /// Resolve a logical chunk id to its representation descriptor
    /// (chunk index lookup).
    fn fetch_descriptor(&self, id: &ChunkId) -> Result<Representation, MaterializeError>;

    /// Decode a rANS stream with the given model bytes.
    fn decode_rans(
        &self,
        model: &[u8],
        encoded: &[u8],
        scale_bits: u8,
        codec: RansCodec,
        out_len: u64,
    ) -> Result<Vec<u8>, MaterializeError>;

    /// Materialize `range` bytes from an entropy universe.
    fn universe_bytes(
        &self,
        universe: UniverseId,
        seed: [u8; 16],
        coordinate: u64,
        range: Range<u64>,
    ) -> Result<Vec<u8>, MaterializeError>;
}

/// Materialization errors. All are typed; none panics.
///
/// Every variant is a REJECTION of the bounded-valid-or-typed-rejection
/// oracle (ADR-0016): the materializer either returns the exact bytes or
/// one of these, and the hostile-media court asserts no other outcome
/// exists (no panic, no OOM, no hang, no silent wrong bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterializeError {
    /// Descriptor validation failed (structural invariant violated — the
    /// descriptor should not have reached the materializer, but a hostile
    /// context may construct one directly).
    InvalidDescriptor(String),
    /// Output length exceeds limits (the declared `len` is over
    /// `max_chunk_size`).
    OutputTooLarge {
        /// Requested length (descriptor-declared, logical bytes).
        requested: u64,
        /// Format maximum (`limits.max_chunk_size`).
        max: u64,
    },
    /// Allocation exceeds limits (an intermediate buffer would exceed
    /// `max_alloc_bytes` — enforced BEFORE the allocation happens).
    AllocTooLarge {
        /// Requested allocation (bytes).
        requested: u64,
        /// Format maximum (`limits.max_alloc_bytes`).
        max: u64,
    },
    /// Reference depth exceeded (a cross-chunk chain is longer than
    /// `max_reference_depth`; the recursion refuses the hop before
    /// fetching the next target).
    DepthExceeded {
        /// Depth reached (number of reference hops, 0 = terminal).
        depth: u8,
        /// Depth cap (`limits.max_reference_depth`).
        max: u8,
    },
    /// Operation budget exceeded (`spend` found fewer than `n` ops
    /// remaining — the adversarial command stream burned `max_decode_work`).
    BudgetExceeded,
    /// Referenced object missing (a RAW/RANS/sequence model or stream
    /// content id is absent from the context's object table).
    MissingObject(ChunkId),
    /// Referenced chunk descriptor missing (EXACT_REF / dictionary base id
    /// is absent from the chunk index).
    MissingChunk(ChunkId),
    /// Referenced range outside the target chunk (an EXACT_REF `off + len`
    /// exceeds the target's length).
    RangeOutOfBounds,
    /// rANS decode failed (bad model or stream).
    RansDecode(String),
    /// SequenceRans decode failed (bad model object, streams, or commands).
    Sequence(String),
    /// Universe materialization failed (unknown universe, bad seed,
    /// coordinate/range violation, or length mismatch).
    Universe(String),
    /// A residual could not be applied (structurally invalid — edits out
    /// of range, exhausted literal/offset streams, copy out of base or
    /// dictionary bounds, or a family invalid for its context).
    Residual(String),
}

impl std::fmt::Display for MaterializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for MaterializeError {}

/// Materialize a chunk descriptor into `output`.
///
/// # What
///
/// Interpret `desc` as a deterministic program and produce its exact
/// `desc.len()` bytes into `output`. This is the read path's single
/// materialization entry point and the §32 byte-exactness authority: the
/// bytes it produces are what every read, fsck check, and dedup
/// verification compares against.
///
/// # Why
///
/// Descriptors are the persisted representation of content; materialization
/// is what turns them back into the bytes applications read. It must be
/// deterministic (same descriptor + same resolved operands ⇒ same bytes on
/// every machine) and bounded (hostile descriptors must fail typed, never
/// panic / OOM / unbounded CPU — ADR-0016).
///
/// # Inputs and authority
///
/// - `desc`: the descriptor program. It arrives ALREADY structurally
///   validated (decode-OK ⇒ validate-OK since Phase-11A) but is still
///   untrusted at the resource level: every length and count is re-checked
///   here against the runtime limits before it drives an allocation or a
///   loop.
/// - `ctx`: the resolver for objects / chunks / streams / universes (the
///   store, the search path's candidate validator, or the hostile-media
///   resolver).
/// - `limits`: the enforcement authority for chunk size, allocation size,
///   model size, reference depth, and the work budget.
/// - `depth`: the current reference depth, 0 at top level; each EXACT_REF /
///   BASE_RESIDUAL / dictionary hop recurses with `depth + 1` and the cap
///   is enforced BEFORE the hop.
/// - `budget`: caller-owned operation counter (starts at
///   `limits.max_decode_work`); every materialize step decrements it and
///   `BudgetExceeded` is returned when exhausted.
/// - `output`: caller-provided buffer of exactly `desc.len()` bytes.
///
/// # Algorithm
///
/// Stage 1 preflight (length, chunk cap, depth cap, initial bulk charge),
/// then Stage 2 dispatch by family: terminal families fill/copy directly;
/// reference families recurse at `depth + 1`; stream families decode
/// through the context; configurational families unrank deterministically.
///
/// # Invariants
///
/// Pre: `output.len() == desc.len()` and `depth <= max_reference_depth`
/// (both enforced, never assumed). Post: on `Ok`, `output` holds exactly
/// `Materialize(desc)`; on `Err`, a typed error and no silent partial
/// write.
///
/// # Concurrency
///
/// No locks, no shared state; safe to call concurrently when `ctx` is
/// thread-safe (see the module doc's CONCURRENCY section — the Phase-10C
/// parallel prefill and the Phase-11E worker pool rely on this).
///
/// # Durability
///
/// Reads only. Success means the bytes were produced from the current
/// backing state; it says nothing about the persistence of those bytes.
///
/// # Resource bounds
///
/// `max_chunk_size` (declared len), `max_alloc_bytes` (every intermediate
/// `vec!`), `max_model_bytes` (fetched models), `max_reference_depth`
/// (each hop), `max_decode_work` (every step). Enforced before the
/// allocation or loop they guard.
///
/// # Failure behavior
///
/// Typed `MaterializeError` only; never panics: `InvalidDescriptor`,
/// `OutputTooLarge`, `AllocTooLarge`, `DepthExceeded`, `BudgetExceeded`,
/// `MissingObject` / `MissingChunk`, `RangeOutOfBounds`, `RansDecode`,
/// `Sequence`, `Universe`, `Residual`.
///
/// # Evidence / rationale
///
/// ADR-0005 / `docs/security/resource-bounds.md` defined the limits.
/// Phase-10E found the depth-bomb / diamond-graph class (locally valid
/// descriptors composing into undecodable graphs) — the depth cap is the
/// fix. Phase-11A sealed the bounded-valid-or-typed-rejection oracle with
/// the hostile-media graph court (`court-1787750784-a2983dc`).
pub fn materialize(
    desc: &Representation,
    ctx: &dyn DecoderContext,
    limits: &Limits,
    depth: u8,
    budget: &mut u64,
    output: &mut [u8],
) -> Result<(), MaterializeError> {
    // -------------------------------------------------------------------
    // Stage 1: preflight — length, chunk cap, depth cap, bulk charge.
    //
    // The descriptor's declared length is checked against BOTH the
    // caller's output buffer and the chunk cap BEFORE any work or
    // allocation: a hostile `len` can never drive an over-budget buffer.
    // `depth` is checked before the hop it guards, and the initial
    // `spend(desc.len() / 8 + 1)` charges the bulk families (ZERO/FILL
    // never spend per byte) so even a maximal declared length is not
    // free CPU. This ordering — validate, then resource-preflight, then
    // materialize — is the read path's pipeline invariant (see the module
    // doc's BOUNDARY section).
    // -------------------------------------------------------------------
    if output.len() as u64 != desc.len() {
        return Err(MaterializeError::InvalidDescriptor(
            "output length does not match descriptor length".into(),
        ));
    }
    if desc.len() > limits.max_chunk_size {
        return Err(MaterializeError::OutputTooLarge {
            requested: desc.len(),
            max: limits.max_chunk_size,
        });
    }
    if depth > limits.max_reference_depth {
        return Err(MaterializeError::DepthExceeded {
            depth,
            max: limits.max_reference_depth,
        });
    }
    spend(desc.len() / 8 + 1, budget)?;

    // -------------------------------------------------------------------
    // Stage 2: dispatch by family.
    //
    // Each arm is one deterministic program. Terminal families (ZERO,
    // FILL, INLINE, RAW) produce bytes directly; RANS and the sequence
    // families decode streams through the context; reference families
    // recurse at `depth + 1`; the configurational families unrank
    // deterministically. Every arm re-checks the runtime-relevant subset
    // of bounds independently of structural validation — materialize
    // never trusts a descriptor merely because it parsed.
    // -------------------------------------------------------------------
    match desc {
        // Terminal families — no external references, no recursion, no
        // intermediate allocations. The only resource question is the
        // bulk fill/copy cost, which Stage 1's initial charge already
        // covered (`spend(desc.len() / 8 + 1)`); ZERO/FILL also cannot
        // carry hostile sub-lengths, so these arms are the cheapest and
        // the safest.
        Representation::Zero { .. } => {
            output.fill(0);
            Ok(())
        }
        Representation::Fill { value, .. } => {
            output.fill(*value);
            Ok(())
        }
        Representation::Inline { data } => {
            // INLINE carries its bytes in the descriptor itself; the
            // preflight already guarantees `output.len() == desc.len() ==
            // data.len()`, and this re-check keeps the arm self-defending
            // if a context ever builds an inconsistent descriptor.
            if data.len() != output.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "inline length mismatch".into(),
                ));
            }
            output.copy_from_slice(data);
            Ok(())
        }
        Representation::Raw { obj, .. } => {
            // RAW is the store's identity representation: the object's
            // bytes ARE the content. The object is fetched by content id
            // (CAS: the id is the bytes' hash, so a matching length + the
            // store's authenticated fetch is the integrity check); the
            // length equality is re-verified before the copy so a hostile
            // context can never feed a wrong-length payload.
            let bytes = ctx.fetch_object(obj)?;
            if bytes.len() as u64 != desc.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "raw object length mismatch".into(),
                ));
            }
            output.copy_from_slice(&bytes);
            Ok(())
        }
        // RANS: the conventional byte-entropy coder. The decoded output
        // is the only allocation, and it is bounded against
        // `max_alloc_bytes` BEFORE `decode_rans` runs — the declared
        // `len` can never drive an over-budget buffer. The model object
        // is also size-checked (the rANS model is attacker-controlled
        // here).
        Representation::Rans {
            model,
            enc_obj,
            scale_bits,
            codec,
            len,
        } => {
            if *len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *len,
                    max: limits.max_alloc_bytes,
                });
            }
            let model_bytes = ctx.fetch_object(model)?;
            if model_bytes.len() as u64 > limits.max_model_bytes {
                return Err(MaterializeError::InvalidDescriptor(
                    "model object too large".into(),
                ));
            }
            let encoded = ctx.fetch_object(enc_obj)?;
            let decoded = ctx.decode_rans(&model_bytes, &encoded, *scale_bits, *codec, *len)?;
            if decoded.len() as u64 != desc.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "rans decoded length mismatch".into(),
                ));
            }
            output.copy_from_slice(&decoded);
            Ok(())
        }
        Representation::ExactRef { target, off, len } => {
            // EXACT_REF: `X = target[off .. off+len]` — the chunk-index
            // alias. Fetch the target chunk descriptor and materialize it
            // with depth+1, then copy the sub-range.
            //
            // DEPTH AND BOUNDED RANDOM ACCESS: the recursion at depth+1 is
            // exactly what makes cross-chunk references bounded — the
            // Stage-1 depth check refuses the hop before the fetch when a
            // hostile graph chains past `max_reference_depth` (Phase-10E
            // depth bombs; the hostile-media graph court's self-reference
            // and cycle exhibits).
            let target_desc = ctx.fetch_descriptor(target)?;
            if *off as u128 + *len as u128 > target_desc.len() as u128 {
                return Err(MaterializeError::RangeOutOfBounds);
            }
            let target_len = target_desc.len();
            // The intermediate full-target buffer is bounded before
            // allocation; `off + len` was already checked against it.
            if target_len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: target_len,
                    max: limits.max_alloc_bytes,
                });
            }
            let mut full = vec![0u8; target_len as usize];
            materialize(&target_desc, ctx, limits, depth + 1, budget, &mut full)?;
            let start = *off as usize;
            let end = start + *len as usize;
            output.copy_from_slice(&full[start..end]);
            Ok(())
        }
        // BASE_RESIDUAL: `X = B ⊕ R` — the target bytes are the base
        // chunk's bytes with a residual overlaid. `base_len` is bounded
        // before the base-buffer allocation, the base descriptor must
        // actually be that long, and the base is materialized at depth+1
        // (so base chains cannot defeat bounded random access either).
        Representation::BaseResidual {
            base,
            base_len,
            residual,
            ..
        } => {
            if *base_len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *base_len,
                    max: limits.max_alloc_bytes,
                });
            }
            let base_desc = ctx.fetch_descriptor(base)?;
            if base_desc.len() != *base_len {
                return Err(MaterializeError::InvalidDescriptor(
                    "base length mismatch".into(),
                ));
            }
            let mut base_bytes = vec![0u8; *base_len as usize];
            materialize(&base_desc, ctx, limits, depth + 1, budget, &mut base_bytes)?;
            apply_residual(residual, &base_bytes, output, ctx, limits, budget)
        }
        // Configurational families: the bytes are DETERMINED by a rank —
        // `unrank` is the exact inverse of the encoder's `rank`, so
        // materialization is a pure combinatorial function with no external
        // references. The rank arithmetic itself is the only CPU; the
        // `unrank_*` helpers are exact and deterministic (they cannot
        // panic on validated input and return typed errors otherwise).
        Representation::Sparse {
            k,
            rank,
            literals,
            len,
        } => {
            output.fill(0);
            let k = *k as usize;
            // `k <= len` is structural (validated), re-checked here so a
            // context-built descriptor cannot drive an out-of-range
            // `unrank_comb_subset`.
            if k as u64 > *len {
                return Err(MaterializeError::InvalidDescriptor(
                    "sparse k exceeds length".into(),
                ));
            }
            let positions = crate::entropy::rank::unrank_comb_subset(*rank, *len, k as u64)
                .map_err(|e| MaterializeError::InvalidDescriptor(e.to_string()))?;
            if positions.len() != k || literals.len() != k {
                return Err(MaterializeError::InvalidDescriptor(
                    "sparse rank/literal mismatch".into(),
                ));
            }
            for (i, &pos) in positions.iter().enumerate() {
                output[pos as usize] = literals[i];
            }
            Ok(())
        }
        Representation::Palette {
            palette,
            counts,
            rank,
            len,
        } => {
            // The multinomial unrank yields one symbol index per output
            // position; symbols are in `0..palette.len()` (structural —
            // validate requires `counts.len() == palette.len()` and no
            // zero count, so every symbol actually appears), and the
            // symbol-sequence length is re-verified against the output.
            let symbols = crate::entropy::rank::unrank_multinomial(*rank, *len, counts)
                .map_err(|e| MaterializeError::InvalidDescriptor(e.to_string()))?;
            if symbols.len() != output.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "palette unrank length mismatch".into(),
                ));
            }
            for (i, &s) in symbols.iter().enumerate() {
                output[i] = palette[s as usize];
            }
            Ok(())
        }
        Representation::Periodic {
            period,
            pattern,
            count,
            tail,
            ..
        } => {
            // `X = pattern repeated count times, then tail`. The pattern is
            // repeated with an explicit per-iteration spend so a maximal
            // count cannot burn unbounded CPU, and each repeat is bounds-
            // checked against the output before the copy (a hostile
            // count/period combination cannot overflow the buffer).
            let period = *period as usize;
            let count = *count as usize;
            if period == 0 || pattern.len() != period {
                return Err(MaterializeError::InvalidDescriptor(
                    "periodic pattern mismatch".into(),
                ));
            }
            let mut written = 0usize;
            for _ in 0..count {
                let end = written + period;
                if end > output.len() {
                    return Err(MaterializeError::InvalidDescriptor(
                        "periodic overflow".into(),
                    ));
                }
                output[written..end].copy_from_slice(pattern);
                written = end;
                spend(period as u64 / 8 + 1, budget)?;
            }
            if written + tail.len() != output.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "periodic tail mismatch".into(),
                ));
            }
            output[written..].copy_from_slice(tail);
            Ok(())
        }
        // ENTROPY_REF: `X = T(E) ⊕ R` — the bytes are generated from an
        // entropy universe (a deterministic XOF stream) and then a residual
        // is overlaid. v1 supports only `Identity` transform; the residual
        // length must equal the generated length (structural, re-checked
        // per arm). This is the only family whose "content" comes from a
        // generator rather than stored bytes — the hostile-media court's
        // universe exhibits exercise exactly this boundary.
        Representation::EntropyRef {
            universe,
            seed,
            coordinate,
            transform,
            residual,
            len,
        } => {
            if *transform != TransformId::Identity {
                return Err(MaterializeError::InvalidDescriptor(
                    "unsupported transform".into(),
                ));
            }
            let generated = ctx.universe_bytes(*universe, *seed, *coordinate, 0..*len)?;
            if generated.len() as u64 != *len {
                return Err(MaterializeError::Universe(
                    "universe length mismatch".into(),
                ));
            }
            // X = T(E) ⊕ R with T = identity ⇒ X = E ⊕ R.
            match residual {
                Residual::XorSparse { len: rlen, edits } => {
                    if *rlen != *len {
                        return Err(MaterializeError::Residual(
                            "entropy residual length mismatch".into(),
                        ));
                    }
                    output.copy_from_slice(&generated);
                    for e in edits {
                        if (e.pos as u64) >= *len {
                            return Err(MaterializeError::Residual(
                                "entropy residual edit out of range".into(),
                            ));
                        }
                        output[e.pos as usize] ^= e.val;
                    }
                    Ok(())
                }
                Residual::RangeReplace { .. } => Err(MaterializeError::Residual(
                    "range-replace residual not valid for entropy ref v1".into(),
                )),
                Residual::RansCoded { .. } => {
                    // X = E ⊕ D where D is the decoded residual stream.
                    let mut diff = vec![0u8; *len as usize];
                    apply_residual(residual, &generated, &mut diff, ctx, limits, budget)?;
                    // apply_residual computed X = E ⊕ D into `diff` already
                    // (base = generated, out = diff), so copy it over.
                    output.copy_from_slice(&diff);
                    Ok(())
                }
                Residual::BaseSequence { .. } => Err(MaterializeError::Residual(
                    "base-sequence residual not valid for entropy ref v1".into(),
                )),
            }
        }
        // PERMUTATION: the bytes are an alphabet permuted by `rank`. The
        // alphabet must be strictly increasing (canonical — validated) and
        // the permutation length is capped at 34 (the rank arithmetic's
        // factorial bound; `m > 34` would overflow `u128` factorial
        // tables, which is why the cap exists and is re-checked here).
        Representation::Permutation {
            rank,
            alphabet,
            len,
        } => {
            let m = *len as usize;
            if m == 0 || m > 34 {
                return Err(MaterializeError::InvalidDescriptor(
                    "permutation length out of range".into(),
                ));
            }
            if alphabet.len() != m {
                return Err(MaterializeError::InvalidDescriptor(
                    "permutation alphabet mismatch".into(),
                ));
            }
            let seq = crate::entropy::rank::unrank_permutation(*rank, m)
                .map_err(|e| MaterializeError::InvalidDescriptor(e.to_string()))?;
            // seq is the permutation of 0..m; map through the sorted
            // alphabet to recover the bytes.
            for (i, &idx) in seq.iter().enumerate() {
                output[i] = alphabet[idx as usize];
            }
            Ok(())
        }
        // SEQUENCE_RANS (E1): the post-registration local-match floor — a
        // copy/literal command stream over a rANS-compressed literal
        // pool, with byte-progressive backward copies (LZ-style, overlap
        // allowed). Every command writes ≥ 1 byte, so the command count
        // is bounded by the output length — that single inequality bounds
        // the stream allocations below it. The walk itself re-checks every
        // run, distance, and stream cursor, and spends per command.
        Representation::SequenceRans {
            model,
            enc_obj,
            scale_bits,
            codec,
            seq_len,
            lit_len,
            off_len,
            cmds,
            lit_out,
            len,
        } => {
            if *len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *len,
                    max: limits.max_alloc_bytes,
                });
            }
            // Every command writes at least one byte, so the command count
            // can never exceed the output length (bounds the decode_rans
            // allocation below).
            if (*cmds as u64) > *len {
                return Err(MaterializeError::InvalidDescriptor(
                    "sequence command count exceeds output length".into(),
                ));
            }
            let d = crate::rans::sequence::decode_three_streams(
                ctx,
                limits,
                crate::rans::sequence::StreamRefs {
                    model: *model,
                    enc_obj: *enc_obj,
                    scale_bits: *scale_bits,
                    codec: *codec,
                },
                crate::rans::sequence::ThreeStreams {
                    seq_len: *seq_len,
                    lit_len: *lit_len,
                    off_len: *off_len,
                    cmds: *cmds,
                    lit_out: *lit_out,
                },
                None,
                2,
            )?;
            let (commands, literals, offsets) = (d.commands, d.literals, d.offsets);

            // Walk the commands (byte-progressive copy; overlap allowed).
            let mut pos = 0usize;
            let mut lit = 0usize;
            let mut off = 0usize;
            for &cmd in &commands {
                if cmd < 0x80 {
                    let run = cmd as usize + 1;
                    if pos + run > output.len() || lit + run > literals.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "literal run overflow".into(),
                        ));
                    }
                    output[pos..pos + run].copy_from_slice(&literals[lit..lit + run]);
                    pos += run;
                    lit += run;
                    spend(run as u64, budget)?;
                } else {
                    let clen = cmd as usize - 0x80 + 4;
                    if off + 2 > offsets.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "copy offset exhausted".into(),
                        ));
                    }
                    let dist = u16::from_le_bytes([offsets[off], offsets[off + 1]]) as usize;
                    off += 2;
                    if dist == 0 || dist > pos {
                        return Err(MaterializeError::InvalidDescriptor(
                            "copy distance out of range".into(),
                        ));
                    }
                    if pos + clen > output.len() {
                        return Err(MaterializeError::InvalidDescriptor("copy overflow".into()));
                    }
                    for _ in 0..clen {
                        output[pos] = output[pos - dist];
                        pos += 1;
                    }
                    spend(clen as u64, budget)?;
                }
            }
            if pos != output.len() || lit != literals.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "sequence command walk did not cover the output".into(),
                ));
            }
            Ok(())
        }
        // SPARSE_BLOCK64: 64-bit words with popcount-indexed sparse
        // content. The popcount stream decodes to one byte per word
        // (popcounts are ≤ 64, so one byte each — the comment below is
        // the allocation bound); the rank stream carries one C(64, k)
        // rank per nonzero word. Word coverage (`words * 8 >= len`) is
        // re-checked so a hostile word count cannot leave output bytes
        // unwritten or drive positions out of range.
        Representation::SparseBlock64 {
            model,
            enc_obj,
            scale_bits,
            codec,
            pc_len,
            rank_len,
            lit_len,
            words,
            nonzero,
            lit_out,
            len,
        } => {
            if *len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *len,
                    max: limits.max_alloc_bytes,
                });
            }
            // Word coverage: words*8 >= len (validated structurally, but
            // re-checked here — materialize never trusts the descriptor).
            let word_count = *words as usize;
            if word_count.saturating_mul(8) < *len as usize {
                return Err(MaterializeError::InvalidDescriptor(
                    "sparse-block64 word count does not cover the output".into(),
                ));
            }
            // Popcount stream decodes to one byte per word; bound the
            // allocation (popcounts <= 64 fit one byte each).
            let d = crate::rans::sequence::decode_three_streams(
                ctx,
                limits,
                crate::rans::sequence::StreamRefs {
                    model: *model,
                    enc_obj: *enc_obj,
                    scale_bits: *scale_bits,
                    codec: *codec,
                },
                crate::rans::sequence::ThreeStreams {
                    seq_len: *pc_len,
                    lit_len: *lit_len,
                    off_len: *rank_len,
                    cmds: *words,
                    lit_out: *lit_out,
                },
                Some(*nonzero),
                8,
            )
            .map_err(|e| MaterializeError::Sequence(e.to_string()))?;
            let popcounts = d.commands;
            let literals = d.literals;
            let ranks = d.offsets;
            if popcounts.len() != word_count {
                return Err(MaterializeError::InvalidDescriptor(
                    "sparse-block64 popcount count mismatch".into(),
                ));
            }
            output.fill(0);
            let mut lit = 0usize;
            let mut rank = 0usize;
            for (w, &k) in popcounts.iter().enumerate() {
                let k = k as usize;
                if k == 0 {
                    continue;
                }
                if k > 64 || rank + 8 > ranks.len() || lit + k > literals.len() {
                    return Err(MaterializeError::InvalidDescriptor(
                        "sparse-block64 stream inconsistency".into(),
                    ));
                }
                let r = u64::from_le_bytes(
                    ranks[rank..rank + 8].try_into().expect("8-byte rank slice"),
                );
                rank += 8;
                // Unrank the C(64, k) subset of bit positions within the
                // word; each position maps to an output byte.
                let positions = crate::entropy::rank::unrank_comb_subset(r as u128, 64, k as u64)
                    .map_err(|e| MaterializeError::InvalidDescriptor(e.to_string()))?;
                if positions.len() != k {
                    return Err(MaterializeError::InvalidDescriptor(
                        "sparse-block64 rank length mismatch".into(),
                    ));
                }
                let base = w * 8;
                for (j, &p) in positions.iter().enumerate() {
                    let out_pos = base + p as usize;
                    if out_pos >= output.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "sparse-block64 position out of bounds".into(),
                        ));
                    }
                    output[out_pos] = literals[lit + j];
                }
                lit += k;
                spend(k as u64 + 1, budget)?;
            }
            if lit != literals.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "sparse-block64 literal count mismatch".into(),
                ));
            }
            Ok(())
        }
        // SEQUENCE_DICT (E2, Phase-9B): the cross-chunk dictionary family.
        // The dictionary is ANOTHER CHUNK referenced by id; materializing
        // it at depth+1 is what makes dictionary references bounded random
        // access — the depth cap stops dictionary chains from defeating
        // the guarantee (the Phase-9B constraint this arm enforces). The
        // command walk is the same copy/literal recipe as SEQUENCE_RANS
        // with two copy sources: LOCAL (byte-progressive backward
        // references into the output) and DICT (absolute offsets into the
        // materialized dictionary).
        Representation::SequenceDict {
            dictionary,
            dictionary_len,
            model,
            enc_obj,
            scale_bits,
            codec,
            seq_len,
            lit_len,
            off_len,
            src_len,
            cmds,
            lit_out,
            len,
        } => {
            if *len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *len,
                    max: limits.max_alloc_bytes,
                });
            }
            // Every command writes at least one byte, so the command count
            // can never exceed the output length (bounds the decode_rans
            // allocation below).
            if (*cmds as u64) > *len {
                return Err(MaterializeError::InvalidDescriptor(
                    "sequence command count exceeds output length".into(),
                ));
            }
            if *dictionary_len as u64 > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *dictionary_len as u64,
                    max: limits.max_alloc_bytes,
                });
            }
            // Resolve and materialize the dictionary chunk at depth+1: the
            // depth cap bounds dictionary chains, so cross-chunk references
            // can never defeat bounded random access (Phase-9B constraint).
            let dict_desc = ctx.fetch_descriptor(dictionary)?;
            if dict_desc.len() != *dictionary_len as u64 {
                return Err(MaterializeError::InvalidDescriptor(
                    "dictionary length mismatch".into(),
                ));
            }
            let mut dict_bytes = vec![0u8; *dictionary_len as usize];
            materialize(&dict_desc, ctx, limits, depth + 1, budget, &mut dict_bytes)?;
            let d = crate::rans::sequence::decode_four_streams(
                ctx,
                limits,
                crate::rans::sequence::StreamRefs {
                    model: *model,
                    enc_obj: *enc_obj,
                    scale_bits: *scale_bits,
                    codec: *codec,
                },
                crate::rans::sequence::FourStreams {
                    seq_len: *seq_len,
                    lit_len: *lit_len,
                    off_len: *off_len,
                    src_len: *src_len,
                    cmds: *cmds,
                    lit_out: *lit_out,
                },
            )?;
            let (commands, literals, offsets, sources) =
                (d.commands, d.literals, d.offsets, d.sources);
            // Walk the commands: LITERAL appends verbatim; COPY consumes
            // one source byte and one u16 value — LOCAL copies are
            // byte-progressive backward references into the output, DICT
            // copies are absolute offsets into the materialized dictionary.
            let mut pos = 0usize;
            let mut lit = 0usize;
            let mut off = 0usize;
            let mut src = 0usize;
            for &cmd in &commands {
                if cmd < 0x80 {
                    let run = cmd as usize + 1;
                    if pos + run > output.len() || lit + run > literals.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "literal run overflow".into(),
                        ));
                    }
                    output[pos..pos + run].copy_from_slice(&literals[lit..lit + run]);
                    pos += run;
                    lit += run;
                    spend(run as u64, budget)?;
                } else {
                    let clen = cmd as usize - 0x80 + 4;
                    if src + 1 > sources.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "copy source exhausted".into(),
                        ));
                    }
                    let source = sources[src];
                    src += 1;
                    if off + 2 > offsets.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "copy offset exhausted".into(),
                        ));
                    }
                    let v = u16::from_le_bytes([offsets[off], offsets[off + 1]]) as usize;
                    off += 2;
                    match source {
                        crate::rans::sequence::SRC_LOCAL => {
                            if v == 0 || v > pos {
                                return Err(MaterializeError::InvalidDescriptor(
                                    "copy distance out of range".into(),
                                ));
                            }
                            if pos + clen > output.len() {
                                return Err(MaterializeError::InvalidDescriptor(
                                    "copy overflow".into(),
                                ));
                            }
                            for _ in 0..clen {
                                output[pos] = output[pos - v];
                                pos += 1;
                            }
                        }
                        crate::rans::sequence::SRC_DICT => {
                            if v.checked_add(clen).is_none() || v + clen > dict_bytes.len() {
                                return Err(MaterializeError::InvalidDescriptor(
                                    "dict copy out of dictionary bounds".into(),
                                ));
                            }
                            if pos + clen > output.len() {
                                return Err(MaterializeError::InvalidDescriptor(
                                    "copy overflow".into(),
                                ));
                            }
                            output[pos..pos + clen].copy_from_slice(&dict_bytes[v..v + clen]);
                            pos += clen;
                        }
                        other => {
                            return Err(MaterializeError::InvalidDescriptor(format!(
                                "unknown copy source {other}"
                            )));
                        }
                    }
                    spend(clen as u64, budget)?;
                }
            }
            if pos != output.len() || lit != literals.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "sequence command walk did not cover the output".into(),
                ));
            }
            Ok(())
        }
        // SEQUENCE_SHARED_DICT (E3, Phase-9C): the shared amortized
        // dictionary family — TWO dictionary sources (the file dictionary,
        // optional, and the shared cross-file dictionary), both resolved
        // at depth+1 so neither can defeat bounded random access. A ZERO
        // file-dictionary id means absent (the single-dictionary variant);
        // the shared dictionary is always present. This is the family the
        // hostile-media court's "shared-dict double branches" exhibit
        // targets (two references that may converge on a common chunk).
        Representation::SequenceSharedDict {
            dictionary,
            dictionary_len,
            shared,
            shared_len,
            model,
            enc_obj,
            scale_bits,
            codec,
            seq_len,
            lit_len,
            off_len,
            src_len,
            cmds,
            lit_out,
            len,
        } => {
            if *len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *len,
                    max: limits.max_alloc_bytes,
                });
            }
            // Every command writes at least one byte, so the command count
            // can never exceed the output length (bounds the decode_rans
            // allocation below).
            if (*cmds as u64) > *len {
                return Err(MaterializeError::InvalidDescriptor(
                    "sequence command count exceeds output length".into(),
                ));
            }
            if *shared_len as u64 > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *shared_len as u64,
                    max: limits.max_alloc_bytes,
                });
            }
            // Resolve and materialize the shared dictionary chunk at
            // depth+1 (bounded random access, Phase-9C constraint).
            let shared_desc = ctx.fetch_descriptor(shared)?;
            if shared_desc.len() != *shared_len as u64 {
                return Err(MaterializeError::InvalidDescriptor(
                    "shared dictionary length mismatch".into(),
                ));
            }
            let mut shared_bytes = vec![0u8; *shared_len as usize];
            materialize(
                &shared_desc,
                ctx,
                limits,
                depth + 1,
                budget,
                &mut shared_bytes,
            )?;
            // Optional previous same-file dictionary (ZERO id = absent).
            let mut dict_bytes: Vec<u8> = Vec::new();
            if !dictionary.is_zero() {
                if *dictionary_len as u64 > limits.max_alloc_bytes {
                    return Err(MaterializeError::AllocTooLarge {
                        requested: *dictionary_len as u64,
                        max: limits.max_alloc_bytes,
                    });
                }
                let dict_desc = ctx.fetch_descriptor(dictionary)?;
                if dict_desc.len() != *dictionary_len as u64 {
                    return Err(MaterializeError::InvalidDescriptor(
                        "file dictionary length mismatch".into(),
                    ));
                }
                dict_bytes = vec![0u8; *dictionary_len as usize];
                materialize(&dict_desc, ctx, limits, depth + 1, budget, &mut dict_bytes)?;
            }
            let d = crate::rans::sequence::decode_four_streams(
                ctx,
                limits,
                crate::rans::sequence::StreamRefs {
                    model: *model,
                    enc_obj: *enc_obj,
                    scale_bits: *scale_bits,
                    codec: *codec,
                },
                crate::rans::sequence::FourStreams {
                    seq_len: *seq_len,
                    lit_len: *lit_len,
                    off_len: *off_len,
                    src_len: *src_len,
                    cmds: *cmds,
                    lit_out: *lit_out,
                },
            )?;
            let (commands, literals, offsets, sources) =
                (d.commands, d.literals, d.offsets, d.sources);
            // Walk the commands: LITERAL appends verbatim; COPY consumes
            // one source byte and one u16 value — LOCAL copies are
            // byte-progressive backward references into the output, DICT
            // copies are absolute offsets into the file dictionary, SHARED
            // copies are absolute offsets into the shared dictionary.
            let mut pos = 0usize;
            let mut lit = 0usize;
            let mut off = 0usize;
            let mut src = 0usize;
            for &cmd in &commands {
                if cmd < 0x80 {
                    let run = cmd as usize + 1;
                    if pos + run > output.len() || lit + run > literals.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "literal run overflow".into(),
                        ));
                    }
                    output[pos..pos + run].copy_from_slice(&literals[lit..lit + run]);
                    pos += run;
                    lit += run;
                    spend(run as u64, budget)?;
                } else {
                    let clen = cmd as usize - 0x80 + 4;
                    if src + 1 > sources.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "copy source exhausted".into(),
                        ));
                    }
                    let source = sources[src];
                    src += 1;
                    if off + 2 > offsets.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "copy offset exhausted".into(),
                        ));
                    }
                    let v = u16::from_le_bytes([offsets[off], offsets[off + 1]]) as usize;
                    off += 2;
                    match source {
                        crate::rans::sequence::SRC_LOCAL => {
                            if v == 0 || v > pos {
                                return Err(MaterializeError::InvalidDescriptor(
                                    "copy distance out of range".into(),
                                ));
                            }
                            if pos + clen > output.len() {
                                return Err(MaterializeError::InvalidDescriptor(
                                    "copy overflow".into(),
                                ));
                            }
                            for _ in 0..clen {
                                output[pos] = output[pos - v];
                                pos += 1;
                            }
                        }
                        crate::rans::sequence::SRC_DICT => {
                            if dict_bytes.is_empty()
                                || v.checked_add(clen).is_none()
                                || v + clen > dict_bytes.len()
                            {
                                return Err(MaterializeError::InvalidDescriptor(
                                    "file dict copy out of bounds".into(),
                                ));
                            }
                            if pos + clen > output.len() {
                                return Err(MaterializeError::InvalidDescriptor(
                                    "copy overflow".into(),
                                ));
                            }
                            output[pos..pos + clen].copy_from_slice(&dict_bytes[v..v + clen]);
                            pos += clen;
                        }
                        crate::rans::sequence::SRC_SHARED => {
                            if v.checked_add(clen).is_none() || v + clen > shared_bytes.len() {
                                return Err(MaterializeError::InvalidDescriptor(
                                    "shared dict copy out of bounds".into(),
                                ));
                            }
                            if pos + clen > output.len() {
                                return Err(MaterializeError::InvalidDescriptor(
                                    "copy overflow".into(),
                                ));
                            }
                            output[pos..pos + clen].copy_from_slice(&shared_bytes[v..v + clen]);
                            pos += clen;
                        }
                        other => {
                            return Err(MaterializeError::InvalidDescriptor(format!(
                                "unknown copy source {other}"
                            )));
                        }
                    }
                    spend(clen as u64, budget)?;
                }
            }
            if pos != output.len() || lit != literals.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "sequence command walk did not cover the output".into(),
                ));
            }
            Ok(())
        }
        // SEQUENCE_DEEP (E4, Phase-9E): the deep-match family — repcodes
        // (REP0/REP1 remember the last one/two distances) and extended
        // length codes (XCOPY/XLIT consume a u16 length extra). The
        // command walk maintains the rep registers and re-checks every
        // distance, length, and stream cursor; reserved command bytes are
        // rejected. Background-only in the search (the foreground keeps
        // the fast greedy matcher); here it is just another bounded
        // program.
        Representation::SequenceDeep {
            model,
            enc_obj,
            scale_bits,
            codec,
            seq_len,
            lit_len,
            off_len,
            len_len,
            cmds,
            lit_out,
            len,
        } => {
            if *len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *len,
                    max: limits.max_alloc_bytes,
                });
            }
            // Every command writes at least one byte, so the command count
            // can never exceed the output length.
            if (*cmds as u64) > *len {
                return Err(MaterializeError::InvalidDescriptor(
                    "sequence command count exceeds output length".into(),
                ));
            }
            let d = crate::rans::sequence::decode_deep_streams(
                ctx,
                limits,
                crate::rans::sequence::StreamRefs {
                    model: *model,
                    enc_obj: *enc_obj,
                    scale_bits: *scale_bits,
                    codec: *codec,
                },
                crate::rans::sequence::DeepLens {
                    seq_len: *seq_len,
                    lit_len: *lit_len,
                    off_len: *off_len,
                    len_len: *len_len,
                    cmds: *cmds,
                    lit_out: *lit_out,
                },
            )?;
            let (commands, literals, offsets, lengths) =
                (d.commands, d.literals, d.offsets, d.lengths);
            // Walk the commands: LIT appends verbatim; COPY/XCOPY consume a
            // NEW u16 distance (byte-progressive copy) and update the rep
            // register; REP0/REP1 copy at the remembered distance without
            // consuming an offset; XCOPY/XLIT consume a u16 length extra.
            let mut pos = 0usize;
            let mut lit = 0usize;
            let mut off = 0usize;
            let mut lenp = 0usize;
            let mut rep0 = 0usize;
            let mut rep1 = 0usize;
            for &cmd in &commands {
                if cmd <= crate::rans::sequence::DEEP_LIT_MAX {
                    let run = cmd as usize + 1;
                    if pos + run > output.len() || lit + run > literals.len() {
                        return Err(MaterializeError::InvalidDescriptor(
                            "literal run overflow".into(),
                        ));
                    }
                    output[pos..pos + run].copy_from_slice(&literals[lit..lit + run]);
                    pos += run;
                    lit += run;
                    spend(run as u64, budget)?;
                    continue;
                }
                // A copy: resolve (distance, clen) from the command byte.
                let (clen, distance): (usize, usize) =
                    if cmd <= crate::rans::sequence::DEEP_COPY_MAX {
                        if off + 2 > offsets.len() {
                            return Err(MaterializeError::InvalidDescriptor(
                                "copy offset exhausted".into(),
                            ));
                        }
                        let d = u16::from_le_bytes([offsets[off], offsets[off + 1]]) as usize;
                        off += 2;
                        (4 + (cmd - crate::rans::sequence::DEEP_COPY_MIN) as usize, d)
                    } else if cmd <= crate::rans::sequence::DEEP_REP0_MAX {
                        (
                            4 + (cmd - crate::rans::sequence::DEEP_REP0_MIN) as usize,
                            rep0,
                        )
                    } else if cmd <= crate::rans::sequence::DEEP_REP1_MAX {
                        (
                            4 + (cmd - crate::rans::sequence::DEEP_REP1_MIN) as usize,
                            rep1,
                        )
                    } else if cmd == crate::rans::sequence::DEEP_XCOPY {
                        if lenp + 2 > lengths.len() {
                            return Err(MaterializeError::InvalidDescriptor(
                                "extended length exhausted".into(),
                            ));
                        }
                        let extra = u16::from_le_bytes([lengths[lenp], lengths[lenp + 1]]) as usize;
                        lenp += 2;
                        if off + 2 > offsets.len() {
                            return Err(MaterializeError::InvalidDescriptor(
                                "copy offset exhausted".into(),
                            ));
                        }
                        let d = u16::from_le_bytes([offsets[off], offsets[off + 1]]) as usize;
                        off += 2;
                        (68 + extra, d)
                    } else if cmd == crate::rans::sequence::DEEP_XLIT {
                        if lenp + 2 > lengths.len() {
                            return Err(MaterializeError::InvalidDescriptor(
                                "extended length exhausted".into(),
                            ));
                        }
                        let extra = u16::from_le_bytes([lengths[lenp], lengths[lenp + 1]]) as usize;
                        lenp += 2;
                        let run = 129 + extra;
                        if pos + run > output.len() || lit + run > literals.len() {
                            return Err(MaterializeError::InvalidDescriptor(
                                "extended literal run overflow".into(),
                            ));
                        }
                        output[pos..pos + run].copy_from_slice(&literals[lit..lit + run]);
                        pos += run;
                        lit += run;
                        spend(run as u64, budget)?;
                        continue;
                    } else {
                        return Err(MaterializeError::InvalidDescriptor(format!(
                            "reserved deep command byte 0x{cmd:02x}"
                        )));
                    };
                if distance == 0 || distance > pos {
                    return Err(MaterializeError::InvalidDescriptor(
                        "copy distance out of range".into(),
                    ));
                }
                if pos + clen > output.len() {
                    return Err(MaterializeError::InvalidDescriptor("copy overflow".into()));
                }
                for _ in 0..clen {
                    output[pos] = output[pos - distance];
                    pos += 1;
                }
                // NEW distances (COPY/XCOPY) update the rep register.
                if cmd <= crate::rans::sequence::DEEP_COPY_MAX
                    || cmd == crate::rans::sequence::DEEP_XCOPY
                {
                    rep1 = rep0;
                    rep0 = distance;
                }
                spend(clen as u64, budget)?;
            }
            if pos != output.len() || lit != literals.len() {
                return Err(MaterializeError::InvalidDescriptor(
                    "deep command walk did not cover the output".into(),
                ));
            }
            Ok(())
        }
    }
}

/// Apply a residual over `base` into `out`.
///
/// # What
///
/// Compute the residual application `out = apply(R, base)` for one of the
/// four residual kinds: XOR-sparse edits, range replacements, a
/// rANS-coded XOR stream, or a base-sequence copy/literal recipe.
///
/// # Why
///
/// BASE_RESIDUAL and ENTROPY_REF both end here: the residual is the
/// delta that turns a cheaply-stored base into the exact target bytes.
/// The residual bytes are persisted and therefore untrusted — every arm
/// re-checks lengths, edit positions, stream cursors, and copy bounds
/// before touching memory, and every byte of work spends the budget.
///
/// # Inputs and authority
///
/// - `residual`: the persisted delta program (untrusted; validated
///   structurally by `Residual::validate` against the representation
///   length, re-checked here at the runtime level).
/// - `base`: the materialized base bytes (already produced by the caller
///   at `depth + 1`). Positional residuals (XorSparse, RangeReplace,
///   RansCoded) require `base.len() >= len`; BaseSequence may reference a
///   base shorter OR longer than the target (insertions/deletions — it
///   builds the output from scratch).
/// - `out`: the output buffer; must be exactly `residual.len()` bytes.
/// - `ctx` / `limits` / `budget`: as in [`materialize`].
///
/// # Invariants
///
/// Pre: `out.len() == residual.len()`; positional residuals additionally
/// require `base.len() >= residual.len()` (both enforced here). Post: on
/// `Ok`, `out` holds `apply(R, base)` exactly.
///
/// # Resource bounds
///
/// Every allocation (the RansCoded decoded stream) is bounded against
/// `max_alloc_bytes` before it happens; every edit/copy spends the
/// budget; every stream is walked with explicit cursor checks.
///
/// # Failure behavior
///
/// Typed `MaterializeError::Residual` (or `AllocTooLarge` / `RansDecode`)
/// only; never panics.
///
/// # Evidence / rationale
///
/// The hostile-media graph court's residual exhibits (edit out of range,
/// exhausted literal/offset streams, copies at the exact end and one byte
/// beyond, corrupted models) pin every arm's bounds; the BASE_RESIDUAL
/// family and the residual algebra are Phase-8 §5.
pub fn apply_residual(
    residual: &Residual,
    base: &[u8],
    out: &mut [u8],
    ctx: &dyn DecoderContext,
    limits: &Limits,
    budget: &mut u64,
) -> Result<(), MaterializeError> {
    let len = residual.len();
    if out.len() as u64 != len {
        return Err(MaterializeError::Residual(
            "residual output length mismatch".into(),
        ));
    }
    // Positional residuals overlay the base and need base >= len; the
    // BaseSequence copy/literal delta validates its own copy bounds.
    if !matches!(residual, Residual::BaseSequence { .. }) && (base.len() as u64) < len {
        return Err(MaterializeError::Residual(
            "base shorter than residual".into(),
        ));
    }
    match residual {
        // XOR-SPARSE: `out = base` with a sparse set of XOR edits. Each
        // edit position is bounds-checked against the residual length
        // (a hostile edit cannot write past the output) and each edit
        // spends the budget.
        Residual::XorSparse { edits, .. } => {
            out[..len as usize].copy_from_slice(&base[..len as usize]);
            for e in edits {
                if (e.pos as u64) >= len {
                    return Err(MaterializeError::Residual("edit out of range".into()));
                }
                out[e.pos as usize] ^= e.val;
                spend(1, budget)?;
            }
            Ok(())
        }
        // RANGE-REPLACE: `out = base` with contiguous literal slices
        // written over `[start, end)` in order. Every range is
        // bounds-checked (start < end <= len), the literal pool is
        // consumed with an explicit cursor (exhaustion is a typed error),
        // and each replaced byte spends the budget.
        Residual::RangeReplace {
            changes, literals, ..
        } => {
            out[..len as usize].copy_from_slice(&base[..len as usize]);
            let mut lit = 0usize;
            for c in changes {
                let start = c.start as usize;
                let end = c.end as usize;
                if end > len as usize || start >= end {
                    return Err(MaterializeError::Residual("range out of bounds".into()));
                }
                let take = end - start;
                if lit + take > literals.len() {
                    return Err(MaterializeError::Residual("literal exhaustion".into()));
                }
                out[start..end].copy_from_slice(&literals[lit..lit + take]);
                lit += take;
                spend(take as u64, budget)?;
            }
            Ok(())
        }
        // RANS-CODED: `out = base ⊕ D` where D is the decoded residual
        // stream — the diff itself is entropy-coded. The decoded length
        // must equal the residual length and be within `max_alloc_bytes`
        // BEFORE the allocation; the model object is size-checked too.
        Residual::RansCoded {
            enc_obj,
            model,
            scale_bits,
            codec,
            decoded_len,
            ..
        } => {
            if *decoded_len != len {
                return Err(MaterializeError::Residual(
                    "residual decoded length mismatch".into(),
                ));
            }
            if *decoded_len > limits.max_alloc_bytes {
                return Err(MaterializeError::AllocTooLarge {
                    requested: *decoded_len,
                    max: limits.max_alloc_bytes,
                });
            }
            let model_bytes = ctx.fetch_object(model)?;
            if model_bytes.len() as u64 > limits.max_model_bytes {
                return Err(MaterializeError::InvalidDescriptor(
                    "residual model object too large".into(),
                ));
            }
            let encoded = ctx.fetch_object(enc_obj)?;
            let decoded = ctx.decode_rans(&model_bytes, &encoded, *scale_bits, *codec, len)?;
            if decoded.len() as u64 != len {
                return Err(MaterializeError::Residual(
                    "residual rans length mismatch".into(),
                ));
            }
            for i in 0..len as usize {
                out[i] = base[i] ^ decoded[i];
            }
            Ok(())
        }
        // BASE_SEQUENCE: the shift-aware copy/literal delta (Phase-8 §5).
        // Unlike the positional residuals this is NOT a diff over the base:
        // the residual IS the full output recipe — literal runs append
        // verbatim, and copy commands read `clen` bytes from an absolute
        // u32 LE offset INTO THE BASE (so insertions/deletions let the
        // output be longer or shorter than the base). The walk re-checks
        // every literal run, base copy bounds, and output bounds, and
        // spends per byte.
        Residual::BaseSequence {
            len,
            enc_obj,
            model,
            scale_bits,
            codec,
            seq_len,
            lit_len,
            off_len,
            cmds,
            lit_out,
            ..
        } => {
            if *len != out.len() as u64 {
                return Err(MaterializeError::Residual(
                    "base-sequence residual output length mismatch".into(),
                ));
            }
            // Every command writes at least one byte, so the command count
            // cannot exceed the output length (bounds the decode_rans
            // allocation).
            if (*cmds as u64) > *len {
                return Err(MaterializeError::Residual(
                    "base-sequence command count exceeds output length".into(),
                ));
            }
            let d = crate::rans::sequence::decode_three_streams(
                ctx,
                limits,
                crate::rans::sequence::StreamRefs {
                    model: *model,
                    enc_obj: *enc_obj,
                    scale_bits: *scale_bits,
                    codec: *codec,
                },
                crate::rans::sequence::ThreeStreams {
                    seq_len: *seq_len,
                    lit_len: *lit_len,
                    off_len: *off_len,
                    cmds: *cmds,
                    lit_out: *lit_out,
                },
                None,
                4,
            )
            .map_err(|e| MaterializeError::Residual(e.to_string()))?;
            let (commands, literals, offsets) = (d.commands, d.literals, d.offsets);
            // Walk the commands: LITERAL appends; COPY reads the base at a
            // u32 LE base offset (the output is built from scratch — the
            // residual is the full copy/literal recipe, not a diff).
            let mut pos = 0usize;
            let mut lit = 0usize;
            let mut off = 0usize;
            for &cmd in &commands {
                if cmd < 0x80 {
                    let run = cmd as usize + 1;
                    if pos + run > out.len() || lit + run > literals.len() {
                        return Err(MaterializeError::Residual(
                            "base-sequence literal run overflow".into(),
                        ));
                    }
                    out[pos..pos + run].copy_from_slice(&literals[lit..lit + run]);
                    pos += run;
                    lit += run;
                    spend(run as u64, budget)?;
                } else {
                    let clen = cmd as usize - 0x80 + 4;
                    if off + 4 > offsets.len() {
                        return Err(MaterializeError::Residual(
                            "base-sequence copy offset exhausted".into(),
                        ));
                    }
                    let boff = u32::from_le_bytes(
                        offsets[off..off + 4]
                            .try_into()
                            .expect("4-byte offset slice"),
                    ) as usize;
                    off += 4;
                    if boff.checked_add(clen).is_none() || boff + clen > base.len() {
                        return Err(MaterializeError::Residual(
                            "base-sequence copy out of base bounds".into(),
                        ));
                    }
                    if pos + clen > out.len() {
                        return Err(MaterializeError::Residual(
                            "base-sequence copy overflow".into(),
                        ));
                    }
                    out[pos..pos + clen].copy_from_slice(&base[boff..boff + clen]);
                    pos += clen;
                    spend(clen as u64, budget)?;
                }
            }
            if pos != out.len() {
                return Err(MaterializeError::Residual(
                    "base-sequence command walk did not cover the output".into(),
                ));
            }
            Ok(())
        }
    }
}

/// Decrement the work budget by `n`; error when exhausted.
///
/// # What
///
/// The operation-budget primitive: every materialize step (byte written,
/// edit applied, period repeated, command walked) charges `n` units; when
/// the counter has fewer than `n` remaining, materialization aborts with
/// `BudgetExceeded`.
///
/// # Why
///
/// The budget is what makes the bounded-materialization guarantee
/// quantitative: without it, a hostile command stream could burn
/// unbounded CPU while remaining within every length/allocation bound
/// (e.g. a `SequenceDeep` stream of near-zero-cost commands over a huge
/// declared length, or repeated `Periodic` fills). `max_decode_work`
/// (default 64 Mi operations) is the total work any single
/// materialization may do.
///
/// # Units
///
/// `n` is in DECODE OPERATIONS (not bytes and not wall time): bulk
/// families charge `len / 8 + 1` once up front; per-byte work charges 1
/// per byte; per-period repeats charge `period / 8 + 1` each. The unit is
/// deliberately coarse — it bounds CPU, it does not meter it precisely.
///
/// # Invariants
///
/// The budget is monotone non-increasing across a materialization and is
/// never incremented (the caller starts it at `max_decode_work`).
///
/// # Failure behavior
///
/// `BudgetExceeded` on exhaustion — a typed rejection, never a panic.
/// The hostile-media graph court's budget exhibits (a valid descriptor
/// under a tiny budget) pin this behavior.
fn spend(n: u64, budget: &mut u64) -> Result<(), MaterializeError> {
    if *budget < n {
        return Err(MaterializeError::BudgetExceeded);
    }
    *budget -= n;
    Ok(())
}

/// Convenience: materialize into a fresh `Vec<u8>`.
///
/// # What
///
/// Allocate an exact `desc.len()` buffer (bounded by `max_alloc_bytes`
/// before the allocation) and run [`materialize`] with a fresh
/// `max_decode_work` budget at depth 0.
///
/// # Why
///
/// The common read-path entry point: the caller wants the bytes, not a
/// reusable buffer. Every caller that only needs the full chunk
/// (`optimizer::search`'s dedup/validation, `store`'s read and fsck
/// paths, the hostile-media courts) goes through here.
///
/// # Resource bounds
///
/// The declared length is checked against `max_alloc_bytes` before the
/// `vec!`; the work budget starts at the full `max_decode_work`. A
/// descriptor over the allocation cap is rejected typed before any work.
pub fn materialize_to_vec(
    desc: &Representation,
    ctx: &dyn DecoderContext,
    limits: &Limits,
) -> Result<Vec<u8>, MaterializeError> {
    let len = desc.len();
    if len > limits.max_alloc_bytes {
        return Err(MaterializeError::AllocTooLarge {
            requested: len,
            max: limits.max_alloc_bytes,
        });
    }
    let mut out = vec![0u8; len as usize];
    let mut budget = limits.max_decode_work;
    materialize(desc, ctx, limits, 0, &mut budget, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    // Unit-level materialization tests over an in-memory context.
    // `MemCtx` deliberately leaves `decode_rans` UNWIRED (a typed error):
    // these tests pin the pure algebra — terminal families, reference
    // sub-ranges, residual application, depth caps, work budgets, and a
    // sparse round trip through the REAL `rank_comb_subset` engine (the
    // encoder/decoder pairing). The adversarial side of the contract —
    // bounded-valid-or-typed-rejection over fuzz-defined graphs — lives
    // in `src/tests/hostile_media/graph_court.rs`, which exercises the
    // same `materialize` entry point through `HostileResolver`.
    use super::*;
    use crate::core::representation::{Edit, Residual};
    use std::collections::HashMap;

    /// Minimal in-memory decoder context for tests.
    ///
    /// `fetch_object` / `fetch_descriptor` resolve from the tables the
    /// test fills; `decode_rans` is deliberately a typed error (stream
    /// decode is the rANS module's own contract and the hostile-media
    /// court's territory); `universe_bytes` returns a fixed 0xAB fill so
    /// ENTROPY_REF algebra is testable without the XOF.
    struct MemCtx {
        objects: HashMap<ChunkId, Vec<u8>>,
        chunks: HashMap<ChunkId, Representation>,
    }

    impl DecoderContext for MemCtx {
        fn fetch_object(&self, id: &ChunkId) -> Result<Vec<u8>, MaterializeError> {
            self.objects
                .get(id)
                .cloned()
                .ok_or(MaterializeError::MissingObject(*id))
        }
        fn fetch_descriptor(&self, id: &ChunkId) -> Result<Representation, MaterializeError> {
            self.chunks
                .get(id)
                .cloned()
                .ok_or(MaterializeError::MissingChunk(*id))
        }
        fn decode_rans(
            &self,
            _model: &[u8],
            _encoded: &[u8],
            _scale_bits: u8,
            _codec: RansCodec,
            _out_len: u64,
        ) -> Result<Vec<u8>, MaterializeError> {
            Err(MaterializeError::RansDecode(
                "not wired in unit test".into(),
            ))
        }
        fn universe_bytes(
            &self,
            _universe: UniverseId,
            _seed: [u8; 16],
            _coordinate: u64,
            range: Range<u64>,
        ) -> Result<Vec<u8>, MaterializeError> {
            Ok(vec![0xAB; (range.end - range.start) as usize])
        }
    }

    fn limits() -> Limits {
        Limits::default()
    }

    #[test]
    fn zero_and_fill() {
        let ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        let z = Representation::Zero { len: 1024 };
        assert_eq!(
            materialize_to_vec(&z, &ctx, &limits()).unwrap(),
            vec![0u8; 1024]
        );
        let f = Representation::Fill { value: 7, len: 512 };
        assert_eq!(
            materialize_to_vec(&f, &ctx, &limits()).unwrap(),
            vec![7u8; 512]
        );
    }

    #[test]
    fn inline_and_raw() {
        let data = b"hello entropy".to_vec();
        let id = ChunkId::of(&data);
        let mut ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        ctx.objects.insert(id, data.clone());
        let raw = Representation::Raw {
            obj: id,
            len: data.len() as u64,
        };
        assert_eq!(materialize_to_vec(&raw, &ctx, &limits()).unwrap(), data);

        let inl = Representation::Inline {
            data: b"abc".to_vec(),
        };
        assert_eq!(materialize_to_vec(&inl, &ctx, &limits()).unwrap(), b"abc");
    }

    #[test]
    fn exact_ref_subrange() {
        // target chunk: 256 bytes of 0x11
        let target = Representation::Fill {
            value: 0x11,
            len: 256,
        };
        let tid = ChunkId::of(&vec![0x11u8; 256]);
        let mut ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        ctx.chunks.insert(tid, target);
        let r = Representation::ExactRef {
            target: tid,
            off: 100,
            len: 10,
        };
        assert_eq!(
            materialize_to_vec(&r, &ctx, &limits()).unwrap(),
            vec![0x11u8; 10]
        );
    }

    #[test]
    fn base_residual_xor() {
        let base_bytes = vec![0u8; 64];
        let base_id = ChunkId::of(&base_bytes);
        let mut ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        ctx.chunks.insert(base_id, Representation::Zero { len: 64 });
        // X[i] = B[i] ^ val: bytes 0 and 63 differ
        let r = Representation::BaseResidual {
            base: base_id,
            base_len: 64,
            residual: Residual::XorSparse {
                len: 64,
                edits: vec![Edit { pos: 0, val: 0xFF }, Edit { pos: 63, val: 0x01 }],
            },
            len: 64,
        };
        let out = materialize_to_vec(&r, &ctx, &limits()).unwrap();
        assert_eq!(out[0], 0xFF);
        assert_eq!(out[63], 0x01);
        assert_eq!(out[1], 0x00);
    }

    #[test]
    fn range_replace_residual() {
        let base_bytes = vec![1u8; 32];
        let base_id = ChunkId::of(&base_bytes);
        let mut ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        ctx.chunks
            .insert(base_id, Representation::Fill { value: 1, len: 32 });
        let r = Representation::BaseResidual {
            base: base_id,
            base_len: 32,
            residual: Residual::RangeReplace {
                len: 32,
                changes: vec![
                    crate::core::representation::RangeChange { start: 4, end: 8 },
                    crate::core::representation::RangeChange { start: 16, end: 18 },
                ],
                literals: vec![9, 9, 9, 9, 7, 7],
            },
            len: 32,
        };
        let out = materialize_to_vec(&r, &ctx, &limits()).unwrap();
        assert_eq!(&out[0..4], &[1, 1, 1, 1]);
        assert_eq!(&out[4..8], &[9, 9, 9, 9]);
        assert_eq!(&out[8..16], &[1; 8]);
        assert_eq!(&out[16..18], &[7, 7]);
        assert_eq!(&out[18..], &[1; 14]);
    }

    #[test]
    fn depth_cap_enforced() {
        // A chain of exact refs longer than the depth cap must error.
        let mut ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        // Build a 6-long chain: c0 -> c1 -> ... -> c5
        let n = 6;
        for i in (0..n).rev() {
            let id = ChunkId::of(&[i as u8; 16]);
            let desc = if i == n - 1 {
                Representation::Fill {
                    value: 0x42,
                    len: 16,
                }
            } else {
                Representation::ExactRef {
                    target: ChunkId::of(&[(i + 1) as u8; 16]),
                    off: 0,
                    len: 16,
                }
            };
            ctx.chunks.insert(id, desc);
        }
        let top = Representation::ExactRef {
            target: ChunkId::of(&[0u8; 16]),
            off: 0,
            len: 16,
        };
        let limits = Limits {
            max_reference_depth: 4,
            ..Default::default()
        };
        let res = materialize_to_vec(&top, &ctx, &limits);
        assert!(matches!(res, Err(MaterializeError::DepthExceeded { .. })));
    }

    #[test]
    fn sparse_roundtrip_via_engine() {
        // Build a sparse chunk: 64 bytes, nonzeros at 2, 17, 55.
        let mut input = vec![0u8; 64];
        input[2] = 0xAA;
        input[17] = 0xBB;
        input[55] = 0xCC;
        let positions: Vec<u32> = input
            .iter()
            .enumerate()
            .filter(|(_, b)| **b != 0)
            .map(|(i, _)| i as u32)
            .collect();
        let rank = crate::entropy::rank::rank_comb_subset(&positions, 64).unwrap();
        let literals: Vec<u8> = input.iter().copied().filter(|&b| b != 0).collect();
        let desc = Representation::Sparse {
            k: 3,
            rank,
            literals,
            len: 64,
        };
        desc.validate(&limits()).unwrap();
        let ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        assert_eq!(materialize_to_vec(&desc, &ctx, &limits()).unwrap(), input);
    }

    #[test]
    fn work_budget_exceeded() {
        let ctx = MemCtx {
            objects: HashMap::new(),
            chunks: HashMap::new(),
        };
        let z = Representation::Zero { len: 65536 };
        let mut budget = 1u64; // tiny budget
        let mut out = vec![0u8; 65536];
        let res = materialize(&z, &ctx, &limits(), 0, &mut budget, &mut out);
        assert_eq!(res, Err(MaterializeError::BudgetExceeded));
    }
}
