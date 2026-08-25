//! Evidence discipline (§50): deterministic casefiles, crash-court
//! receipts, benchmark manifests, reproducibility records.
//!
//! Every nontrivial storage claim must be backed by a reproducible
//! artifact. serde_json is used here and only here for persistent data
//! (evidence is human-readable; the filesystem format is explicit byte
//! codecs).

#![forbid(unsafe_code)]

pub mod campaign;
pub mod casefile;
pub mod corpus;
pub mod environment;
pub mod manifest;
pub mod receipt;
