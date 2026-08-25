//! Rank/unrank property tests: `unrank(rank(x)) == x` and
//! `rank(unrank(i)) == i` over admissible domains
//! (`docs/theory/configurational-storage.md`).

#![forbid(unsafe_code)]

use proptest::prelude::*;

use crate::entropy::rank::{
    multinomial, rank_comb_subset, rank_multinomial, rank_permutation, unrank_comb_subset,
    unrank_multinomial, unrank_permutation,
};

proptest! {
    #[test]
    fn combination_rank_unrank_roundtrip(
        n in 1u64..300,
        k in 1u64..40,
        seed_rank in any::<u64>(),
    ) {
        let k = k.min(n);
        // Only when the state space fits u128.
        if let Some(total) = crate::entropy::rank::comb(n as u128, k as u128) {
            let rank = (seed_rank as u128) % total;
            let positions = unrank_comb_subset(rank, n, k).unwrap();
            // positions strictly ascending, in range
            for w in positions.windows(2) {
                assert!(w[0] < w[1]);
            }
            assert!(positions.iter().all(|&p| (p as u64) < n));
            let back = rank_comb_subset(&positions, n).unwrap();
            assert_eq!(back, rank);
            // and unrank(rank(x)) == x
            let positions2 = unrank_comb_subset(back, n, k).unwrap();
            assert_eq!(positions2, positions);
        }
    }

    #[test]
    fn multinomial_rank_unrank_roundtrip(
        n in 1u64..40,
        m in 2usize..7,
        seed_rank in any::<u64>(),
    ) {
        // random counts summing to n over m bins
        let mut counts = vec![0u32; m];
        let mut rem = n;
        for c in counts.iter_mut().take(m - 1) {
            let take = (seed_rank as u64 % (rem + 1)) as u32;
            *c = take;
            rem -= take as u64;
        }
        counts[m - 1] = rem as u32;
        if let Some(total) = multinomial(n, &counts) {
            let rank = (seed_rank as u128) % total;
            let seq = unrank_multinomial(rank, n, &counts).unwrap();
            assert_eq!(seq.len() as u64, n);
            let back = rank_multinomial(&seq, n, &counts).unwrap();
            assert_eq!(back, rank);
        }
    }

    #[test]
    fn permutation_rank_unrank_roundtrip(
        m in 1usize..9,
        seed_rank in any::<u64>(),
    ) {
        let total = crate::entropy::rank::factorial(m as u128).unwrap();
        let rank = (seed_rank as u128) % total;
        let perm = unrank_permutation(rank, m).unwrap();
        assert_eq!(perm.len(), m);
        let back = rank_permutation(&perm).unwrap();
        assert_eq!(back, rank);
        // permutation property: all elements 0..m exactly once
        let mut seen = vec![false; m];
        for &s in &perm {
            assert!(!seen[s as usize]);
            seen[s as usize] = true;
        }
    }
}
