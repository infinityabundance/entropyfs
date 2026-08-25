//! Bounded deterministic reversible transforms (`T` in the defining
//! equation `X = T(E(U, S, P)) ⊕ R`).
//!
//! v1 registry: only [`TransformId::Identity`]. Any transform added later
//! must be deterministic, bounded, reversible, and format-feature-gated.

#![forbid(unsafe_code)]

use crate::core::representation::TransformId;

/// Apply a transform to bytes. v1: identity only; unknown transforms are a
/// typed error (never a panic).
pub fn apply(transform: TransformId, data: &[u8]) -> Result<Vec<u8>, TransformError> {
    match transform {
        TransformId::Identity => Ok(data.to_vec()),
    }
}

/// Transform application errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformError {
    /// The transform id is not in the format registry.
    UnknownTransform,
}

impl std::fmt::Display for TransformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for TransformError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity() {
        let data = b"abc";
        assert_eq!(apply(TransformId::Identity, data).unwrap(), b"abc");
    }
}
