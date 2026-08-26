//! Narrowly isolated Linux-specific machinery.
//!
//! Per `docs/security/unsafe-ledger.md` the crate builds with zero
//! `unsafe` except the ONE designated file: [`io_uring`] (the io_uring
//! submission ring is kernel-shared memory; its SQE push is inherently
//! `unsafe`). Everything else in this module — and the whole crate — is
//! safe Rust with `#![forbid(unsafe_code)]` per module. The crate root
//! carries `#![deny(unsafe_code)]` as the backstop, and a test walks
//! `src/` asserting the set of files containing `unsafe` equals the
//! ledger's file list.

#![deny(unsafe_code)]

/// One io_uring ring with a safe submit-and-collect API (Phase 10F). The
/// crate's only `unsafe` file; see the ledger.
pub mod io_uring;

pub mod linux;
