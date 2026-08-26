//! BaseSequence: shift-aware copy/literal delta coding (Phase-8 directive
//! §5, ADR-0005 residual kind 0x04).
//!
//! # PURPOSE
//!
//! The target chunk `X` is represented against a base `B` by a command
//! stream: `COPY(base_offset, len)` copies a base range, `LITERAL(run)`
//! appends literal bytes. Inserted/deleted regions shift positions, so
//! positional XOR residuals (XorSparse/RansCoded) degrade catastrophically
//! on such edits; copy/literal deltas do not — this is the shift-aware
//! member of the `BASE_RESIDUAL` family.
//!
//! # BOUNDARY
//!
//! - Knows: the base bytes and the target bytes, and the SEQUENCE_RANS
//!   stream codec it reuses (`encode_streams`, `hash_at`).
//! - Never knows: how the base chunk is itself represented (it is a
//!   content-addressed reference), entropy model construction, or the
//!   store layout. One candidate is proposed per base, depth-capped by
//!   the caller.
//!
//! # MODEL
//!
//! Command encoding (one byte per command):
//!
//! - `0x00..=0x7F`: literal run of `b + 1` (1..=128) bytes.
//! - `0x80..=0xFF`: copy of `b - 0x80 + 4` (4..=131) bytes from the base
//!   at a u32 LE base offset (next 4 bytes of the offset stream).
//!
//! The three streams reuse the SEQUENCE_RANS stream codec (per-stream
//! rANS with a raw fallback; three-slot model object). The offset width is
//! 4 bytes per copy (base offsets up to 2^32). The output is built from
//! scratch against the base: the streams are the full copy/literal recipe,
//! not a diff.
//!
//! # PERSISTENT AUTHORITY
//!
//! The command stream, literal stream, and offset stream are persisted
//! (rANS-coded or raw, per slot) together with the base reference; the
//! decoder reproduces `X` exactly from the base bytes. Byte-exactness is a
//! persistence invariant: the tail-remainder clip (below) changes only
//! *how* bytes are coded, never the materialized output.
//!
//! # CORRECTNESS INVARIANTS
//!
//! - Copy lengths are `4..=131`; a tail remainder of `1..=3` bytes after
//!   131-byte chunking is clipped off so it is emitted as literals — the
//!   same corruption trap as the SEQUENCE_RANS encoder (`0x7F` would
//!   decode as a 128-byte literal run; Phase 8 M3, sealed
//!   `campaign-1787671040-923df7b/`).
//! - Every copy's `base[off..off+len]` is validated against the base
//!   length at decode (materialize), so an out-of-bounds copy is a typed
//!   error, never an overrun.
//! - The base may be shorter or longer than the target (insertions and
//!   deletions) — unlike positional residuals, which require
//!   `base_len >= target_len`.
//!
//! # CONCURRENCY
//!
//! `encode_delta` is a pure deterministic function over `(target, base)`;
//! `DeltaEncoder` holds no state. No locks.
//!
//! # RESOURCE BOUNDS
//!
//! Hash-chain tables are `2^16` heads + `base.len()` chain slots; the
//! match walk is depth-capped (`CHAIN_DEPTH`); offsets are u32. All loops
//! are length-bounded by `target.len()` / `base.len()`.
//!
//! # PERFORMANCE
//!
//! Greedy longest-match parse with continuation copies at consecutive
//! base offsets: an inserted region costs only its own literal bytes, and
//! a shifted region is a single copy command, regardless of how far the
//! shift moved it (positional XOR would pay for every shifted byte).
//!
//! # FAILURE MODES
//!
//! No typed errors: degenerate inputs (empty target, base shorter than
//! `MIN_MATCH`) degrade to an all-literals delta, which is correct though
//! useless; the honest gate (descriptor + model + enc vs raw) and the
//! cost model drop candidates that do not beat RAW.
//!
//! # HISTORY / EVIDENCE
//!
//! - Phase 8 M4 — BaseSequence sealed `campaign-1787666036-43bf17e/`:
//!   H2 flips back to **+35.2%** (sequential 2.752× vs shuffled 1.784×);
//!   the shuffled control grows because deltas also capture structural
//!   similarity between unrelated-history chunks — recorded as the
//!   finding.
//! - The tail-remainder clip inherits the Phase 8 M3 SEQUENCE_RANS fix
//!   (H2 campaign, `campaign-1787671040-923df7b/`).

#![forbid(unsafe_code)]

use crate::core::candidate::{Candidate, CandidateContext, Encoder, ObjectRecord};
use crate::core::cost::ByteSplit;
use crate::core::representation::{RansCodec, Representation, Residual};
use crate::rans::sequence::{
    MAX_COPY, MAX_LIT_RUN, MIN_MATCH, SequenceStreams, encode_streams, hash_at,
};

