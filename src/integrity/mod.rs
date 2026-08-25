//! Integrity primitives (ADR-0011, §33): logical content hashes, physical
//! record integrity, root integrity.
//!
//! The three concepts are deliberately distinct:
//!
//! - [`content`] — logical content hash: BLAKE3 over materialized bytes;
//!   identical for different physical representations of the same bytes.
//! - [`record`] — physical record integrity: envelope CRC32C + embedded
//!   content id of the stored payload.
//! - [`root`] — root integrity: the superblock-to-root-object binding and
//!   generation agreement.
//!
//! fsck composes all three into the full validation chain.

#![forbid(unsafe_code)]

pub mod content;
pub mod record;
pub mod root;
