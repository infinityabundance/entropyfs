//! Periodic structure encoder: `input[i] == input[i % p]` for a small
//! period `p` (pattern repeated, possibly truncated — tail handled).
//!
//! # PURPOSE
//!
//! The PERIODIC representation family (tag `0x09`): a chunk that is a
//! pattern repeated `count` times plus a short tail. `len = p·count + tail`;
//! FILL is the `p = 1` special case (`docs/theory/
//! configurational-storage.md` §5).
//!
//! # BOUNDARY
//!
//! A pure candidate encoder: it searches for a period and proposes the
//! smallest one it can verify; it never touches the store and never
//! decides whether the family wins (ADR-0010). FILL (period 1) is
//! subsumed by the same machinery but constructed separately by
//! `fill_candidate` (cheaper), so this encoder starts at period 2.
//!
//! # MODEL
//!
//! `chunk = pattern^count ‖ tail` with `count = n/p` and
//! `tail = input[p·count..]` (`tail.len() < p`). The descriptor persists
//! `(period, pattern, count, tail, len)`; materialization repeats the
//! pattern and appends the tail.
//!
//! # PERSISTENT AUTHORITY
//!
//! Yes: the descriptor is persisted verbatim when this candidate wins
//! (`docs/format/ondisk-v1.md`, tag `0x09`: `period u32, pattern (period
//! bytes), count u32, tail_len u32, tail`).
//!
//! # CORRECTNESS INVARIANTS
//!
//! - the chosen period reproduces the chunk exactly — verified by the
//!   full compare `input[p..] == input[..n-p]` (equivalently
//!   `input[i] == input[i-p]` for all `i ≥ p`);
//! - the smallest valid period is selected: the ascending search returns
//!   on the first hit (pinned by `smallest_period_wins`);
//! - `len == p·count + tail.len()` and `tail.len() < p`;
//! - materialization is byte-exact — enforced by the §32 candidate
//!   validation gate.
//!
//! # CONCURRENCY
//!
//! Stateless encoder; safe to call from any thread (parallel chunk
//! preparation, Phase-10C).
//!
//! # RESOURCE BOUNDS
//!
//! `n ≥ 4`; `p ≤ min(max_period, n/2)` (a period must repeat at least
//! twice). The search budget [`PERIODIC_WORK_CAP`] — 8 MiB of total
//! byte-comparisons — bounds foreground CPU for any chunk size; it is the
//! encode-side analog of the decode-side `max_decode_work` budget
//! (`docs/security/resource-bounds.md`). Worst case is `O(n·max_p)`
//! comparisons, but the work cap cuts it off well before that.
//!
//! # PERFORMANCE
//!
//! An O(1) first-boundary filter (`input[p] == input[0]`) skips a full
//! compare for most candidate periods; the full compare runs only on a
//! boundary match, and the first (smallest) hit returns immediately. The
//! honest `ByteSplit` (tail bytes as residual payload; pattern/count stay
//! in the descriptor) means the cost function sees exactly what would be
//! persisted.
//!
//! # FAILURE MODES
//!
//! Work-cap exhaustion or a failed `Representation::validate` yields an
//! empty candidate list — the family skips itself and RAW wins. Nothing
//! here panics; a wrong candidate would be caught by the §32 gate.
//!
//! # HISTORY / EVIDENCE
//!
//! Phase-1 family (`docs/theory/configurational-storage.md` §5); the
//! smallest-period canonical form is pinned by `smallest_period_wins`.

#![forbid(unsafe_code)]

use crate::core::candidate::{Candidate, CandidateContext, Encoder};
use crate::core::cost::ByteSplit;
use crate::core::representation::Representation;

/// Periodic encoder. `FILL` (period 1) is subsumed by the same machinery;
/// the FILL candidate is cheaper to construct separately, so the periodic
/// encoder starts at period 2 (period 1 handled by `fill_candidate`).
#[derive(Debug, Default)]
pub struct PeriodicEncoder;

/// Search budget: total byte-comparisons before giving up on finding a
/// period. Bounded foreground behavior (`docs/security/resource-bounds.md`).
///
/// This is an encode-side CPU bound: the write path must never spend
/// unbounded time hunting for structure, regardless of chunk size.
const PERIODIC_WORK_CAP: usize = 8 * 1024 * 1024;

