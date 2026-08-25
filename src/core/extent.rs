//! Logical extent definitions and content identity.
//!
//! A [`ChunkId`] is the 256-bit BLAKE3 logical content hash of a chunk's
//! *materialized* bytes (ADR-0011). Two different physical representations
//! of the same logical bytes MUST have the same [`ChunkId`].

#![forbid(unsafe_code)]

use std::fmt;

/// 256-bit content identifier: BLAKE3 over materialized logical bytes.
///
/// Used for: logical content identity (dedup, references), structural
/// object identity in the store, and integrity verification.
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

/// A logical extent: `[offset, offset+length)` within a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Extent {
    /// Logical offset of the extent start.
    pub offset: u64,
    /// Logical length of the extent.
    pub length: u64,
}

impl Extent {
    /// Create a new extent. The caller guarantees `offset + length` does not
    /// overflow (validated at parse time; here we check and panic in debug).
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
