//! Exact residual derivation for base+residual and entropy+residual
//! representations.
//!
//! `R = residual(X, B)`: the exact difference evidence between target `X`
//! and candidate base `B`. Several exact forms are tried; the cheapest is
//! selected by exact cost. XOR is never assumed optimal
//! (`docs/adr/0005-representation-set.md` §11).

#![forbid(unsafe_code)]

use crate::core::candidate::{Candidate, CandidateContext, Encoder};
use crate::core::cost::ByteSplit;
use crate::core::representation::{Edit, RangeChange, Representation, Residual};

/// Base+residual candidate family: `X = apply(B, R)` for each candidate
/// base `B` in the context, with the cheapest exact residual form
/// (XorSparse or RangeReplace; rANS-coded residuals come from
/// `rans::residual::RansResidualEncoder`).
#[derive(Debug, Default)]
pub struct BaseResidualEncoder;

impl Encoder for BaseResidualEncoder {
    fn name(&self) -> &'static str {
        "BASE_RESIDUAL"
    }

    fn encode(&self, input: &[u8], ctx: &CandidateContext<'_>) -> Vec<Candidate> {
        if ctx.bases.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for base in ctx.bases {
            if base.depth >= ctx.limits.max_reference_depth {
                continue;
            }
            if base.bytes.len() != input.len() {
                // v1: same-length bases only (range-based bases are a
                // future refinement).
                continue;
            }
            for residual in derive_residuals(input, &base.bytes, ctx.limits.max_fanout) {
                // A zero-edit residual means X == B exactly: EXACT_REF is
                // strictly cheaper; skip it here.
                if matches!(&residual, Residual::XorSparse { edits, .. } if edits.is_empty()) {
                    continue;
                }
                let rep = Representation::BaseResidual {
                    base: base.id,
                    base_len: base.bytes.len() as u64,
                    residual: residual.clone(),
                    len: input.len() as u64,
                };
                if rep.validate(ctx.limits).is_err() {
                    continue;
                }
                let split = ByteSplit {
                    residual: residual_data_bytes(&residual),
                    reference: 32,
                    ..Default::default()
                };
                let cost = crate::core::cost::estimate(&rep, &split, 0);
                out.push(Candidate {
                    representation: rep,
                    objects: Vec::new(),
                    cost,
                    content_id: ctx.content_id,
                });
            }
        }
        out
    }
}

/// Residual payload bytes (the "data" part, excluding kind/count headers
/// which stay in `descriptor_bytes`). Mirrors the descriptor codec split.
pub fn residual_data_bytes(residual: &Residual) -> u64 {
    match residual {
        Residual::XorSparse { edits, .. } => 5 * edits.len() as u64,
        Residual::RangeReplace {
            changes, literals, ..
        } => 8 * changes.len() as u64 + literals.len() as u64,
        Residual::RansCoded { .. } => 0,
    }
}

