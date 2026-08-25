//! Narrowly isolated Linux-specific machinery. The only module permitted
//! to hold `unsafe` (see `docs/security/unsafe-ledger.md`); currently the
//! crate builds with zero unsafe code, so this module is safe-only for now.

#![forbid(unsafe_code)]

// (module populated by the platform implementation step)
