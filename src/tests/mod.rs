//! Integration test modules.
//!
//! These live inside the single crate (ADR-0001) and exercise cross-module
//! invariants: representation round trips, rank round trips, the persistent
//! store, crash recovery, FUSE semantics, fsck, and ENOSPC behavior.

#![forbid(unsafe_code)]

pub mod base_sequence;
pub mod concurrency;
pub mod crash_recovery;
pub mod durability;
pub mod enospc;
pub mod fsck;
pub mod helpers;
pub mod model_bundle;
pub mod model_oracle;
pub mod namespace_ops;
pub mod optimizer;
pub mod perf_diag;
pub mod persistent_store;
pub mod physical_convergence;
pub mod rank_roundtrip;
pub mod representation_roundtrip;
pub mod seqdeep;
pub mod seqdict;
pub mod seqrans_versioned_repro;
pub mod shared_dict;
pub mod snapshots;
pub mod sparse_block64;
pub mod srctree_diag;
