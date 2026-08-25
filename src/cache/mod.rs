//! Bounded performance-only caches (ADR-0014, §26): materialized chunks,
//! metadata, and decoded models.
//!
//! Every cache has an explicit memory budget and is **never**
//! authoritative: dropping all caches affects only performance, never
//! correctness. Keys are immutable content ids or inode numbers whose
//! backing objects are content-addressed.

#![forbid(unsafe_code)]

pub mod materialized;
pub mod metadata;
pub mod model;
