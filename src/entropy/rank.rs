//! Checked combinatorial rank/unrank arithmetic.
//!
//! All functions return `None`/`Err` on overflow of `u128` — a state space
//! that does not fit is rejected, never truncated. Property tests assert
//! `unrank(rank(x)) == x` and `rank(unrank(i)) == i` over admissible
//! domains (`docs/theory/configurational-storage.md`).
//!
//! # PURPOSE
//!
//! The bijections behind configurational storage
//! (`docs/theory/configurational-storage.md`): every admissible
//! configuration `x` of a family `F(n, ...)` maps to a coordinate
//! `rank(x) ∈ [0, |F|)` and back. These coordinates are the persisted
//! "configurational bytes" of the SPARSE / PALETTE / PERMUTATION
//! descriptors — the saved bits are `ceil(log2 |F|)` instead of the
//! configuration's raw bytes.
//!
//! # BOUNDARY
//!
//! This module knows only integers and slices. It does not know what a
//! chunk is, what a descriptor is, or which family is proposing what: the
//! callers (`sparse.rs`, `sparse64.rs`, `palette.rs`, `permutation.rs`)
//! own the mapping from chunk bytes to position subsets / symbol-index
//! sequences. `rank_multinomial` explicitly documents that its sequence is
//! over symbol *indices* — the palette mapping is the caller's concern.
//!
//! # MODEL
//!
//! Three combinatorial families, each with a rank and an unrank:
//!
//! - combinations: `rank_comb_subset` / `unrank_comb_subset` — the
//!   combinatorial number system (Macaulay), `rank = Σ C(p_i, i+1)`;
//! - multisets: `rank_multinomial` / `unrank_multinomial` — block
//!   decomposition of `n!/(∏ c_i!)` by prefix counts;
//! - permutations: `rank_permutation` / `unrank_permutation` — factoradic
//!   (Lehmer code) over `m ≤ 34` distinct elements.
//!
//! `comb` / `multinomial` / `factorial` are the shared, checked
//! arithmetic primitives.
//!
//! # PERSISTENT AUTHORITY
//!
//! Yes — the coordinates computed here are persisted verbatim in v1
//! descriptors (SPARSE rank u128, PALETTE rank u128, PERMUTATION rank
//! u128; `docs/format/ondisk-v1.md`). The format therefore inherits the
//! `u128` state-space bound: a configuration whose family size exceeds
//! `u128::MAX` is not representable in v1 and must be rejected, never
//! truncated — there is no lossy fallback (`configurational-storage.md`
//! §6). This is what caps PERMUTATION at `m ≤ 34` (`34!` fits u128,
//! `35!` does not) and what SPARSE_BLOCK64 (Phase-8) exists to route
//! around for the sparse family.
//!
//! # CORRECTNESS INVARIANTS
//!
//! - `unrank(rank(x)) == x` for every admissible `x`;
//! - `rank(unrank(i)) == i` for every `i ∈ [0, |F|)`;
//! - no truncation, ever: overflow of an intermediate or final value
//!   yields `None` / a typed [`RankError`], never a wrapped coordinate;
//! - inputs are validated: positions strictly ascending and `< n`,
//!   sequences are permutations of `0..m`, counts sum to `n`.
//!
//! # CONCURRENCY
//!
//! All functions are pure and stateless — no locks, no shared state, no
//! interior mutability. They may be called from any thread (parallel
//! chunk preparation, Phase-10C, calls them concurrently).
//!
//! # RESOURCE BOUNDS
//!
//! The `u128` accumulator bounds every result: the largest representable
//! family has `|F| ≤ u128::MAX ≈ 3.40e38`. Time is polynomial in `n` and
//! the family: `comb` is `O(k)` multiplications with constant-bounded gcd
//! steps (Euclid on 128-bit values); the multinomial rank/unrank loops
//! are `O(n·m)` block evaluations; the subset unrank performs `O(k log n)`
//! `comb` evaluations in its binary search. All inputs are caller-sized
//! (chunk length ≤ 256 KiB in v1), so no attacker-controlled size reaches
//! this module unbounded.
//!
//! # PERFORMANCE
//!
//! `comb` uses the multiplicative form with full gcd reduction at every
//! step so intermediate values never exceed the final value — the result
//! fits `u128` exactly when the true binomial does (no spurious
//! overflow). The subset unrank replaces a linear scan with a binary
//! search over `[i-1, c]`: `O(k log n)` instead of `O(k·n)` `comb`
//! evaluations.
//!
//! # FAILURE MODES
//!
//! Every failure is a typed rejection ([`RankError`]) or `None`: out-of-
//! range ranks, `k > n`, non-ascending positions, count mismatches,
//! state spaces that overflow `u128`. The one thing that must never
//! happen is a silent wrong coordinate — the property round-trip tests
//! and the Kani harnesses at bounded sizes guard exactly that
//! (`configurational-storage.md` §7).
//!
//! # HISTORY / EVIDENCE
//!
//! The overflow-rejection contract is a Phase-1 format decision
//! (`docs/theory/configurational-storage.md` §1, §6). The boundary tests
//! here pin the binomial cliff — `C(65536, 9) ≈ 2^125.5` fits u128 while
//! `C(65536, 10)` overflows — which is the exact cliff SPARSE_BLOCK64
//! (Phase-8, tag `0x0E`) later removed for the sparse family
//! (`docs/format/ondisk-v1.md`).

