//! Logical extent definitions and content identity.
//!
//! A [`ChunkId`] is the 256-bit BLAKE3 logical content hash of a chunk's
//! *materialized* bytes (ADR-0011). Two different physical representations
//! of the same logical bytes MUST have the same [`ChunkId`].
//!
//! # PURPOSE
//!
//! Content identity ([`ChunkId`]) and logical extent geometry ([`Extent`]) —
//! the two value types every other core module builds on.
//!
//! # BOUNDARY
//!
//! Knows nothing about representations, encoding, the store, or the disk
//! format. Pure value types with parsing helpers.
//!
//! # MODEL
//!
//! `ChunkId` = BLAKE3-256 over the *materialized logical bytes* — identity
//! is representation-independent, so a rewrite that changes the physical
//! encoding must not change the id. `ChunkId::ZERO` is the "none"
//! sentinel. `Extent` is a half-open interval `[offset, offset+length)`
//! within a file, in bytes.
//!
//! # PERSISTENT AUTHORITY
//!
//! Yes: content ids are persisted in descriptors (references, bases,
//! models, enc objects), in the chunk index, and as the integrity anchor
//! — the store authenticates bytes against the id (ADR-0011), and dedup
//! keys on it. `Extent` offsets/lengths appear in the extent tree.
//!
//! # CORRECTNESS INVARIANTS
//!
//! - the id is a function of the materialized bytes only: same bytes ⇒
//!   same id (BLAKE3 collision resistance);
//! - `is_zero` is a sentinel, never a real content hash (`ChunkId::of`
//!   cannot produce it except by collision, which is assumed infeasible);
//! - extent end arithmetic is checked: `[offset, offset+length)` must not
//!   wrap (validated at parse time; [`Extent::end`] re-checks).
//!
//! # CONCURRENCY
//!
//! Immutable `Copy` values; no locks; shared freely across threads.
//!
//! # RESOURCE BOUNDS
//!
//! Fixed 32-byte ids; parsing accepts exactly 32 raw bytes or 64 hex
//! characters — nothing else.
//!
//! # FAILURE MODES
//!
//! [`ChunkId::from_bytes`] / [`ChunkId::from_hex`] return `None` on
//! malformed input; [`Extent::end`] returns `None` on overflow (and
//! `contains` then correctly answers false).
//!
//! # HISTORY / EVIDENCE
//!
//! ADR-0011 (integrity: the content id is the authenticated-bytes
//! anchor); `docs/format/ondisk-v1.md` (ids and extents on disk).

#![forbid(unsafe_code)]

use std::fmt;

/// 256-bit content identifier: BLAKE3 over materialized logical bytes.
///
/// Used for: logical content identity (dedup, references), structural
/// object identity in the store, and integrity verification.
///
/// Invariants: the id depends only on the materialized bytes, so physical
/// representation changes never change identity; [`ChunkId::ZERO`] is a
/// reserved sentinel that `ChunkId::of` cannot produce for real content.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkId(pub [u8; 32]);

impl ChunkId {
    /// The all-zero id ("none").
    pub const ZERO: ChunkId = ChunkId([0u8; 32]);

    /// Create from a byte array.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// True if this is the all-zero sentinel.
    pub const fn is_zero(&self) -> bool {
        let mut i = 0;
        while i < 32 {
            if self.0[i] != 0 {
                return false;
            }
            i += 1;
        }
        true
    }

    /// Inner bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Compute the content id of a byte slice (BLAKE3-256).
    pub fn of(data: &[u8]) -> Self {
        Self(blake3::hash(data).into())
    }

    /// Parse from exactly 32 raw bytes.
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        b.try_into().ok().map(Self)
    }

    /// Parse from 64 hex characters (case-insensitive).
    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks_exact(2).enumerate() {
            let hi = hex_val(chunk[0])?;
            let lo = hex_val(chunk[1])?;
            out[i] = (hi << 4) | lo;
        }
        Some(Self(out))
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

impl fmt::Display for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChunkId({})", hex_prefix(&self.0, 12))
    }
}

fn hex_prefix(bytes: &[u8; 32], n: usize) -> String {
    let mut s = String::with_capacity(n * 2 + 3);
    for b in &bytes[..n] {
        s.push_str(&format!("{b:02x}"));
    }
    s.push_str("...");
    s
}

/// A logical extent: `[offset, offset+length)` within a file — a
/// half-open interval in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Extent {
    /// Logical offset of the extent start.
    pub offset: u64,
    /// Logical length of the extent.
    pub length: u64,
}

impl Extent {
    /// Create a new extent. The caller guarantees `offset + length` does
    /// not overflow (validated at parse time); callers that need the
    /// checked end use [`Extent::end`].
    pub const fn new(offset: u64, length: u64) -> Self {
        Self { offset, length }
    }

    /// End offset (exclusive). Checked; returns `None` on overflow.
    pub const fn end(&self) -> Option<u64> {
        match self.offset.checked_add(self.length) {
            Some(e) => Some(e),
            None => None,
        }
    }

    /// Whether this extent contains `pos`.
    pub const fn contains(&self, pos: u64) -> bool {
        if let Some(end) = self.end() {
            pos >= self.offset && pos < end
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_id_roundtrip() {
        let c = ChunkId::of(b"hello world");
        let hex = c.to_string();
        assert_eq!(hex.len(), 64);
        assert_eq!(ChunkId::from_hex(&hex), Some(c));
        assert_eq!(ChunkId::from_hex(&hex.to_uppercase()), Some(c));
        assert_eq!(ChunkId::from_hex("abc"), None);
        assert_eq!(ChunkId::from_hex(&"z".repeat(64)), None);
        assert_eq!(ChunkId::ZERO, ChunkId::ZERO);
        assert!(ChunkId::ZERO.is_zero());
        assert!(!c.is_zero());
    }

    #[test]
    fn extent_arithmetic() {
        let e = Extent::new(10, 5);
        assert_eq!(e.end(), Some(15));
        assert!(e.contains(10));
        assert!(e.contains(14));
        assert!(!e.contains(15));
        assert!(!e.contains(9));
        let big = Extent::new(u64::MAX - 1, 5);
        assert_eq!(big.end(), None);
    }
}
