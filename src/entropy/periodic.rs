//! Periodic structure encoder: `input[i] == input[i % p]` for a small
//! period `p` (pattern repeated, possibly truncated — tail handled).

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
const PERIODIC_WORK_CAP: usize = 8 * 1024 * 1024;

impl Encoder for PeriodicEncoder {
    fn name(&self) -> &'static str {
        "PERIODIC"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        let n = input.len();
        if n < 4 {
            return Vec::new();
        }
        let max_p = (ctx.limits.max_period as usize).min(n / 2);
        if max_p < 2 {
            return Vec::new();
        }
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