#![forbid(unsafe_code)]

/// Typed rejection for rank/unrank arithmetic.
///
/// The module contract: an error always means *rejected*, never
/// *truncated*. A coordinate or state space that does not fit `u128` must
/// not be silently wrapped — the candidate family simply is not
/// representable in v1 (`docs/theory/configurational-storage.md` §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankError {
    /// k exceeds n.
    KExceedsN,
    /// The state space exceeds u128 (not representable in v1).
    SpaceOverflow,
    /// Rank out of admissible range.
    RankOutOfRange,
    /// Position out of range or not strictly ascending.
    BadPositions,
    /// Counts do not sum to n.
    CountMismatch,
    /// n too large for the family.
    SizeTooLarge,
    /// Generic overflow.
    Overflow,
}

impl std::fmt::Display for RankError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for RankError {}

/// `C(n, k)` with checked `u128` arithmetic. `None` on overflow.
///
/// Uses the multiplicative form with full gcd reduction at every step, so
/// intermediate values never exceed the final value (which is ≤ C(n, n/2));
/// the result fits `u128` exactly when the true binomial does.
///
/// That "fits exactly when the true binomial does" property is what lets
/// the encoders distinguish *genuinely unusable* state spaces (e.g.
/// `C(65536, 10)` — the plain-SPARSE cliff at 64 KiB) from merely large
/// ones (`C(65536, 9) ≈ 2^125.5` still fits; see the boundary tests).
pub fn comb(n: u128, k: u128) -> Option<u128> {
    if k > n {
        return Some(0);
    }
    let k = k.min(n - k);
    let mut r: u128 = 1;
    for i in 1..=k {
        let mut num = n - k + i;
        let mut den = i;
        // reduce r/den
        let g = gcd_u128(r, den);
        r /= g;
        den /= g;
        // reduce num/den
        let g2 = gcd_u128(num, den);
        num /= g2;
        den /= g2;
        debug_assert_eq!(den, 1);
        r = r.checked_mul(num)?;
    }
    Some(r)
}

