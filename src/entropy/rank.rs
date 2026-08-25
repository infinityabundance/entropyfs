//! Checked combinatorial rank/unrank arithmetic.
//!
//! All functions return `None`/`Err` on overflow of `u128` — a state space
//! that does not fit is rejected, never truncated. Property tests assert
//! `unrank(rank(x)) == x` and `rank(unrank(i)) == i` over admissible
//! domains (`docs/theory/configurational-storage.md`).

#![forbid(unsafe_code)]

/// Rank/unrank arithmetic errors.
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
pub fn rank_comb_subset(positions: &[u32], n: u64) -> Result<u128, RankError> {
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
pub fn unrank_comb_subset(rank: u128, n: u64, k: u64) -> Result<Vec<u32>, RankError> {
    if k > n {
        return Err(RankError::KExceedsN);
    }
    let total = comb(n as u128, k as u128).ok_or(RankError::SpaceOverflow)?;
    if rank >= total {
        return Err(RankError::RankOutOfRange);
    }
    let k = k as u32;
    let mut x = rank;
    // Recover c_k > c_{k-1} > ... > c_1 (descending), then reverse.
    let mut c: u128 = n as u128 - 1;
    let mut desc = Vec::with_capacity(k as usize);
    for i in (1..=k as u128).rev() {
        // Find the largest c with C(c, i) <= x via binary search in [i-1, c].
        let mut lo = i - 1;
        let mut hi = c;
        while lo < hi {
            let mid = lo + (hi - lo + 1) / 2;
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
    if x != 0 || desc.len() != k as usize {
        return Err(RankError::RankOutOfRange);
    }
    desc.reverse();
    Ok(desc)
}

/// Multinomial coefficient `n! / (c_1! ... c_m!)` with checked arithmetic.
/// `counts` must sum to `n` (checked).
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
pub fn rank_multinomial(seq: &[u8], n: u64, counts: &[u32]) -> Result<u128, RankError> {
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
pub fn unrank_multinomial(rank: u128, n: u64, counts: &[u32]) -> Result<Vec<u8>, RankError> {
    let total = multinomial(n, counts).ok_or(RankError::SpaceOverflow)?;
    if rank >= total {
        return Err(RankError::RankOutOfRange);
    }
    let mut counts = counts.to_vec();
    let m = counts.len();
    let mut rem = rank;
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
    if rem != 0 {
        return Err(RankError::RankOutOfRange);
    }
    Ok(out)
}

/// Factoradic rank of a permutation of `m` distinct elements (`m ≤ 34`).
/// `seq` contains the permutation of `0..m`.
pub fn rank_permutation(seq: &[u8]) -> Result<u128, RankError> {
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
    // Lehmer code: for each position, count unused elements smaller than it.
    let mut unused = vec![true; m];
    let mut rank: u128 = 0;
    for i in 0..m {
        let s = seq[i] as usize;
        let mut smaller = 0usize;
        for t in 0..s {
            if unused[t] {
                smaller += 1;
            }
        }
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
pub fn unrank_permutation(rank: u128, m: usize) -> Result<Vec<u8>, RankError> {
    if m > 34 {
        return Err(RankError::SizeTooLarge);
    }
    let total = factorial(m as u128).ok_or(RankError::SpaceOverflow)?;
    if rank >= total {
        return Err(RankError::RankOutOfRange);
    }
    let mut rem = rank;
    let mut available: Vec<u8> = (0..m as u8).collect();
    let mut out = Vec::with_capacity(m);
    for i in 0..m {
        let f = factorial((m - i - 1) as u128).ok_or(RankError::SpaceOverflow)?;
        let idx = if f == 0 { 0 } else { (rem / f) as usize };
        rem %= f;
        if idx >= available.len() {
            return Err(RankError::RankOutOfRange);
        }
        out.push(available.remove(idx));
    }
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
