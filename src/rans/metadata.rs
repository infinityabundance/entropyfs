//! rANS model metadata codec: model identity and serialization for
//! EntropyFS (`docs/format/ondisk-v1.md` §8).
//!
//! Explicit little-endian byte encoding with delta+RLE frequency packing.
//! No floating point. No serde. Model identity is BLAKE3 of the encoding;
//! identical models collapse to one content-addressed object.

#![forbid(unsafe_code)]

use crate::core::extent::ChunkId;
use crate::core::representation::RansCodec;
use crate::rans::model::{MAX_SCALE_BITS, MIN_SCALE_BITS, RansModel};

/// Token tags for the frequency stream codec.
const TOK_SET: u8 = 0x00;
const TOK_RLE: u8 = 0x01;
const TOK_DELTA: u8 = 0x02;
const TOK_END: u8 = 0xFF;

/// Encode a model to its persisted byte form.
pub fn encode_model(model: &RansModel) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + 256 * 3);
    out.push(model.scale_bits);
    out.push(model.codec.tag());
    out.extend_from_slice(&(256u16).to_le_bytes()); // sym_count

    // Frequency stream: SET first, then RLE runs / DELTA steps.
    out.push(TOK_SET);
    out.extend_from_slice(&model.freqs[0].to_le_bytes());
    let mut i = 1usize;
    let mut prev = model.freqs[0] as i32;
    while i < 256 {
        let f = model.freqs[i] as i32;
        if f == prev {
            // run length
            let start = i;
            while i < 256 && model.freqs[i] as i32 == f {
                i += 1;
            }
            let run = (i - start) as u16;
            out.push(TOK_RLE);
            out.extend_from_slice(&(f as u16).to_le_bytes());
            out.extend_from_slice(&run.to_le_bytes());
            continue;
        }
        let delta = (f - prev) as i16;
        out.push(TOK_DELTA);
        out.extend_from_slice(&delta.to_le_bytes());
        prev = f;
        i += 1;
    }
    out.push(TOK_END);

    // Integrity: CRC32C over everything before it.
    let crc = crc32c::crc32c(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

/// Decode a model from persisted bytes. Typed errors; never panics; never
/// allocates more than `max_bytes`.
pub fn decode_model(bytes: &[u8], max_bytes: u64) -> Result<RansModel, ModelDecodeError> {
    if bytes.len() as u64 > max_bytes {
        return Err(ModelDecodeError::TooLarge {
            len: bytes.len() as u64,
            max: max_bytes,
        });
    }
    if bytes.len() < 4 {
        return Err(ModelDecodeError::Truncated);
    }
    let payload_len = bytes.len() - 4;
    let stored_crc = u32::from_le_bytes(bytes[payload_len..].try_into().unwrap());
    let computed = crc32c::crc32c(&bytes[..payload_len]);
    if stored_crc != computed {
        return Err(ModelDecodeError::Checksum);
    }
    let mut rd = Reader::new(&bytes[..payload_len]);
    let scale_bits = rd.u8()?;
    if !(MIN_SCALE_BITS..=MAX_SCALE_BITS).contains(&scale_bits) {
        return Err(ModelDecodeError::BadScaleBits);
    }
    let codec_tag = rd.u8()?;
    let codec = RansCodec::from_u8(codec_tag).ok_or(ModelDecodeError::BadCodec)?;
    let sym_count = rd.u16()? as usize;
    if sym_count != 256 {
        return Err(ModelDecodeError::BadSymbolCount);
    }

    // Reconstruct frequencies from tokens.
    let mut freqs = [0u16; 256];
    let mut idx = 0usize;
    let mut prev: i32 = 0;
    loop {
        let tok = rd.u8()?;
        match tok {
            TOK_SET => {
                if idx != 0 {
                    return Err(ModelDecodeError::Malformed);
                }
                let v = rd.u16()? as i32;
                freqs[0] = v as u16;
                prev = v;
                idx = 1;
            }
            TOK_RLE => {
                let v = rd.u16()? as i32;
                let run = rd.u16()? as usize;
                if run == 0 || idx + run > 256 {
                    return Err(ModelDecodeError::Malformed);
                }
                for _ in 0..run {
                    freqs[idx] = v as u16;
                    idx += 1;
                }
                prev = v;
            }
            TOK_DELTA => {
                if idx == 0 {
                    return Err(ModelDecodeError::Malformed);
                }
                let d = rd.i16()? as i32;
                let v = prev + d;
                if !(0..=32768).contains(&v) {
                    return Err(ModelDecodeError::Malformed);
                }
                freqs[idx] = v as u16;
                prev = v;
                idx += 1;
            }
            TOK_END => break,
            _ => return Err(ModelDecodeError::Malformed),
        }
        if idx > 256 {
            return Err(ModelDecodeError::Malformed);
        }
    }
    if idx != 256 {
        return Err(ModelDecodeError::Malformed);
    }
    if !rd.done() {
        return Err(ModelDecodeError::TrailingBytes);
    }

    let model = RansModel {
        scale_bits,
        codec,
        freqs,
    };
    model
        .validate()
        .map_err(|e| ModelDecodeError::Invalid(e.to_string()))?;
    Ok(model)
}

/// Model identity: BLAKE3 of the encoded bytes.
pub fn model_id(model: &RansModel) -> ChunkId {
    ChunkId::of(&encode_model(model))
}

/// Model decode errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelDecodeError {
    /// Model object exceeds the format limit.
    TooLarge { len: u64, max: u64 },
    /// Truncated byte stream.
    Truncated,
    /// CRC32C mismatch.
    Checksum,
    /// scale_bits out of range.
    BadScaleBits,
    /// Unknown codec tag.
    BadCodec,
    /// Symbol count is not 256.
    BadSymbolCount,
    /// Malformed token stream.
    Malformed,
    /// Trailing bytes after the model.
    TrailingBytes,
    /// Model invariants violated (freqs do not sum, etc.).
    Invalid(String),
}

