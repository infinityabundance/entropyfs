//! rANS adaptation layer over the `ryg-rans-rs` dependency
//! (ADR-0003): canonical model construction, model identity,
//! serialization for EntropyFS, candidate modes, residual encoding,
//! descriptor-stream encoding, runtime backend selection, exact cost
//! measurement.
//!
//! This is a thin layer: the coder logic lives in `ryg-rans-rs`; we never
//! fork it.

#![forbid(unsafe_code)]

pub mod dispatch;
pub mod metadata;
pub mod model;
pub mod residual;
pub mod sequence;
