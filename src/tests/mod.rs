//! Integration test modules.
//!
//! These live inside the single crate (ADR-0001) and exercise cross-module
//! invariants: representation round trips, rank round trips, the persistent
//! store, crash recovery, FUSE semantics, fsck, and ENOSPC behavior.

#![forbid(unsafe_code)]

pub mod base_sequence;
pub mod concurrency;
pub mod court_repro;
pub mod crash_recovery;
pub mod durability;
pub mod enospc;
pub mod epoch;
pub mod epoch_self_alias;
pub mod epoch_seq_monotonic;
pub mod fsck;
pub mod fuse_epoch;
pub mod helpers;
pub mod hostile_media;
pub mod io_backend_parity;
pub mod model_bundle;
pub mod model_oracle;
pub mod namespace_ops;
pub mod namespace_repro;
pub mod optimizer;
pub mod partial_window_read;
pub mod perf_diag;
pub mod perf_reconciled;
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
pub mod split_write;
pub mod srctree_diag;
pub mod unsafe_ledger;
pub mod uring_bench;
pub mod worker_oracle;
pub mod worker_pool_probe;
pub mod write_parallel;
