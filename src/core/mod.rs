//! Pure deterministic representation algebra.
//!
//! `core` owns: logical extent definitions, representation descriptors,
//! materialization, generator contracts, rank/unrank algorithms, transform
//! definitions, residual application, representation cost accounting,
//! bounded recursion/graph evaluation, exact byte reconstruction.
//!
//! It must know nothing about FUSE, the store, or DSFB. It may use external
//! dependencies (`blake3` for content identity), but never the disk format.

#![forbid(unsafe_code)]

pub mod candidate;
pub mod cost;
pub mod extent;
pub mod limits;
pub mod materialize;
pub mod representation;
