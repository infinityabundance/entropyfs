//! # EntropyFS
//!
//! Entropy-native Linux filesystem: **persist irreducible state, materialize
//! structure, preserve exact bytes, measure everything.**
//!
//! This crate is the entire EntropyFS implementation — one package, one
//! architectural source tree (see `docs/adr/0001-single-crate.md`).
//!
//! # Safety
//!
//! `#![forbid(unsafe_code)]` is crate-wide. Any future exception is confined
//! to `platform` and must be recorded in `docs/security/unsafe-ledger.md`.
#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

// Core: pure deterministic representation algebra. Knows nothing about
// FUSE, disk, or DSFB.
pub mod core;

// Mathematical/configurational storage machinery (rank/unrank, universes,
// transforms, residuals).
pub mod entropy;

// Thin adaptation layer over the `ryg-rans-rs` dependency.
pub mod rans;

// Storage-specific DSFB observer (zero decoding authority).
pub mod dsfb;

// Permanent on-disk format: explicit little-endian byte codecs.
pub mod format;

// Crash-consistent persistent immutable object store.
pub mod store;

// Linux POSIX/VFS adapter (FUSE). No entropy algorithms here.
pub mod fuse;

// Representation search and migration.
pub mod optimizer;

// Bounded performance-only caches.
pub mod cache;

// Integrity primitives: logical content, physical record, root.
pub mod integrity;

// Independent validation and repair.
pub mod fsck;

// Evidence: casefiles, receipts, manifests.
pub mod evidence;

// Narrowly isolated Linux-specific machinery.
pub mod platform;

// Experimental ublk block-device frontend (Phase 7; requires root +
// CONFIG_BLK_DEV_UBLK to run; the BlockStore adapter is kernel-free).
pub mod ublk;

#[cfg(test)]
mod tests;
