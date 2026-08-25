//! Integration test modules.
//!
//! These live inside the single crate (ADR-0001) and exercise cross-module
//! invariants: representation round trips, rank round trips, the persistent
//! store, crash recovery, FUSE semantics, fsck, and ENOSPC behavior.

#![forbid(unsafe_code)]

pub mod helpers;
pub mod persistent_store;
pub mod rank_roundtrip;
pub mod representation_roundtrip;