/// Derive the exact residual forms relating `target` to `base`.
///
/// Returns candidate residuals in increasing cost order (cheapest first):
/// 1. `XorSparse` when the differing-position count is small;
/// 2. `RangeReplace` when merging differing positions into ranges is
///    cheaper than the sparse edit set.
///
/// `max_fanout` bounds the returned edit/change counts
/// (`docs/security/resource-bounds.md`). If `target == base` the returned
/// vector contains the empty `XorSparse` residual (a zero-cost exact
/// match — the caller can then prefer `EXACT_REF`).
pub fn derive_residuals(target: &[u8], base: &[u8], max_fanout: u32) -> Vec<Residual> {
    if target.len() != base.len() {
        return Vec::new();
    }
    let n = target.len();
    if n == 0 {
        return Vec::new();
    }

    // Pass 1: differing positions.
    let mut edits: Vec<Edit> = Vec::new();
    for i in 0..n {
        if target[i] != base[i] {
            edits.push(Edit {
                pos: i as u32,
                val: target[i] ^ base[i],
            });
        }
    }

    let mut out: Vec<Residual> = Vec::new();
    if edits.len() as u64 <= max_fanout as u64 {
        out.push(Residual::XorSparse {
            len: n as u64,
            edits: edits.clone(),
        });
    }

    // Pass 2: merge consecutive differing positions into ranges.
    let mut changes: Vec<RangeChange> = Vec::new();
    let mut literals: Vec<u8> = Vec::new();
    let mut i = 0usize;
    while i < n {
        if target[i] != base[i] {
            let start = i as u32;
            let mut end = i;
            while end < n && target[end] != base[end] {
                literals.push(target[end]);
                end += 1;
            }
            changes.push(RangeChange {
                start,
                end: end as u32,
            });
            i = end;
        } else {
            i += 1;
        }
    }
    if changes.len() as u64 <= max_fanout as u64 && !changes.is_empty() {
        out.push(Residual::RangeReplace {
            len: n as u64,
            changes,
            literals,
        });
    }

    // Order by encoded size (cheapest first) so callers can take the
    // first element as the best residual form.
    out.sort_by_key(|r| r.encoded_size());
    out
}

/// A simpler "changed-range summary" used by DSFB evidence features:
/// returns the number of differing positions and the number of contiguous
/// differing runs.
pub fn diff_summary(target: &[u8], base: &[u8]) -> (usize, usize) {
    if target.len() != base.len() {
        return (usize::MAX, usize::MAX);
    }
    let mut positions = 0usize;
    let mut runs = 0usize;
    let mut in_run = false;
    for i in 0..target.len() {
        if target[i] != base[i] {
            positions += 1;
            if !in_run {
                runs += 1;
                in_run = true;
            }
        } else {
            in_run = false;
        }
    }
    (positions, runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_targets() {
        let a = b"hello world hello world";
        let r = derive_residuals(a, a, 4096);
        // Only the empty XorSparse (no degenerate empty RangeReplace).
        assert_eq!(r.len(), 1);
        match &r[0] {
            Residual::XorSparse { edits, .. } => assert!(edits.is_empty()),
            other => panic!("expected xor, got {other:?}"),
        }
    }

    #[test]
    fn sparse_diffs() {
        let base = vec![0u8; 100];
        let mut target = base.clone();
        target[10] = 1;
        target[50] = 2;
        let r = derive_residuals(&target, &base, 4096);
        assert!(!r.is_empty());
        match &r[0] {
            Residual::XorSparse { edits, .. } => {
                assert_eq!(edits.len(), 2);
                assert_eq!(edits[0].pos, 10);
                assert_eq!(edits[0].val, 1);
            }
            other => panic!("expected xor, got {other:?}"),
        }
    }

    #[test]
    fn dense_run_uses_range() {
        let base = vec![0u8; 64];
        let mut target = base.clone();
        for i in 16..48 {
            target[i] = 0xFF;
        }
        let r = derive_residuals(&target, &base, 4096);
        // XorSparse would cost 5 + 5*32 = 165; RangeReplace 5 + 8 + 32 = 45.
        assert!(r.iter().any(|x| matches!(x, Residual::RangeReplace { .. })));
        // The cheapest (first) must be the range form.
        assert!(matches!(r[0], Residual::RangeReplace { .. }));
    }

    #[test]
    fn fanout_cap() {
        // Fragmented diff (16 runs) exceeds a fanout of 4 for both forms.
        let base = vec![0u8; 32];
        let target: Vec<u8> = (0..32).map(|i| if i % 2 == 0 { 1 } else { 0 }).collect();
        let r = derive_residuals(&target, &base, 4);
        assert!(r.is_empty());
        // A single contiguous run is one change and stays representable.
        let target2 = vec![1u8; 32];
        let r2 = derive_residuals(&target2, &base, 4);
        assert!(!r2.is_empty());
        assert!(matches!(r2[0], Residual::RangeReplace { .. }));
    }
}
