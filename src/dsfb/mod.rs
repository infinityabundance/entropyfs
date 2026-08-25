//! Storage-specific DSFB observer (ADR-0004).
//!
//! DSFB has **zero decoding authority**. It may rank candidate predictors,
//! recognize persistent representation regimes, detect drift, detect slew,
//! decide how much candidate search to perform, and decide whether a
//! background re-optimization is promising. It may never alter bytes: a
//! filesystem image remains perfectly decodable if all DSFB runtime state
//! is deleted.
//!
//! The winning representation is always selected by exact deterministic
//! cost (`core::cost`); DSFB only orders the candidate search and sizes the
//! budget. If DSFB predicts poorly, the filesystem wastes CPU — never data.
//!
//! The observer core (φ/ω/α + trust weighting) is the published `dsfb`
//! crate (`docs/research/upstream-audit.md` §2); this module adapts it to
//! storage evidence.

#![forbid(unsafe_code)]

pub mod drift;
pub mod features;
pub mod observer;
pub mod selection;
pub mod slew;
pub mod trust;
