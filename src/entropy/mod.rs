//! Mathematical/configurational storage machinery.
//!
//! Owns entropy universes, coordinates, rank/unrank, sparse and palette
//! configurations, periodic structures, deterministic transforms, exact
//! residual forms, and candidate derivation for mathematical
//! representation families (`docs/theory/configurational-storage.md`).
//!
//! All arithmetic is checked; any state space that overflows `u128` is
//! rejected (the candidate is simply not representable in v1).

#![forbid(unsafe_code)]

pub mod coordinate;
pub mod palette;
pub mod periodic;
pub mod permutation;
pub mod rank;
pub mod residual;
pub mod sparse;
pub mod sparse64;
pub mod transform;
pub mod universe;