impl Encoder for PeriodicEncoder {
    fn name(&self) -> &'static str {
        "PERIODIC"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        // -------------------------------------------------------------------
        // Stage 1: size and period-range gates. `n < 4` has no room for a
        // period-2 repetition plus tail; `max_p ≤ n/2` guarantees a period
        // repeats at least twice.
        // -------------------------------------------------------------------
        let n = input.len();
        if n < 4 {
            return Vec::new();
        }
        let max_p = (ctx.limits.max_period as usize).min(n / 2);
        if max_p < 2 {
            return Vec::new();
        }
        // -------------------------------------------------------------------
        // Stage 2: ascending period search. The first hit is the smallest
        // period — the canonical (and cheapest) encoding. Every byte
        // comparison is counted against the work cap.
        // -------------------------------------------------------------------
        let mut work: usize = 0;
        for p in 2..=max_p {
            // O(1) filter: the first repeat boundary must match.
            if input[p] != input[0] {
                work += 1;
                if work > PERIODIC_WORK_CAP {
                    return Vec::new();
                }
                continue;
            }
            // -------------------------------------------------------------------
            // Stage 3: full verification and descriptor construction on the
            // first (smallest) valid period.
            // -------------------------------------------------------------------
            // Full check: input[p..] == input[..n-p] (equivalently
            // input[i] == input[i-p] for all i >= p).
            work += n - p;
            if work > PERIODIC_WORK_CAP {
                return Vec::new();
            }
            if input[p..] == input[..n - p] {
                let period = p as u32;
                let count = (n / p) as u32;
                let tail = input[p * (n / p)..].to_vec();
                let rep = Representation::Periodic {
                    period,
                    pattern: input[..p].to_vec(),
                    count,
                    tail: tail.clone(),
                    len: n as u64,
                };
                if rep.validate(ctx.limits).is_err() {
                    return Vec::new();
                }
                // -------------------------------------------------------------------
                // Stage 4: honest accounting and candidate — the tail bytes
                // are residual payload; pattern/count stay in the
                // descriptor. The cost function decides from here.
                // -------------------------------------------------------------------
                let split = ByteSplit {
                    residual: tail.len() as u64,
                    ..Default::default()
                };
                let cost = crate::core::cost::estimate(&rep, &split, 0);
                return vec![Candidate {
                    representation: rep,
                    objects: Vec::new(),
                    cost,
                    content_id: ctx.content_id,
                }];
            }
        }
        // No period ≤ max_p reproduces the chunk — the family does not apply.
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::candidate::CandidateContext;
    use crate::core::cost::Policy;
    use crate::core::limits::Limits;
    use crate::core::materialize::materialize_to_vec;
    use crate::tests::helpers::MemResolver;

    fn ctx_for<'a>(input: &[u8], limits: &'a Limits, policy: &'a Policy) -> CandidateContext<'a> {
        CandidateContext {
            limits,
            policy,
            content_id: crate::core::extent::ChunkId::of(input),
            bases: &[],
            dedup: None,
        }
    }

    #[test]
    fn periodic_roundtrip() {
        // pattern "abcde" repeated 100 times = 500 bytes
        let limits = Limits::default();
        let policy = Policy::default();
        let mut input = Vec::with_capacity(500);
        for _ in 0..100 {
            input.extend_from_slice(b"abcde");
        }
        let cands = PeriodicEncoder.encode(&input, &ctx_for(&input, &limits, &policy));
        assert_eq!(cands.len(), 1);
        let resolver = MemResolver::empty();
        let out =
            materialize_to_vec(&cands[0].representation, &resolver, &Limits::default()).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn periodic_with_tail() {
        let limits = Limits::default();
        let policy = Policy::default();
        let mut input = Vec::new();
        for _ in 0..33 {
            input.extend_from_slice(b"xy");
        }
        input.extend_from_slice(b"x"); // 67 bytes, period 2, tail "x"
        let cands = PeriodicEncoder.encode(&input, &ctx_for(&input, &limits, &policy));
        assert_eq!(cands.len(), 1);
        assert!(matches!(
            cands[0].representation,
            Representation::Periodic {
                period: 2,
                count: 33,
                ..
            }
        ));
        let resolver = MemResolver::empty();
        let out =
            materialize_to_vec(&cands[0].representation, &resolver, &Limits::default()).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn periodic_skips_nonperiodic() {
        let limits = Limits::default();
        let policy = Policy::default();
        // Depends on both i%251 and i/251: no period <= 512 reproduces it.
        let input: Vec<u8> = (0..1000u32)
            .map(|i| ((i % 251) + (i / 251)) % 251)
            .map(|v| v as u8)
            .collect();
        assert!(
            PeriodicEncoder
                .encode(&input, &ctx_for(&input, &limits, &policy))
                .is_empty()
        );
    }

    #[test]
    fn smallest_period_wins() {
        let limits = Limits::default();
        let policy = Policy::default();
        // period 4 structure is also period 8; smallest must win.
        let mut input = Vec::new();
        for _ in 0..64 {
            input.extend_from_slice(b"abcd");
        }
        let cands = PeriodicEncoder.encode(&input, &ctx_for(&input, &limits, &policy));
        assert_eq!(cands.len(), 1);
        match &cands[0].representation {
            Representation::Periodic { period, .. } => assert_eq!(*period, 4),
            other => panic!("expected periodic, got {other:?}"),
        }
    }
}
