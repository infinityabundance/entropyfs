//! Hard resource limits for representation decode/encode.
//!
//! Every limit is enforced **before** the allocation or loop it guards
//! (`docs/security/resource-bounds.md`). Limits never come from disk fields.
//!
//! # PURPOSE
//!
//! The resource-bounds contract for the representation engine and parsers:
//! the single place that defines how large anything derived from
//! untrusted persisted data may become, and how much work decoding may
//! spend.
//!
//! # BOUNDARY
//!
//! Compile-time defaults live here; the values are configurable at
//! mkfs/mount. Limits are never read from disk — a persisted length field
//! is data, not an allocation authority (commentary standard §1's
//! ordering rule: bounded parse → validation → resource preflight →
//! materialization).
//!
//! # MODEL — which field bounds what
//!
//! - [`Limits::max_chunk_size`] — bytes; bounds **every materialization**:
//!   the largest logical chunk class (256 KiB default). No decode output
//!   may exceed it.
//! - [`Limits::max_decode_work`] — a deterministic **operation budget**
//!   for a single materialization; every decode step decrements the
//!   counter (64 Mi operations default). This is the CPU bound.
//! - [`Limits::max_reference_depth`] — the **chain cap** in levels
//!   (EXACT_REF / BASE_RESIDUAL / dictionary chains; default 4), enforced
//!   against the longest path (the graph court's "diamonds" attack).
//! - [`Limits::max_descriptor_bytes`] — bytes; the **descriptor codec**
//!   parse bound (8192 default): an encoded descriptor never exceeds it.
//! - [`Limits::max_alloc_bytes`] — bytes; any single decode allocation
//!   derived from persisted data (1 MiB default).
//! - [`Limits::max_fanout`] — a count; residual edit count / B-tree node
//!   fanout (4096 default).
//! - [`Limits::max_model_bytes`] / [`Limits::max_inline_bytes`] — bytes;
//!   the rANS model object and the INLINE representation caps.
//! - [`Limits::max_period`] / [`Limits::max_palette`] — family parameter
//!   caps for PERIODIC and PALETTE.
//!
//! These are the **hostile-media court's allocation gates**: the Phase-11A
//! courts prove "typed error, never panic, never OOM, never unbounded
//! CPU" against exactly these bounds (`docs/security/hostile-media-
//! court.md`; `docs/security/resource-bounds.md` §6).
//!
//! # PERSISTENT AUTHORITY
//!
//! Indirectly: the format's boundedness (ADR-0005 hard rules — max output
//! size, max encoded size, deterministic operation budget, bounded
//! reference depth, bounded memory) is defined by these limits, and they
//! are format-policy controlled.
//!
//! # CORRECTNESS INVARIANTS
//!
//! - every limit is enforced **before** the allocation or loop it guards;
//! - limits never come from disk fields;
//! - defaults are consistent (e.g. `max_chunk_size ≥ chunk_class`,
//!   `max_reference_depth ≥ 1` — pinned by `defaults_are_consistent`).
//!
//! # CONCURRENCY
//!
//! Plain `Copy` data; no locks; shared read-only across threads.
//!
//! # RESOURCE BOUNDS
//!
//! The limits themselves are the gates; the defaults are conservative
//! (`docs/security/resource-bounds.md`).
//!
//! # FAILURE MODES
//!
//! Exceeding a limit is a typed rejection path (parse error /
//! materialization error), never a panic, OOM, or unbounded CPU — exactly
//! the property the hostile-media court (Phase-11A) proves and seals
//! (`evidence/hostile-media/court-1787750784-a2983dc/`; ADR-0016).
//!
//! # HISTORY / EVIDENCE
//!
//! `docs/security/resource-bounds.md` (the authoritative table);
//! Phase-11A hostile-media court (the enforcement proof); ADR-0016
//! (bounded-valid-result-or-typed-rejection oracle).

#![forbid(unsafe_code)]

/// Default maximum logical chunk size: 256 KiB (largest supported class).
pub const DEFAULT_MAX_CHUNK_SIZE: u64 = 256 * 1024;
/// Default write chunk class: 64 KiB (`docs/adr/0006-chunk-classes.md`).
pub const DEFAULT_CHUNK_CLASS: u64 = 64 * 1024;
/// Default maximum encoded descriptor size (bytes; the descriptor codec
/// parse bound).
pub const DEFAULT_MAX_DESCRIPTOR_BYTES: u64 = 8192;
/// Default maximum reference depth (chain levels; base chains /
/// exact-ref / dictionary chains).
pub const DEFAULT_MAX_REFERENCE_DEPTH: u8 = 4;
/// Default maximum decode work budget (deterministic operation counter;
/// every materialize step decrements).
pub const DEFAULT_MAX_DECODE_WORK: u64 = 1 << 26;
/// Default maximum single decode allocation (bytes).
pub const DEFAULT_MAX_ALLOC_BYTES: u64 = 1024 * 1024;
/// Default maximum residual edit count / node fanout (count).
pub const DEFAULT_MAX_FANOUT: u32 = 4096;
/// Default maximum rANS model object size (bytes).
pub const DEFAULT_MAX_MODEL_BYTES: u64 = 2048;
/// Default maximum INLINE literal size (bytes).
pub const DEFAULT_MAX_INLINE_BYTES: u64 = 4096;
/// Maximum periodic pattern length (bytes).
pub const DEFAULT_MAX_PERIOD: u32 = 1024;
/// Maximum palette cardinality (count).
pub const DEFAULT_MAX_PALETTE: usize = 16;

/// Chunk classes supported by the format (ADR-0006), in bytes: the four
/// v1 classes 4 / 16 / 64 / 256 KiB.
pub const CHUNK_CLASSES: [u64; 4] = [4 * 1024, 16 * 1024, 64 * 1024, 256 * 1024];

/// Resource limits enforced by the representation engine and parsers.
///
/// These are the hostile-media court's allocation gates: the Phase-11A
/// courts prove typed-error (never panic/OOM/unbounded CPU) behavior
/// against exactly these bounds (`docs/security/hostile-media-court.md`;
/// `docs/security/resource-bounds.md` §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest logical chunk class (bytes; bounds every materialization).
    pub max_chunk_size: u64,
    /// Default write chunk class (bytes).
    pub chunk_class: u64,
    /// Maximum encoded representation descriptor size (bytes; the codec
    /// parse bound).
    pub max_descriptor_bytes: u64,
    /// Maximum reference depth (chain levels; EXACT_REF / BASE_RESIDUAL /
    /// dictionary chains, capped against the longest path).
    pub max_reference_depth: u8,
    /// Deterministic operation budget for a single materialization
    /// (operations; every decode step decrements).
    pub max_decode_work: u64,
    /// Maximum single allocation derived from persisted data (bytes).
    pub max_alloc_bytes: u64,
    /// Maximum edit count / fanout in residuals and B-tree nodes (count).
    pub max_fanout: u32,
    /// Maximum rANS model object size (bytes).
    pub max_model_bytes: u64,
    /// Maximum INLINE representation size (bytes).
    pub max_inline_bytes: u64,
    /// Maximum periodic pattern length (bytes).
    pub max_period: u32,
    /// Maximum palette cardinality (count).
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
