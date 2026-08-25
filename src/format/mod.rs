//! Permanent on-disk format: explicit little-endian byte codecs
//! (ADR-0012, `docs/format/ondisk-v1.md`).

#![forbid(unsafe_code)]

pub mod codec;
pub mod descriptor;
pub mod features;
pub mod record;
pub mod superblock;
pub mod version;

pub use codec::{CodecError, Reader, Writer};
