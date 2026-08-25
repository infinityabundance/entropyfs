//! Narrowly isolated Linux-specific machinery (safe Rust only).
//!
//! Per `docs/security/unsafe-ledger.md` the crate currently builds with
//! zero `unsafe`; if a future Linux boundary genuinely requires it, it
//! belongs here with a ledger entry.

#![forbid(unsafe_code)]

pub mod linux;