impl std::fmt::Display for ModelDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ModelDecodeError {}

/// Minimal checked little-endian reader.
struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }
    fn u8(&mut self) -> Result<u8, ModelDecodeError> {
        let v = *self.b.get(self.pos).ok_or(ModelDecodeError::Truncated)?;
        self.pos += 1;
        Ok(v)
    }
    fn u16(&mut self) -> Result<u16, ModelDecodeError> {
        let s = self
            .b
            .get(self.pos..self.pos + 2)
            .ok_or(ModelDecodeError::Truncated)?;
        self.pos += 2;
        Ok(u16::from_le_bytes(s.try_into().unwrap()))
    }
    fn i16(&mut self) -> Result<i16, ModelDecodeError> {
        let s = self
            .b
            .get(self.pos..self.pos + 2)
            .ok_or(ModelDecodeError::Truncated)?;
        self.pos += 2;
        Ok(i16::from_le_bytes(s.try_into().unwrap()))
    }
    fn done(&self) -> bool {
        self.pos == self.b.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rans::model::normalize_histogram;

    fn hist_of(data: &[u8]) -> [u32; 256] {
        let mut h = [0u32; 256];
        for &b in data {
            h[b as usize] += 1;
        }
        h
    }

    #[test]
    fn roundtrip() {
        let data: Vec<u8> = (0..20000u32).map(|i| ((i * 13) % 53) as u8).collect();
        let m = normalize_histogram(&hist_of(&data), 14, RansCodec::Interleaved2).unwrap();
        let bytes = encode_model(&m);
        let back = decode_model(&bytes, 2048).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn roundtrip_skewed() {
        let mut data = vec![0u8; 65536];
        for i in 0..65536 {
            data[i] = if i % 10 == 0 { 1 } else { 0 };
        }
        let m = normalize_histogram(&hist_of(&data), 15, RansCodec::Single).unwrap();
        let bytes = encode_model(&m);
        assert!(bytes.len() < 200, "model size {}", bytes.len());
        let back = decode_model(&bytes, 2048).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn uniform_model_is_small() {
        let data: Vec<u8> = (0..65536u32).map(|i| (i % 256) as u8).collect();
        let m = normalize_histogram(&hist_of(&data), 14, RansCodec::Single).unwrap();
        let bytes = encode_model(&m);
        // all freqs equal => one RLE token
        assert!(bytes.len() < 32, "size {}", bytes.len());
    }

    #[test]
    fn corrupt_bytes_typed_error() {
        let data: Vec<u8> = (0..20000u32).map(|i| ((i * 13) % 53) as u8).collect();
        let m = normalize_histogram(&hist_of(&data), 14, RansCodec::Single).unwrap();
        let bytes = encode_model(&m);
        for flip in [0usize, 3, 8, bytes.len() - 1] {
            let mut bad = bytes.clone();
            bad[flip] ^= 0x55;
            assert!(decode_model(&bad, 2048).is_err(), "flip at {flip}");
        }
    }

    #[test]
    fn truncated_typed_error() {
        let data: Vec<u8> = (0..20000u32).map(|i| ((i * 13) % 53) as u8).collect();
        let m = normalize_histogram(&hist_of(&data), 14, RansCodec::Single).unwrap();
        let bytes = encode_model(&m);
        for cut in [0usize, 1, 3, 10, bytes.len() - 1] {
            assert!(decode_model(&bytes[..cut], 2048).is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn size_limit_enforced() {
        let data: Vec<u8> = (0..20000u32).map(|i| ((i * 13) % 53) as u8).collect();
        let m = normalize_histogram(&hist_of(&data), 14, RansCodec::Single).unwrap();
        let bytes = encode_model(&m);
        assert_eq!(
            decode_model(&bytes, bytes.len() as u64 - 1),
            Err(ModelDecodeError::TooLarge {
                len: bytes.len() as u64,
                max: bytes.len() as u64 - 1,
            })
        );
    }

    #[test]
    fn model_id_is_content_addressed() {
        let data: Vec<u8> = (0..20000u32).map(|i| ((i * 13) % 53) as u8).collect();
        let m = normalize_histogram(&hist_of(&data), 14, RansCodec::Single).unwrap();
        assert_eq!(model_id(&m), ChunkId::of(&encode_model(&m)));
    }
}
