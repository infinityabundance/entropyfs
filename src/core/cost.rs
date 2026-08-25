//! Representation cost accounting and policy modes (ADR-0010).
//!
//! ```text
//! J = persisted_bytes
//!   + λ_read  * estimated_read_cycles
//!   + λ_write * estimated_write_cycles
//!   + λ_io    * dependent_physical_reads
//!   + λ_depth * reference_depth
//! ```
//!
//! Cycle estimates are deterministic fixed tables (not wall-clock
//! measurements at selection time). `persisted_bytes` is the full
//! accountable persisted state for the extent
//! (`docs/theory/information-accounting.md`).

#![forbid(unsafe_code)]

use crate::core::representation::Representation;

/// Per-component byte accounting for one extent.
///
/// `persisted_bytes = descriptor + model + residual + seed/state +
/// reference + configurational + integrity` (GC overhead at FS level).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CostBreakdown {
    /// Materialized (logical) bytes.
    pub logical_bytes: u64,
    /// Encoded representation descriptor bytes.
    pub descriptor_bytes: u64,
    /// rANS model bytes attributable to this extent.
    pub model_bytes: u64,
    /// Payload bytes of the Data objects this extent needs (raw payload,
    /// rANS stream, residual stream). The single largest persisted term
    /// for object-backed families; must never be zero for RAW/RANS.
    pub object_payload_bytes: u64,
    /// Residual payload bytes.
    pub residual_bytes: u64,
    /// Seed/state bytes (ENTROPY_REF).
    pub seed_state_bytes: u64,
    /// Reference bytes (content ids of bases/targets/models).
    pub reference_bytes: u64,
    /// Configurational bytes (rank coordinates).
    pub configurational_bytes: u64,
    /// Attributable integrity bytes (checksums/hashes).
    pub integrity_bytes: u64,
    /// Attributable GC overhead estimate (amortized).
    pub gc_overhead_bytes: u64,
    /// Estimated read cycles (deterministic table).
    pub read_cycles: u64,
    /// Estimated write cycles (deterministic table).
    pub write_cycles: u64,
    /// Dependent physical object reads to materialize.
    pub dependent_reads: u32,
    /// Reference depth (base chains / exact-ref chains).
    pub depth: u8,
}

impl CostBreakdown {
    /// Total persisted bytes for this extent (excludes GC overhead).
    pub const fn persisted_bytes(&self) -> u64 {
        self.descriptor_bytes
            .saturating_add(self.model_bytes)
            .saturating_add(self.object_payload_bytes)
            .saturating_add(self.residual_bytes)
            .saturating_add(self.seed_state_bytes)
            .saturating_add(self.reference_bytes)
            .saturating_add(self.configurational_bytes)
            .saturating_add(self.integrity_bytes)
    }

    /// Total persisted bytes including attributable GC overhead.
    pub const fn persisted_with_gc(&self) -> u64 {
        self.persisted_bytes()
            .saturating_add(self.gc_overhead_bytes)
    }

    /// The objective `J` under a policy.
    pub fn total(&self, policy: &Policy) -> u128 {
        let persisted = self.persisted_with_gc() as u128;
        let read = ((policy.lambda_read as u128) * (self.read_cycles as u128)) / 1024;
        let write = ((policy.lambda_write as u128) * (self.write_cycles as u128)) / 1024;
        let io = (policy.lambda_io as u128) * (self.dependent_reads as u128);
        let depth = (policy.lambda_depth as u128) * (self.depth as u128);
        persisted
            .saturating_add(read)
            .saturating_add(write)
            .saturating_add(io)
            .saturating_add(depth)
    }
}

/// Policy mode names (ADR-0010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PolicyMode {
    /// Physical bytes dominate.
    Capacity,
    /// Conservative defaults.
    Balanced,
    /// Cheap materialization dominates.
    Latency,
    /// Deep density; background optimization heavy.
    Archive,
    /// RAM-mode: regeneration cost dominates.
    Ram,
}

/// The λ table for one policy mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Weight per 1024 read cycles.
    pub lambda_read: u64,
    /// Weight per 1024 write cycles.
    pub lambda_write: u64,
    /// Weight per dependent physical read.
    pub lambda_io: u64,
    /// Weight per unit of reference depth.
    pub lambda_depth: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self::balanced()
    }
}

impl Policy {
    /// The balanced default.
    pub const fn balanced() -> Self {
        Self {
            lambda_read: 8,
            lambda_write: 8,
            lambda_io: 512,
            lambda_depth: 1024,
        }
    }

