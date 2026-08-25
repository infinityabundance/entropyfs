//! Entropy coordinates: persisted state-space coordinates and their
//! accounting (`docs/theory/entropy-medium.md` §2).
//!
//! The stored entropy of a configurational representation is
//! `ceil(log2(|state space|))` — the rank selects among `|F|` states.
//! Coordinates that do not fit `u128` are not representable in v1
//! (the candidate is rejected, never truncated).

#![forbid(unsafe_code)]

use crate::entropy::rank::{comb, factorial, multinomial};

/// A mathematical coordinate inside a combinatorial state space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coordinate {
    /// Subset of `k` positions among `n`: `C(n, k)` states.
    Combination {
        /// Universe size.
        n: u64,
        /// Selected positions count.
        k: u64,
    },
    /// Multiset of `m` symbols summing to `n`: `n!/(∏c_i!)` states.
    Multinomial {
        /// Total symbol count.
        n: u64,
        /// Number of distinct symbols.
        m: usize,
    },
    /// Permutation of `m` distinct elements: `m!` states.
    Factoradic {
        /// Number of distinct elements.
        m: u32,
    },
}

impl Coordinate {
    /// Size of the state space (`None` if it overflows `u128`).
    pub fn space(&self) -> Option<u128> {
        match self {
            Coordinate::Combination { n, k } => comb(*n as u128, *k as u128),
            Coordinate::Multinomial { n, m } => multinomial(*n, &vec![1; *m]),
            Coordinate::Factoradic { m } => factorial(*m as u128),
        }
    }

    /// Stored entropy in bits: `ceil(log2(space))`.
    pub fn bits(&self) -> Option<u64> {
        let space = self.space()?;
        let bits = 128 - space.leading_zeros() as u64;
        Some(bits)
    }

    /// Stored entropy in bytes: `ceil(bits / 8)`, minimum 1.
    pub fn bytes(&self) -> Option<u64> {
        self.bits().map(|b| b.div_ceil(8).max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combination_coordinate() {
        let c = Coordinate::Combination { n: 65536, k: 2 };
        assert_eq!(c.space(), comb(65536, 2));
        // C(65536,2) = 2147450880 < 2^31 => 31 bits, 4 bytes
        assert_eq!(c.bits(), Some(31));
        assert_eq!(c.bytes(), Some(4));
    }

    #[test]
    fn multinomial_coordinate() {
        // counts [1; m] with n = m: multinomial = n!/(1!..1!) = n!
        let c = Coordinate::Multinomial { n: 10, m: 10 };
        assert_eq!(c.space(), factorial(10));
    }

    #[test]
    fn factoradic_coordinate() {
        let c = Coordinate::Factoradic { m: 34 };
        assert_eq!(c.space(), factorial(34));
        assert!(c.space().is_some());
        let too_big = Coordinate::Factoradic { m: 35 };
        assert!(too_big.space().is_none());
    }
}