/// Hash-chain depth cap (deterministic, bounded match search).
const CHAIN_DEPTH: usize = 16;
/// Scale bits shared by the three rANS models.
const SCALE_BITS: u8 = 14;
/// Codec shared by the three streams.
const CODEC: RansCodec = RansCodec::Interleaved2;

/// Build the copy/literal delta streams of `target` against `base`.
///
/// Greedy: at each target position, find the longest match in the base
/// (hash chains over the base's 4-byte windows), emit copies (continuation
/// at consecutive base offsets), otherwise literal runs. Deterministic and
/// bounded; the base can be shorter or longer than the target (insertions
/// and deletions shift positions — the shift-aware property that
/// positional XOR residuals lack).
///
/// The returned streams are the full copy/literal recipe: the decoder
/// builds the output from scratch against the base, so byte-exactness is a
/// direct invariant of this parse.
pub fn encode_delta(target: &[u8], base: &[u8]) -> SequenceStreams {
    let mut commands = Vec::new();
    let mut literals = Vec::new();
    let mut offsets = Vec::new();
    // ---------------------------------------------------------------------
    // Stage 1: Degenerate inputs — an all-literals delta (correct, if
    // useless; the candidate gate drops it later).
    // ---------------------------------------------------------------------
    if target.is_empty() || base.len() < MIN_MATCH {
        // All literals (a correct, if useless, delta).
        let mut t = 0usize;
        while t < target.len() {
            let run = (target.len() - t).min(MAX_LIT_RUN);
            commands.push((run - 1) as u8);
            literals.extend_from_slice(&target[t..t + run]);
            t += run;
        }
        return SequenceStreams {
            commands,
            literals,
            offsets,
        };
    }
    // ---------------------------------------------------------------------
    // Stage 2: Build the base hash-chain tables (built once; the base is
    // immutable for the parse). 2^16 heads × base.len() chain slots;
    // `find_base_match` walks each chain at most `CHAIN_DEPTH` deep.
    // ---------------------------------------------------------------------
    // Hash chains over the base.
    let hsize = 1usize << 16;
    let mut head = vec![u32::MAX; hsize];
    let mut chain = vec![u32::MAX; base.len()];
    for (p, slot) in chain.iter_mut().enumerate() {
        if p + MIN_MATCH > base.len() {
            break;
        }
        let h = hash_at(base, p);
        *slot = head[h];
        head[h] = p as u32;
    }
    // ---------------------------------------------------------------------
    // Stage 3: Greedy parse — longest base match or a literal run.
    // ---------------------------------------------------------------------
    let mut t = 0usize;
    while t < target.len() {
        if let Some((boff, len)) = find_base_match(target, t, base, &head, &chain) {
            // -----------------------------------------------------------------
            // Stage 3a: Copy emission with the SEQUENCE_RANS tail-remainder
            // clip: a tail of 1..=3 bytes after 131-byte chunking would
            // encode into the literal-command range (`0x80 + 3 - 4 = 0x7F`
            // decodes as a 128-byte literal run — the Phase 8 M3
            // corruption, sealed `campaign-1787671040-923df7b/`), so the
            // tail is clipped off and falls to the literal path below;
            // byte-exactness is preserved.
            // -----------------------------------------------------------------
            // Clip so the tail remainder after 131-byte chunking is never
            // 1..=3 (an invalid copy length; the tail falls to literals).
            let mut len = len;
            let rem = len % MAX_COPY;
            if rem > 0 && rem < MIN_MATCH {
                len -= rem;
            }
            // Continuation commands advance the base offset by `take` —
            // the decoder reads each command's u32 LE offset independently.
            let mut remaining = len;
            let mut o = boff;
            while remaining > 0 {
                let take = remaining.min(MAX_COPY);
                debug_assert!((MIN_MATCH..=MAX_COPY).contains(&take));
                commands.push((0x80 + take - MIN_MATCH) as u8);
                offsets.extend_from_slice(&(o as u32).to_le_bytes());
                remaining -= take;
                o += take;
            }
            t += len;
        } else {
            // Stage 3b: Literal run — consume positions with no match,
            // capped at 128 bytes per command.
            let start = t;
            let mut run = 0usize;
            while t < target.len() && run < MAX_LIT_RUN {
                if find_base_match(target, t, base, &head, &chain).is_some() {
                    break;
                }
                t += 1;
                run += 1;
            }
            if run > 0 {
                commands.push((run - 1) as u8);
                literals.extend_from_slice(&target[start..t]);
            }
        }
    }
    SequenceStreams {
        commands,
        literals,
        offsets,
    }
}