/// Euclid's algorithm for `u128`.
pub fn gcd_u128(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// Checked factorial.
pub fn factorial(n: u128) -> Option<u128> {
    let mut r: u128 = 1;
    for i in 2..=n {
        r = r.checked_mul(i)?;
    }
    Some(r)
}

/// Rank of an ascending position subset `p_0 < p_1 < ... < p_{k-1}` inside
/// `[0, n)`, using the combinatorial number system:
/// `rank = Σ C(p_i, i+1)`.
///
/// For valid inputs the result satisfies `rank < C(n, k)` — the encoders
/// rely on this when validating rank ranges (`Representation::validate`
/// double-checks it before a candidate is proposed).
pub fn rank_comb_subset(positions: &[u32], n: u64) -> Result<u128, RankError> {
    // -------------------------------------------------------------------
    // Stage 1: validate the subset while accumulating the rank.
    //
    // Positions must be strictly ascending and inside `[0, n)`; each term
    // `C(p_i, i+1)` is checked, so an unrepresentable state space rejects
    // the whole coordinate (never a truncated rank).
    // -------------------------------------------------------------------
    let mut rank: u128 = 0;
    let mut prev: Option<u32> = None;
    for (i, &p) in positions.iter().enumerate() {
        if p as u64 >= n {
            return Err(RankError::BadPositions);
        }
        if let Some(pp) = prev {
            if p <= pp {
                return Err(RankError::BadPositions);
            }
        }
        prev = Some(p);
        let c = comb(p as u128, i as u128 + 1).ok_or(RankError::SpaceOverflow)?;
        rank = rank.checked_add(c).ok_or(RankError::SpaceOverflow)?;
    }
    Ok(rank)
}

/// Unrank a combination rank into the ascending position subset of size `k`
/// inside `[0, n)`.
///
/// # What
///
/// Recover `p_0 < p_1 < ... < p_{k-1}` from `rank ∈ [0, C(n, k))` via the
/// combinatorial number system: greedily recover the *descending*
/// coordinates `c_k > c_{k-1} > ... > c_1` with `c_i = max{c : C(c, i) ≤ x}`
/// (`x` the remaining rank), then reverse.
///
/// # Why
///
/// This is the decode direction of configurational storage: a SPARSE
/// descriptor persists only `(k, rank, literals)`, and the materializer
/// must regenerate the marked positions from the coordinate alone
/// (`docs/theory/configurational-storage.md` §2).
///
/// # Algorithm
///
/// Each coordinate is found by binary search over `[i-1, c]` for the
/// largest `c` with `C(c, i) ≤ x` — `O(k log n)` `comb` evaluations
/// instead of a linear scan.
///
/// # Invariants
///
/// - Precondition: `k ≤ n` and `rank < C(n, k)` (checked).
/// - Postcondition: strictly ascending positions in `[0, n)`.
/// - The trailing `x == 0` and `desc.len() == k` checks make the recovery
///   exact: an invalid rank cannot yield a partial subset.
pub fn unrank_comb_subset(rank: u128, n: u64, k: u64) -> Result<Vec<u32>, RankError> {
    // -------------------------------------------------------------------
    // Stage 1: admissible-domain gate.
    //
    // Reject `k > n` and out-of-range ranks before any recovery work.
    // -------------------------------------------------------------------
    if k > n {
        return Err(RankError::KExceedsN);
    }
    let total = comb(n as u128, k as u128).ok_or(RankError::SpaceOverflow)?;
    if rank >= total {
        return Err(RankError::RankOutOfRange);
    }
    let k = k as u32;
    // -------------------------------------------------------------------
    // Stage 2: greedy descending recovery.
    //
    // Recover c_k > c_{k-1} > ... > c_1 (descending), then reverse. `c` is
    // the running upper bound for the next coordinate; `x` is the remaining
    // rank after subtracting the coordinates recovered so far.
    // -------------------------------------------------------------------
    let mut x = rank;
    let mut c: u128 = n as u128 - 1;
    let mut desc = Vec::with_capacity(k as usize);
    for i in (1..=k as u128).rev() {
        // Find the largest c with C(c, i) <= x via binary search in [i-1, c].
        let mut lo = i - 1;
        let mut hi = c;
        while lo < hi {
            let mid = lo + (hi - lo + 1).div_ceil(2);
            let cm = comb(mid, i).ok_or(RankError::SpaceOverflow)?;
            if cm <= x {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let c_i = lo;
        let cm = comb(c_i, i).ok_or(RankError::SpaceOverflow)?;
        x = x.checked_sub(cm).ok_or(RankError::Overflow)?;
        desc.push(c_i as u32);
        if c_i > 0 {
            c = c_i - 1;
        } else {
            break;
        }
    }
    // -------------------------------------------------------------------
    // Stage 3: exactness check and canonical order.
    //
    // The rank must be consumed exactly and all `k` coordinates recovered;
    // only then is the result reversed to ascending order.
    // -------------------------------------------------------------------
    if x != 0 || desc.len() != k as usize {
        return Err(RankError::RankOutOfRange);
    }
    desc.reverse();
    Ok(desc)
}

/// Multinomial coefficient `n! / (c_1! ... c_m!)` with checked arithmetic.
/// `counts` must sum to `n` (checked).
///
/// Computed as the product of successive combination coefficients
/// `C(remaining, c_i)`: intermediates stay minimal and the checked `comb`
/// is reused. The `remaining -= c` subtraction is safe because the
/// count-sum check guarantees the counts partition `n`.
pub fn multinomial(n: u64, counts: &[u32]) -> Option<u128> {
    let mut total: u64 = 0;
    for &c in counts {
        total = total.checked_add(c as u64)?;
    }
    if total != n {
        return None;
    }
    let mut r: u128 = 1;
    let mut remaining = n;
    for &c in counts {
        let ck = comb(remaining as u128, c as u128)?;
        r = r.checked_mul(ck)?;
        remaining -= c as u64;
    }
    Some(r)
}

/// Rank of an index sequence under multiset counts (symbols `0..m` with
/// multiplicity `counts`). The sequence is over symbol *indices*; the
/// palette mapping is the caller's concern.
///
/// The rank is the lexicographic position of `seq` among all sequences
/// with these multiplicities: the sum, over each position, of the sizes of
/// all blocks whose next symbol precedes the chosen one. The state space
/// is `|F| = n!/(∏ c_i!)`; a space that overflows `u128` is rejected
/// (never truncated).
pub fn rank_multinomial(seq: &[u8], n: u64, counts: &[u32]) -> Result<u128, RankError> {
    // -------------------------------------------------------------------
    // Stage 1: validate length and that the counts partition n.
    // -------------------------------------------------------------------
    if seq.len() as u64 != n {
        return Err(RankError::CountMismatch);
    }
    let mut counts = counts.to_vec();
    let m = counts.len();
    let mut sum: u64 = 0;
    for &c in counts.iter() {
        sum = sum.checked_add(c as u64).ok_or(RankError::Overflow)?;
    }
    if sum != n {
        return Err(RankError::CountMismatch);
    }
    // -------------------------------------------------------------------
    // Stage 2: per-position block accumulation.
    //
    // For each symbol `t < s` still available, tentatively decrement
    // `counts[t]`, add the block size (all completions with `t` at this
    // position) to the rank, and restore the count; committing the actual
    // symbol `s` decrements it permanently. The trial decrement/restore
    // works on a copy, so the caller's slice is untouched.
    // -------------------------------------------------------------------
    let mut rank: u128 = 0;
    for (pos, &s) in seq.iter().enumerate() {
        let s = s as usize;
        if s >= m {
            return Err(RankError::BadPositions);
        }
        if counts[s] == 0 {
            return Err(RankError::CountMismatch);
        }
        let remaining = n - pos as u64 - 1;
        for t in 0..s {
            if counts[t] == 0 {
                continue;
            }
            counts[t] -= 1;
            let block = multinomial(remaining, &counts).ok_or(RankError::SpaceOverflow)?;
            rank = rank.checked_add(block).ok_or(RankError::SpaceOverflow)?;
            counts[t] += 1;
        }
        counts[s] -= 1;
    }
    Ok(rank)
}

/// Unrank a multiset coordinate into the symbol-index sequence.
///
/// Greedy inverse of [`rank_multinomial`]: at each position, walk the
/// symbols in ascending order, subtracting each block size until the
/// remaining rank falls inside one block; that symbol is the choice. The
/// final `rem == 0` check guarantees the coordinate was consumed exactly.
pub fn unrank_multinomial(rank: u128, n: u64, counts: &[u32]) -> Result<Vec<u8>, RankError> {
    // -------------------------------------------------------------------
    // Stage 1: state-space bound and rank-range gate.
    // -------------------------------------------------------------------
    let total = multinomial(n, counts).ok_or(RankError::SpaceOverflow)?;
    if rank >= total {
        return Err(RankError::RankOutOfRange);
    }
    let mut counts = counts.to_vec();
    let m = counts.len();
    let mut rem = rank;
    // -------------------------------------------------------------------
    // Stage 2: per-position greedy block selection.
    //
    // Symbols are tried in ascending order; each trial decrements the
    // count, measures the block, and restores it unless the rank lands
    // inside that block (the chosen symbol stays decremented).
    // -------------------------------------------------------------------
    let mut out = Vec::with_capacity(n as usize);
    for pos in 0..n as usize {
        let remaining = n as usize - pos - 1;
        let mut chosen: Option<u8> = None;
        for s in 0..m {
            if counts[s] == 0 {
                continue;
            }
            counts[s] -= 1;
            let block = multinomial(remaining as u64, &counts).ok_or(RankError::SpaceOverflow)?;
            if rem < block {
                chosen = Some(s as u8);
                break;
            }
            rem -= block;
            counts[s] += 1;
        }
        let s = chosen.ok_or(RankError::RankOutOfRange)?;
        out.push(s);
    }
    // -------------------------------------------------------------------
    // Stage 3: exactness — the rank must be fully consumed.
    // -------------------------------------------------------------------
    if rem != 0 {
        return Err(RankError::RankOutOfRange);
    }
    Ok(out)
}

/// Factoradic rank of a permutation of `m` distinct elements (`m ≤ 34`).
/// `seq` contains the permutation of `0..m`.
///
/// Lehmer code: at each position, the count of still-unused elements
/// smaller than the chosen one, weighted by `(m-i-1)!`. The `m ≤ 34` cap
/// is exact: `34!` fits `u128`, `35!` does not (`docs/theory/
/// configurational-storage.md` §4).
pub fn rank_permutation(seq: &[u8]) -> Result<u128, RankError> {
    // -------------------------------------------------------------------
    // Stage 1: size gate and permutation validation.
    // -------------------------------------------------------------------
    let m = seq.len();
    if m > 34 {
        return Err(RankError::SizeTooLarge);
    }
    // Validate: must be a permutation of 0..m.
    let mut used = vec![false; m];
    for &s in seq {
        if (s as usize) >= m || used[s as usize] {
            return Err(RankError::BadPositions);
        }
        used[s as usize] = true;
    }
    // -------------------------------------------------------------------
    // Stage 2: Lehmer-code accumulation.
    // -------------------------------------------------------------------
    // Lehmer code: for each position, count unused elements smaller than it.
    let mut unused = vec![true; m];
    let mut rank: u128 = 0;
    for (i, &s) in seq.iter().enumerate() {
        let s = s as usize;
        let smaller = unused[..s].iter().filter(|&&u| u).count();
        let f = factorial((m - i - 1) as u128).ok_or(RankError::SpaceOverflow)?;
        rank = rank
            .checked_add(
                (smaller as u128)
                    .checked_mul(f)
                    .ok_or(RankError::SpaceOverflow)?,
            )
            .ok_or(RankError::SpaceOverflow)?;
        unused[s] = false;
    }
    Ok(rank)
}

/// Factoradic unrank: recover the permutation of `0..m` at `rank`.
///
/// Greedy inverse of [`rank_permutation`]: at each position, the
/// coordinate `idx = rank / (m-i-1)!` selects the `idx`-th remaining
/// element and `rank %= (m-i-1)!` carries the remainder.
pub fn unrank_permutation(rank: u128, m: usize) -> Result<Vec<u8>, RankError> {
    // -------------------------------------------------------------------
    // Stage 1: size gate and rank-range check.
    // -------------------------------------------------------------------
    if m > 34 {
        return Err(RankError::SizeTooLarge);
    }
    let total = factorial(m as u128).ok_or(RankError::SpaceOverflow)?;
    if rank >= total {
        return Err(RankError::RankOutOfRange);
    }
    // -------------------------------------------------------------------
    // Stage 2: factoradic greedy selection.
    // -------------------------------------------------------------------
    let mut rem = rank;
    let mut available: Vec<u8> = (0..m as u8).collect();
    let mut out = Vec::with_capacity(m);
    for i in 0..m {
        let f = factorial((m - i - 1) as u128).ok_or(RankError::SpaceOverflow)?;
        // f is the factorial of a non-negative number, so f >= 1 always.
        let idx = (rem / f) as usize;
        rem %= f;
        if idx >= available.len() {
            return Err(RankError::RankOutOfRange);
        }
        out.push(available.remove(idx));
    }
    // -------------------------------------------------------------------
    // Stage 3: exactness — the rank must be fully consumed.
    // -------------------------------------------------------------------
    if rem != 0 {
        return Err(RankError::RankOutOfRange);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comb_basics() {
        assert_eq!(comb(8, 3), Some(56));
        assert_eq!(comb(5, 0), Some(1));
        assert_eq!(comb(5, 5), Some(1));
        assert_eq!(comb(0, 0), Some(1));
        assert_eq!(comb(3, 5), Some(0));
        assert_eq!(comb(52, 5), Some(2_598_960));
    }

    #[test]
    fn comb_symmetry() {
        for n in 0..20u128 {
            for k in 0..=n {
                assert_eq!(comb(n, k), comb(n, n - k));
            }
        }
    }

    #[test]
    fn comb_fits_u128_boundary() {
        // C(65536, 9) ≈ 2^125.5 fits u128; C(65536, 10) overflows.
        assert!(comb(65536, 9).is_some());
        assert_eq!(comb(65536, 10), None);
        assert_eq!(comb(65536, 2), Some(2147450880));
    }

    #[test]
    fn subset_roundtrip_small() {
        // exhaustive over all subsets of {0..5} with k=2
        let n = 5u64;
        for a in 0..n {
            for b in (a + 1)..n {
                let pos = vec![a as u32, b as u32];
                let r = rank_comb_subset(&pos, n).unwrap();
                assert!(r < comb(n as u128, 2).unwrap());
                let back = unrank_comb_subset(r, n, 2).unwrap();
                assert_eq!(back, pos);
            }
        }
    }

    #[test]
    fn subset_roundtrip_exhaustive_k1() {
        for n in 1..=20u64 {
            for a in 0..n {
                let r = rank_comb_subset(&[a as u32], n).unwrap();
                assert_eq!(unrank_comb_subset(r, n, 1).unwrap(), vec![a as u32]);
            }
        }
    }

    #[test]
    fn subset_rank_overflow_rejected() {
        // C(65535, 10) overflows u128 => the rank sum is rejected, never
        // truncated. (Small positions yield zero terms and succeed.)
        assert!(
            rank_comb_subset(
                &[
                    65526, 65527, 65528, 65529, 65530, 65531, 65532, 65533, 65534, 65535
                ],
                65536
            )
            .is_err()
        );
    }

    #[test]
    fn multinomial_counts() {
        // 4 positions, counts [2, 2]: 4!/(2!2!) = 6
        assert_eq!(multinomial(4, &[2, 2]), Some(6));
        assert_eq!(multinomial(4, &[4]), Some(1));
        assert_eq!(multinomial(5, &[2, 3]), Some(10));
        // counts don't sum to n
        assert_eq!(multinomial(4, &[1, 1]), None);
    }

    #[test]
    fn multinomial_roundtrip_exhaustive_small() {
        // n = 4, counts [2,1,1]: 12 states
        let counts = [2u32, 1, 1];
        let n = 4u64;
        for rank in 0..12 {
            let seq = unrank_multinomial(rank, n, &counts).unwrap();
            let back = rank_multinomial(&seq, n, &counts).unwrap();
            assert_eq!(back, rank, "rank {rank} -> seq {seq:?} -> {back}");
        }
    }

    #[test]
    fn multinomial_out_of_range() {
        let counts = [2u32, 1, 1];
        assert!(unrank_multinomial(12, 4, &counts).is_err());
        assert!(unrank_multinomial(13, 4, &counts).is_err());
    }

    #[test]
    fn permutation_roundtrip() {
        for m in 1..=7usize {
            let total = factorial(m as u128).unwrap();
            for rank in 0..total {
                let perm = unrank_permutation(rank, m).unwrap();
                let back = rank_permutation(&perm).unwrap();
                assert_eq!(back, rank, "m={m} rank={rank}");
            }
        }
    }

    #[test]
    fn permutation_size_cap() {
        // 35! overflows u128; the rank functions must reject m > 34.
        let big = vec![0u8; 35];
        assert!(rank_permutation(&big).is_err());
        assert!(unrank_permutation(0, 35).is_err());
    }

    #[test]
    fn unrank_34_fits() {
        assert!(unrank_permutation(0, 34).is_ok());
        // 34! ≈ 2.95e38 < u128::MAX ≈ 3.40e38
        assert!(factorial(34).is_some());
        assert!(factorial(35).is_none());
    }
}