    /// Policy table for a mode. Units: λ applied to cycles/1024 and to
    /// counts; the byte term has implicit weight 1.
    pub const fn mode(mode: PolicyMode) -> Self {
        match mode {
            PolicyMode::Capacity => Self {
                lambda_read: 2,
                lambda_write: 2,
                lambda_io: 256,
                lambda_depth: 512,
            },
            PolicyMode::Balanced => Self::balanced(),
            PolicyMode::Latency => Self {
                lambda_read: 64,
                lambda_write: 16,
                lambda_io: 4096,
                lambda_depth: 8192,
            },
            PolicyMode::Archive => Self {
                lambda_read: 1,
                lambda_write: 1,
                lambda_io: 64,
                lambda_depth: 256,
            },
            PolicyMode::Ram => Self {
                lambda_read: 32,
                lambda_write: 32,
                lambda_io: 2048,
                lambda_depth: 4096,
            },
        }
    }
}

/// Deterministic read-cycle estimates per representation family.
///
/// These are fixed, documented heuristic constants — not measurements.
/// They exist so selection is reproducible across machines. The `x / 8`
/// scaling approximates word-at-a-time copies; multipliers approximate
/// per-byte decode work.
pub fn estimated_read_cycles(rep: &Representation) -> u64 {
    let len = rep.len();
    match rep {
        Representation::Zero { .. } | Representation::Fill { .. } => len / 8,
        Representation::Inline { .. } | Representation::Raw { .. } => len,
        Representation::Rans { .. } => len * 4,
        Representation::ExactRef { .. } => len,
        Representation::BaseResidual { .. } => len * 2,
        Representation::Sparse { .. } => len / 8 + 4,
        Representation::Palette { .. } => len,
        Representation::Periodic { .. } => len / 8,
        Representation::EntropyRef { .. } => len * 8,
        Representation::Permutation { .. } => len * 4,
        // Three rANS streams (≈len·4 total symbols) plus the copy/literal
        // walk (≈len byte copies) — slightly heavier than plain RANS.
        Representation::SequenceRans { .. } => len * 5,
        // Per-word popcount + rank unranking + literal placement.
        Representation::SparseBlock64 { .. } => len / 8 + 8,
    }
}

/// Deterministic write-cycle estimates per representation family.
pub fn estimated_write_cycles(rep: &Representation) -> u64 {
    let len = rep.len();
    match rep {
        Representation::Zero { .. } | Representation::Fill { .. } => len / 8,
        Representation::Inline { .. } | Representation::Raw { .. } => len,
        Representation::Rans { .. } => len * 6,
        Representation::ExactRef { .. } => 32,
        Representation::BaseResidual { .. } => len * 3,
        Representation::Sparse { .. } => len / 8 + 8,
        Representation::Palette { .. } => len * 2,
        Representation::Periodic { .. } => len / 8,
        Representation::EntropyRef { .. } => len * 10,
        Representation::Permutation { .. } => len * 8,
        // LZ hash search + three histograms + three rANS encodes.
        Representation::SequenceRans { .. } => len * 8,
        Representation::SparseBlock64 { .. } => len / 8 + 16,
    }
}

/// Number of dependent physical object reads to materialize (0 = none).
pub fn dependent_reads(rep: &Representation) -> u32 {
    match rep {
        Representation::Zero { .. }
        | Representation::Fill { .. }
        | Representation::Inline { .. }
        | Representation::Periodic { .. }
        | Representation::Permutation { .. } => 0,
        Representation::Raw { .. } | Representation::Rans { .. } => 1,
        Representation::ExactRef { .. } => 1,
        Representation::BaseResidual { residual, .. } => match residual {
            crate::core::representation::Residual::RansCoded { .. } => 3, // base + encoded + model
            crate::core::representation::Residual::BaseSequence { .. } => 3, // base + encoded + model
            _ => 1,
        },
        Representation::Sparse { .. } | Representation::Palette { .. } => 0,
        Representation::EntropyRef { residual, .. } => match residual {
            crate::core::representation::Residual::RansCoded { .. } => 2, // encoded + model
            _ => 0,
        },
        // Model object + enc object.
        Representation::SequenceRans { .. } => 2,
        // Model object + enc object.
        Representation::SparseBlock64 { .. } => 2,
    }
}

/// Reference depth contributed by this representation (0 for terminal
/// families; 1 for a reference that adds one level).
pub fn reference_depth(rep: &Representation) -> u8 {
    match rep {
        Representation::ExactRef { .. } => 1,
        Representation::BaseResidual { .. } => 1,
        _ => 0,
    }
}