/// Find the longest base match at target position `t` (chain depth
/// capped). Returns `(base_offset, len)` with `len >= MIN_MATCH`.
///
/// Hash-chain search: walk the chain of base positions sharing the
/// 4-byte window hash, compare bytewise, keep the longest match, and stop
/// early when a match reaches the input end (nothing can be longer).
fn find_base_match(
    target: &[u8],
    t: usize,
    base: &[u8],
    head: &[u32],
    chain: &[u32],
) -> Option<(usize, usize)> {
    if t + MIN_MATCH > target.len() {
        return None;
    }
    let h = hash_at(target, t);
    let mut c = head[h];
    let mut best_len = 0usize;
    let mut best_off = 0usize;
    let mut depth = 0usize;
    while c != u32::MAX && depth < CHAIN_DEPTH {
        let cpos = c as usize;
        let max_len = (base.len() - cpos).min(target.len() - t);
        let mut l = 0usize;
        while l < max_len && base[cpos + l] == target[t + l] {
            l += 1;
        }
        if l >= MIN_MATCH && l > best_len {
            best_len = l;
            best_off = cpos;
            if l == max_len {
                break;
            }
        }
        c = chain[cpos];
        depth += 1;
    }
    if best_len >= MIN_MATCH {
        Some((best_off, best_len))
    } else {
        None
    }
}

/// The shift-aware delta candidate family: one candidate per base chunk.
///
/// The encoder is stateless; `ctx.bases` supplies the candidate bases and
/// the depth cap. A candidate is proposed only when the full persisted
/// cost (descriptor + model object + enc object) beats RAW.
#[derive(Debug, Default)]
pub struct DeltaEncoder;

