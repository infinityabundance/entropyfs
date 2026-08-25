//! Logical content integrity: the identity of *materialized* bytes.
//!
//! Logical content identity is BLAKE3 over the exact application-visible
//! bytes (§33). It is deliberately independent of the physical
//! representation: two different descriptors that materialize identical
//! bytes share one logical content id. The chunk index is keyed by this
//! id, so a descriptor that materializes to the wrong bytes is detected
//! whenever the chunk index is verified.

#![forbid(unsafe_code)]

use crate::core::extent::ChunkId;
use crate::core::limits::Limits;
use crate::core::materialize::{DecoderContext, MaterializeError, materialize_to_vec};
use crate::core::representation::Representation;

/// Compute the logical content id of materialized bytes.
pub fn logical_content_hash(bytes: &[u8]) -> ChunkId {
    ChunkId::of(bytes)
}

/// Verify materialized bytes against an expected logical content id.
pub fn verify_content(bytes: &[u8], expected: &ChunkId) -> bool {
    &logical_content_hash(bytes) == expected
}

/// Errors from content verification of a descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentVerifyError {
    /// The descriptor could not be materialized (missing object, bad
    /// reference, decode failure).
    Materialize(MaterializeError),
    /// Materialized length differs from the declared chunk length.
    LengthMismatch {
        /// Length the descriptor declares.
        declared: u64,
        /// Length actually materialized.
        actual: u64,
    },
    /// Materialized bytes hash to a different content id than the index
    /// key (logical corruption: the descriptor is wrong for its key).
    HashMismatch {
        /// The index key (expected content id).
        key: ChunkId,
        /// Content id of the actual materialized bytes.
        actual: ChunkId,
    },
}

impl std::fmt::Display for ContentVerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ContentVerifyError {}

/// Materialize `descriptor` and verify that the result is exactly
/// `len` bytes whose content id equals `key` (the chunk-index key).
///
/// This is the fsck-grade check that a valid physical record still
/// materializes to the logical bytes its index entry claims (§33).
pub fn verify_descriptor(
    descriptor: &Representation,
    key: &ChunkId,
    resolver: &dyn DecoderContext,
    limits: &Limits,
) -> Result<Vec<u8>, ContentVerifyError> {
    let bytes = materialize_to_vec(descriptor, resolver, limits)
        .map_err(ContentVerifyError::Materialize)?;
    if bytes.len() as u64 != descriptor.len() {
        return Err(ContentVerifyError::LengthMismatch {
            declared: descriptor.len(),
            actual: bytes.len() as u64,
        });
    }
    let actual = logical_content_hash(&bytes);
    if &actual != key {
        return Err(ContentVerifyError::HashMismatch { key: *key, actual });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_id_distinguishes_bytes() {
        assert_ne!(logical_content_hash(b"a"), logical_content_hash(b"b"));
        assert_eq!(logical_content_hash(b"same"), logical_content_hash(b"same"));
    }

    #[test]
    fn verify_descriptor_rejects_wrong_key() {
        let data = vec![0x42u8; 4096];
        let rep = Representation::Fill {
            value: 0x42,
            len: 4096,
        };
        let resolver = crate::tests::helpers::MemResolver::empty();
        let limits = Limits::default();
        // Correct key: passes.
        let key = ChunkId::of(&data);
        let out = verify_descriptor(&rep, &key, &resolver, &limits).unwrap();
        assert_eq!(out, data);
        // Wrong key: rejected.
        let bad = ChunkId::of(b"not the data");
        assert!(matches!(
            verify_descriptor(&rep, &bad, &resolver, &limits),
            Err(ContentVerifyError::HashMismatch { .. })
        ));
    }
}