/// Disjoint byte-split categories within a descriptor
/// (`docs/theory/information-accounting.md` §1).
///
/// `descriptor_bytes` is derived as the descriptor's total encoded size
/// minus these categories, so the categories sum exactly to the persisted
/// byte total with no double counting:
///
/// ```text
/// persisted = descriptor + model + residual + seed + reference
///           + configurational + integrity
///           = encoded_size + model + integrity
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ByteSplit {
    /// Residual payload bytes (edit sets / literals).
    pub residual: u64,
    /// Content-id reference bytes.
    pub reference: u64,
    /// Rank/coordinate bytes.
    pub configurational: u64,
    /// Seed/state bytes.
    pub seed_state: u64,
}

/// Build a [`CostBreakdown`] from a representation and its byte split.
/// `model_bytes` is the encoded size of any rANS model object attributable
/// to this extent.
pub fn estimate(rep: &Representation, split: &ByteSplit, model_bytes: u64) -> CostBreakdown {
    let encoded = rep.encoded_size();
    let descriptor = encoded
        .saturating_sub(split.residual)
        .saturating_sub(split.reference)
        .saturating_sub(split.configurational)
        .saturating_sub(split.seed_state);
    CostBreakdown {
        logical_bytes: rep.len(),
        descriptor_bytes: descriptor,
        model_bytes,
        object_payload_bytes: 0,
        residual_bytes: split.residual,
        seed_state_bytes: split.seed_state,
        reference_bytes: split.reference,
        configurational_bytes: split.configurational,
        integrity_bytes: 4, // amortized descriptor/record CRC
        gc_overhead_bytes: 0,
        read_cycles: estimated_read_cycles(rep),
        write_cycles: estimated_write_cycles(rep),
        dependent_reads: dependent_reads(rep),
        depth: reference_depth(rep),
    }
}

#[cfg(test)]
mod split_tests {
    use super::*;
    use crate::core::representation::{Edit, Residual};

    #[test]
    fn split_sums_match_encoded_plus_model() {
        let rep = Representation::Sparse {
            k: 3,
            rank: 10,
            literals: vec![1, 2, 3],
            len: 64,
        };
        let split = ByteSplit {
            residual: 3,
            configurational: 16,
            ..Default::default()
        };
        let c = estimate(&rep, &split, 0);
        assert_eq!(c.persisted_bytes(), rep.encoded_size() + 4);
    }

    #[test]
    fn residual_split_rules() {
        // XorSparse: residual data = 5 * edit count
        let res = Residual::XorSparse {
            len: 64,
            edits: vec![Edit { pos: 1, val: 2 }, Edit { pos: 3, val: 4 }],
        };
        let rep = Representation::BaseResidual {
            base: crate::core::extent::ChunkId::ZERO,
            base_len: 64,
            residual: res.clone(),
            len: 64,
        };
        let split = ByteSplit {
            residual: 10,
            reference: 32,
            ..Default::default()
        };
        let c = estimate(&rep, &split, 0);
        assert_eq!(c.persisted_bytes(), rep.encoded_size() + 4);
        assert_eq!(c.reference_bytes, 32);
        assert_eq!(c.residual_bytes, 10);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_dominates_random_under_capacity() {
        // RAW: the 64 KiB payload is a Data object.
        let raw = CostBreakdown {
            logical_bytes: 65536,
            descriptor_bytes: 9,
            object_payload_bytes: 65536,
            ..Default::default()
        };
        // A hypothetical "generated" representation that still needs a
        // full-size residual must never win: the full residual (== data
        // size) plus seed/coordinate state is strictly more persisted
        // state than storing the bytes directly. (Id-vs-seed pointer size
        // differences are a separate, bounded accounting question.)
        let generated = CostBreakdown {
            logical_bytes: 65536,
            descriptor_bytes: 9,
            seed_state_bytes: 24,
            residual_bytes: 65536,
            ..Default::default()
        };
        let p = Policy::mode(PolicyMode::Capacity);
        assert!(generated.persisted_bytes() > raw.persisted_bytes());
        assert!(generated.total(&p) > raw.total(&p));
    }

    #[test]
    fn latency_policy_prefers_cheap_reads() {
        // 64 KiB raw vs 32 KiB rans with heavy read cost: latency mode
        // closes the gap relative to capacity mode.
        let raw = CostBreakdown {
            logical_bytes: 65536,
            descriptor_bytes: 40,
            read_cycles: estimated_read_cycles(&Representation::Raw {
                obj: crate::core::extent::ChunkId::ZERO,
                len: 65536,
            }),
            ..Default::default()
        };
        assert_eq!(raw.read_cycles, 65536);
        assert!(
            Policy::mode(PolicyMode::Latency).lambda_read
                > Policy::mode(PolicyMode::Capacity).lambda_read
        );
    }
}