impl Encoder for DeltaEncoder {
    fn name(&self) -> &'static str {
        "BASE_SEQUENCE"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        // -----------------------------------------------------------------
        // Stage 1: Input guard — empty/oversized chunks have no candidate.
        // -----------------------------------------------------------------
        if input.is_empty() || input.len() as u64 > ctx.limits.max_chunk_size {
            return Vec::new();
        }
        let mut out = Vec::new();
        // -----------------------------------------------------------------
        // Stage 2: One parse per base — depth-capped, non-empty bases
        // only.
        // -----------------------------------------------------------------
        for base in ctx.bases {
            if base.depth >= ctx.limits.max_reference_depth {
                continue;
            }
            if base.bytes.is_empty() {
                continue;
            }
            let streams = encode_delta(input, &base.bytes);
            if streams.commands.is_empty() {
                continue;
            }
            let enc = match encode_streams(&streams) {
                Some(e) => e,
                None => continue,
            };
            let model_obj = ObjectRecord::model(enc.model_obj);
            let enc_obj = ObjectRecord::data(enc.enc_obj);
            let residual = Residual::BaseSequence {
                len: input.len() as u64,
                enc_obj: enc_obj.id,
                model: model_obj.id,
                scale_bits: SCALE_BITS,
                codec: CODEC,
                seq_len: enc.seq_len,
                lit_len: enc.lit_len,
                off_len: enc.off_len,
                cmds: enc.cmds,
                lit_out: enc.lit_out,
            };
            let rep = Representation::BaseResidual {
                base: base.id,
                base_len: base.bytes.len() as u64,
                residual,
                len: input.len() as u64,
            };
            // -----------------------------------------------------------------
            // Stage 3: Honest gate — descriptor + persisted model object +
            // enc object must beat the raw bytes (the base chunk itself is
            // a reference, accounted where it is materialized). This is
            // the Phase-9G0 model-cost discipline applied at the candidate
            // level: a model that cannot pay for itself must not be
            // persisted (evidence-sealed campaign-1787684918-80e36c8).
            // -----------------------------------------------------------------
            // Honest gate: descriptor + model + enc must beat raw.
            let total = rep
                .encoded_size()
                .saturating_add(model_obj.payload.len() as u64)
                .saturating_add(enc_obj.payload.len() as u64);
            if total >= input.len() as u64 {
                continue;
            }
            let split = ByteSplit {
                reference: 32 + 64, // base id + model + enc ids
                ..Default::default()
            };
            let cost = crate::core::candidate::account_objects(
                crate::core::cost::estimate(&rep, &split, model_obj.payload.len() as u64),
                &[enc_obj.clone(), model_obj.clone()],
            );
            out.push(Candidate {
                representation: rep,
                objects: vec![enc_obj, model_obj],
                cost,
                content_id: ctx.content_id,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::candidate::{CandidateContext, validate_candidate};
    use crate::core::cost::Policy;
    use crate::core::limits::Limits;
    use crate::tests::helpers::MemResolver;

    fn ctx_for<'a>(
        input: &'a [u8],
        limits: &'a Limits,
        policy: &'a Policy,
        bases: &'a [crate::core::candidate::BaseChunk],
    ) -> CandidateContext<'a> {
        CandidateContext {
            limits,
            policy,
            content_id: crate::core::extent::ChunkId::of(input),
            bases,
            dedup: None,
        }
    }

    #[test]
    fn inserted_line_shift_delta_is_tiny() {
        // A base file, then the same file with a line inserted near the
        // start: positional XOR would be catastrophic; the copy/literal
        // delta must be tiny.
        let mut base = Vec::new();
        for i in 0..2000 {
            base.extend_from_slice(
                format!("line {i}: the quick brown fox jumps over the lazy dog\n").as_bytes(),
            );
        }
        let mut target = Vec::new();
        for i in 0..500 {
            target.extend_from_slice(
                format!("line {i}: the quick brown fox jumps over the lazy dog\n").as_bytes(),
            );
        }
        target.extend_from_slice(
            b"line INSERTED: a brand new line that shifts everything after it\n",
        );
        for i in 500..2000 {
            target.extend_from_slice(
                format!("line {i}: the quick brown fox jumps over the lazy dog\n").as_bytes(),
            );
        }
        let streams = encode_delta(&target, &base);
        assert!(
            streams.literals.len() < 200,
            "shifted delta literal bytes {} — expected tiny",
            streams.literals.len()
        );
        // Manual walk against the base.
        let mut lits = 0usize;
        let mut offs = 0usize;
        let mut out = Vec::with_capacity(target.len());
        for &cmd in &streams.commands {
            if cmd < 0x80 {
                let run = cmd as usize + 1;
                out.extend_from_slice(&streams.literals[lits..lits + run]);
                lits += run;
            } else {
                let clen = cmd as usize - 0x80 + 4;
                let boff = u32::from_le_bytes(streams.offsets[offs..offs + 4].try_into().unwrap())
                    as usize;
                offs += 4;
                out.extend_from_slice(&base[boff..boff + clen]);
            }
        }
        assert_eq!(out, target);
    }

    #[test]
    fn delta_candidate_roundtrips_and_wins() {
        let limits = Limits::default();
        let policy = Policy::default();
        let base: Vec<u8> = (0..65536u32).map(|i| ((i / 64) % 97) as u8).collect();
        let mut target = base.clone();
        // Insert 4 KiB of new data in the middle.
        let insert: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        target.splice(32000..32000, insert.iter().cloned());
        let bases = vec![crate::core::candidate::BaseChunk {
            id: crate::core::extent::ChunkId::of(&base),
            bytes: base,
            depth: 0,
        }];
        let cands = DeltaEncoder.encode(&target, &ctx_for(&target, &limits, &policy, &bases));
        assert_eq!(cands.len(), 1);
        let cand = &cands[0];
        // The resolver needs the base chunk descriptor (the candidate's own
        // objects plus a RAW descriptor for the base).
        let mut resolver = MemResolver::empty();
        resolver.put_object(bases[0].id, bases[0].bytes.clone());
        resolver.put_chunk(
            bases[0].id,
            crate::core::representation::Representation::Raw {
                obj: bases[0].id,
                len: bases[0].bytes.len() as u64,
            },
        );
        for o in &cand.objects {
            resolver.put_object(o.id, o.payload.clone());
        }
        validate_candidate(cand, &target, &resolver, &limits).unwrap();
        assert!(
            cand.cost.persisted_bytes() < target.len() as u64 / 8,
            "delta persisted {} for {} logical",
            cand.cost.persisted_bytes(),
            target.len()
        );
        // The inserted 4 KiB of near-random data is literal; everything
        // else is a base copy, so the delta must be far below raw.
        assert!(
            cand.cost.persisted_bytes() < 8192,
            "delta persisted {}",
            cand.cost.persisted_bytes()
        );
    }

    #[test]
    fn delta_skips_unrelated_base() {
        let limits = Limits::default();
        let policy = Policy::default();
        // Two independent SplitMix64 streams: no matches, no delta.
        let base = splitmix(65536, 0x1111_2222_3333_4444);
        let target = splitmix(65536, 0x5555_6666_7777_8888);
        let bases = vec![crate::core::candidate::BaseChunk {
            id: crate::core::extent::ChunkId::of(&base),
            bytes: base,
            depth: 0,
        }];
        let cands = DeltaEncoder.encode(&target, &ctx_for(&target, &limits, &policy, &bases));
        assert!(cands.is_empty(), "unrelated base must not produce a delta");
    }

    /// Deterministic byte-uniform noise (SplitMix64).
    fn splitmix(n: usize, seed: u64) -> Vec<u8> {
        let mut state = seed;
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            let b = z.to_le_bytes();
            let take = (n - out.len()).min(8);
            out.extend_from_slice(&b[..take]);
        }
        out
    }
}
