//! Root integrity: the filesystem root object and its superblock binding.
//!
//! A committed root must satisfy: `root_object_id == BLAKE3(root_payload)`
//! and `superblock.generation == root.generation`. These two bindings make
//! the superblock slot self-authenticating for the root record it points
//! to (§18, §33). fsck verifies them for every valid slot and for every
//! snapshot root.

#![forbid(unsafe_code)]

use crate::core::extent::ChunkId;
use crate::format::superblock::Superblock;
use crate::store::root::Root;

/// Errors from root verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootVerifyError {
    /// The root payload decodes but its content id differs from the
    /// superblock's `root_object_id`.
    RootIdMismatch {
        /// The superblock's expected id.
        expected: ChunkId,
        /// The actual payload hash.
        actual: ChunkId,
    },
    /// The root payload is structurally malformed.
    RootDecode,
    /// The superblock generation does not match the root generation.
    GenerationMismatch {
        /// Superblock generation.
        sb: u64,
        /// Root generation.
        root: u64,
    },
    /// The root object record is not present in the object index.
    RootObjectMissing,
}

impl std::fmt::Display for RootVerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for RootVerifyError {}

/// The root payload bytes must hash to the superblock's root object id.
pub fn verify_root_payload(sb: &Superblock, root_payload: &[u8]) -> Result<(), RootVerifyError> {
    let actual = ChunkId::of(root_payload);
    if actual != sb.root_object_id {
        return Err(RootVerifyError::RootIdMismatch {
            expected: sb.root_object_id,
            actual,
        });
    }
    Ok(())
}

/// Decode and verify a root payload against its superblock binding.
pub fn verify_root(sb: &Superblock, root_payload: &[u8]) -> Result<Root, RootVerifyError> {
    verify_root_payload(sb, root_payload)?;
    let root = Root::decode(root_payload).map_err(|_| RootVerifyError::RootDecode)?;
    if root.generation != sb.generation {
        return Err(RootVerifyError::GenerationMismatch {
            sb: sb.generation,
            root: root.generation,
        });
    }
    Ok(root)
}

/// Verify a snapshot root object payload (no superblock binding; the
/// snapshot entry holds the root id).
pub fn verify_snapshot_root(
    expected_id: &ChunkId,
    root_payload: &[u8],
) -> Result<Root, RootVerifyError> {
    let actual = ChunkId::of(root_payload);
    if &actual != expected_id {
        return Err(RootVerifyError::RootIdMismatch {
            expected: *expected_id,
            actual,
        });
    }
    Root::decode(root_payload).map_err(|_| RootVerifyError::RootDecode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_payload_binding() {
        let root = Root {
            generation: 7,
            ..Default::default()
        };
        let payload = root.encode();
        let mut sb = Superblock {
            generation: 7,
            ..Default::default()
        };
        sb.root_object_id = ChunkId::of(&payload);
        assert!(verify_root(&sb, &payload).is_ok());
        // Generation mismatch is caught.
        sb.generation = 8;
        assert!(matches!(
            verify_root(&sb, &payload),
            Err(RootVerifyError::GenerationMismatch { .. })
        ));
    }

    #[test]
    fn id_mismatch_detected() {
        let root = Root::default();
        let payload = root.encode();
        let sb = Superblock {
            root_object_id: ChunkId::of(b"other"),
            ..Default::default()
        };
        assert!(matches!(
            verify_root(&sb, &payload),
            Err(RootVerifyError::RootIdMismatch { .. })
        ));
    }
}
