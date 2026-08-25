//! Explicit little-endian byte codecs (ADR-0012, `docs/format/ondisk-v1.md`).
//!
//! Every permanent structure is encoded by explicit byte-level codecs with
//! magic/tag, version, length, checked arithmetic, explicit endianness,
//! integrity, and compatibility rules. Never serialize Rust enum
//! discriminants or struct layouts.

#![forbid(unsafe_code)]

use std::fmt;

/// A checked little-endian writer. Grows on demand; callers impose their
/// own size limits before use.
#[derive(Debug, Default, Clone)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    /// New empty writer.
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// New writer with reserved capacity.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
        }
    }

    /// Underlying bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    /// Consume into bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    /// Current length.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Write one byte.
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    /// Write little-endian u16.
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write little-endian u32.
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write little-endian u64.
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write little-endian u128.
    pub fn u128(&mut self, v: u128) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write raw bytes.
    pub fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    /// Write a length-prefixed byte slice (u16 length).
    pub fn bytes16(&mut self, b: &[u8]) -> Result<(), CodecError> {
        let len = u16::try_from(b.len()).map_err(|_| CodecError::TooLong)?;
        self.u16(len);
        self.bytes(b);
        Ok(())
    }

    /// Write a length-prefixed byte slice (u32 length).
    pub fn bytes32(&mut self, b: &[u8]) -> Result<(), CodecError> {
        let len = u32::try_from(b.len()).map_err(|_| CodecError::TooLong)?;
        self.u32(len);
        self.bytes(b);
        Ok(())
    }

    /// Write an unsigned LEB128-style varint (u64).
    pub fn varint(&mut self, mut v: u64) {
        loop {
            let mut b = (v & 0x7F) as u8;
            v >>= 7;
            if v != 0 {
                b |= 0x80;
            }
            self.buf.push(b);
            if v == 0 {
                break;
            }
        }
    }
}

/// A checked little-endian reader over an exact slice.
#[derive(Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// New reader.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Remaining bytes.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Whether fully consumed.
    pub fn done(&self) -> bool {
        self.pos == self.buf.len()
    }

    /// Current position.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Peek the next byte without consuming.
    pub fn peek_u8(&self) -> Result<u8, CodecError> {
        self.buf.get(self.pos).copied().ok_or(CodecError::Truncated)
    }

    /// Read one byte.
    pub fn u8(&mut self) -> Result<u8, CodecError> {
        let v = self
            .buf
            .get(self.pos)
            .copied()
            .ok_or(CodecError::Truncated)?;
        self.pos += 1;
        Ok(v)
    }

    /// Read little-endian u16.
    pub fn u16(&mut self) -> Result<u16, CodecError> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes(s.try_into().unwrap()))
    }

    /// Read little-endian u32.
    pub fn u32(&mut self) -> Result<u32, CodecError> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes(s.try_into().unwrap()))
    }

    /// Read little-endian u64.
    pub fn u64(&mut self) -> Result<u64, CodecError> {
        let s = self.take(8)?;
        Ok(u64::from_le_bytes(s.try_into().unwrap()))
    }

    /// Read little-endian u128.
    pub fn u128(&mut self) -> Result<u128, CodecError> {
        let s = self.take(16)?;
        Ok(u128::from_le_bytes(s.try_into().unwrap()))
    }

    /// Read exactly `n` bytes.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        let end = self.pos.checked_add(n).ok_or(CodecError::Truncated)?;
        if end > self.buf.len() {
            return Err(CodecError::Truncated);
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    /// Read a u16 length-prefixed byte slice.
    pub fn bytes16(&mut self) -> Result<&'a [u8], CodecError> {
        let len = self.u16()? as usize;
        self.take(len)
    }

    /// Read a u32 length-prefixed byte slice with a caller cap.
    pub fn bytes32_capped(&mut self, cap: u64) -> Result<&'a [u8], CodecError> {
        let len = self.u32()? as u64;
        if len > cap {
            return Err(CodecError::TooLong);
        }
        self.take(len as usize)
    }

    /// Read a u32 length-prefixed byte slice.
    pub fn bytes32(&mut self) -> Result<&'a [u8], CodecError> {
        self.bytes32_capped(u32::MAX as u64)
    }

    /// Read a varint (u64).
    pub fn varint(&mut self) -> Result<u64, CodecError> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            if shift >= 64 {
                return Err(CodecError::Malformed);
            }
            let b = self.u8()?;
            result |= ((b & 0x7F) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
    }

    /// Skip `n` bytes (for forward-compatible extension areas).
    pub fn skip(&mut self, n: usize) -> Result<(), CodecError> {
        self.take(n).map(|_| ())
    }
}

/// Codec errors: typed, never panics on corrupt input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecError {
    /// Input ended mid-structure.
    Truncated,
    /// A length exceeds the format limit.
    TooLong,
    /// Structural violation (bad tag, version, ordering).
    Malformed,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for CodecError {}

/// CRC32C over bytes (physical record integrity, ADR-0011).
pub fn crc32c(bytes: &[u8]) -> u32 {
    crc32c::crc32c(bytes)
}

/// Combine two CRCs over a concatenation (for incremental hashing).
pub fn crc32c_combine(first: u32, second: u32, second_len: usize) -> u32 {
    crc32c::crc32c_combine(first, second, second_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_roundtrip() {
        let mut w = Writer::new();
        w.u8(1);
        w.u16(0x1234);
        w.u32(0xDEAD_BEEF);
        w.u64(0x0123_4567_89AB_CDEF);
        w.u128(u128::MAX);
        let b = w.into_bytes();
        let mut r = Reader::new(&b);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.u16().unwrap(), 0x1234);
        assert_eq!(r.u32().unwrap(), 0xDEAD_BEEF);
        assert_eq!(r.u64().unwrap(), 0x0123_4567_89AB_CDEF);
        assert_eq!(r.u128().unwrap(), u128::MAX);
        assert!(r.done());
    }

    #[test]
    fn varint_roundtrip() {
        for v in [0u64, 1, 127, 128, 300, 16384, u32::MAX as u64, u64::MAX] {
            let mut w = Writer::new();
            w.varint(v);
            let mut r = Reader::new(w.as_slice());
            assert_eq!(r.varint().unwrap(), v);
            assert!(r.done());
        }
    }

    #[test]
    fn length_prefixed() {
        let mut w = Writer::new();
        w.bytes16(b"hello").unwrap();
        w.bytes32(b"world").unwrap();
        let b = w.into_bytes();
        let mut r = Reader::new(&b);
        assert_eq!(r.bytes16().unwrap(), b"hello");
        assert_eq!(r.bytes32().unwrap(), b"world");
    }

    #[test]
    fn truncated_errors() {
        let b = [1u8, 2];
        let mut r = Reader::new(&b);
        assert_eq!(r.u16().unwrap(), 0x0201);
        assert_eq!(r.u8(), Err(CodecError::Truncated));
    }

    #[test]
    fn too_long_capped() {
        let mut w = Writer::new();
        w.u32(1000);
        w.bytes(&[0u8; 1000]);
        let b = w.into_bytes();
        let mut r = Reader::new(&b);
        assert_eq!(r.bytes32_capped(100), Err(CodecError::TooLong));
    }

    #[test]
    fn crc() {
        let a = crc32c(b"hello entropy");
        let b = crc32c(b"hello entropy");
        let c = crc32c(b"hello entropX");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
