//! Hard resource limits for representation decode/encode.
//!
//! Every limit is enforced **before** the allocation or loop it guards
//! (`docs/security/resource-bounds.md`). Limits never come from disk fields.

#![forbid(unsafe_code)]

/// Default maximum logical chunk size: 256 KiB (largest supported class).
pub const DEFAULT_MAX_CHUNK_SIZE: u64 = 256 * 1024;
/// Default write chunk class: 64 KiB (`docs/adr/0006-chunk-classes.md`).
pub const DEFAULT_CHUNK_CLASS: u64 = 64 * 1024;
/// Default maximum encoded descriptor size.
pub const DEFAULT_MAX_DESCRIPTOR_BYTES: u64 = 8192;
/// Default maximum reference depth (base chains / exact-ref chains).
pub const DEFAULT_MAX_REFERENCE_DEPTH: u8 = 4;
/// Default maximum decode work budget (operation counter).
pub const DEFAULT_MAX_DECODE_WORK: u64 = 1 << 26;
/// Default maximum single decode allocation.
pub const DEFAULT_MAX_ALLOC_BYTES: u64 = 1024 * 1024;
/// Default maximum residual edit count / node fanout.
pub const DEFAULT_MAX_FANOUT: u32 = 4096;
/// Default maximum rANS model object size.
pub const DEFAULT_MAX_MODEL_BYTES: u64 = 2048;
/// Default maximum INLINE literal size.
pub const DEFAULT_MAX_INLINE_BYTES: u64 = 4096;
/// Maximum periodic pattern length.
pub const DEFAULT_MAX_PERIOD: u32 = 1024;
/// Maximum palette cardinality.
pub const DEFAULT_MAX_PALETTE: usize = 16;

/// Chunk classes supported by the format (ADR-0006).
pub const CHUNK_CLASSES: [u64; 4] = [4 * 1024, 16 * 1024, 64 * 1024, 256 * 1024];

/// Resource limits enforced by the representation engine and parsers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest logical chunk class (bounds every materialization).
    pub max_chunk_size: u64,
    /// Default write chunk class.
    pub chunk_class: u64,
    /// Maximum encoded representation descriptor size.
    pub max_descriptor_bytes: u64,
    /// Maximum reference depth (EXACT_REF / BASE_RESIDUAL chains).
    pub max_reference_depth: u8,
    /// Deterministic operation budget for a single materialization.
    pub max_decode_work: u64,
    /// Maximum single allocation derived from persisted data.
    pub max_alloc_bytes: u64,
    /// Maximum edit count / fanout in residuals and B-tree nodes.
    pub max_fanout: u32,
    /// Maximum rANS model object size.
    pub max_model_bytes: u64,
    /// Maximum INLINE representation size.
    pub max_inline_bytes: u64,
    /// Maximum periodic pattern length.
    pub max_period: u32,
    /// Maximum palette cardinality.
    pub max_palette: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_chunk_size: DEFAULT_MAX_CHUNK_SIZE,
            chunk_class: DEFAULT_CHUNK_CLASS,
            max_descriptor_bytes: DEFAULT_MAX_DESCRIPTOR_BYTES,
            max_reference_depth: DEFAULT_MAX_REFERENCE_DEPTH,
            max_decode_work: DEFAULT_MAX_DECODE_WORK,
            max_alloc_bytes: DEFAULT_MAX_ALLOC_BYTES,
            max_fanout: DEFAULT_MAX_FANOUT,
            max_model_bytes: DEFAULT_MAX_MODEL_BYTES,
            max_inline_bytes: DEFAULT_MAX_INLINE_BYTES,
            max_period: DEFAULT_MAX_PERIOD,
            max_palette: DEFAULT_MAX_PALETTE,
        }
    }
}

impl Limits {
    /// Returns the chunk class equal to `len`, or `None` if `len` is not a
    /// supported chunk class.
    pub fn class_for_len(&self, len: u64) -> Option<u64> {
        CHUNK_CLASSES.iter().copied().find(|&c| c == len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_consistent() {
        let l = Limits::default();
        assert!(l.max_chunk_size >= l.chunk_class);
        assert!(l.max_fanout > 0);
        assert!(l.max_reference_depth >= 1);
        assert_eq!(l.class_for_len(64 * 1024), Some(64 * 1024));
        assert_eq!(l.class_for_len(12345), None);
    }
}
