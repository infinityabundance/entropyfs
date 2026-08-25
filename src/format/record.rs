//! Segment record envelope codec (`docs/format/ondisk-v1.md` §3).

#![forbid(unsafe_code)]

use crate::core::extent::ChunkId;
use crate::format::codec::{CodecError, Reader, Writer, crc32c};
use crate::format::version::{RECORD_HEADER_SIZE, RECORD_VERSION, RecordTag};

/// Fixed header size for v1 records.
pub const HEADER_SIZE: u64 = RECORD_HEADER_SIZE;

/// Flag: materialized_len field is valid.
pub const FLAG_HAS_MATERIALIZED_LEN: u16 = 0x0001;

/// A decoded record envelope (payload referenced, not owned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record<'a> {
    /// Record tag.
    pub tag: RecordTag,
    /// Format version.
    pub version: u8,
    /// Flags.
    pub flags: u16,
    /// Stored payload length.
    pub stored_len: u32,
    /// Materialized/logical length (valid when the flag is set).
    pub materialized_len: Option<u64>,
    /// Content id (BLAKE3 of the payload).
    pub content_id: ChunkId,
    /// Payload bytes.
    pub payload: &'a [u8],
    /// Absolute offset of the record start within its segment.
    pub offset: u64,
}

impl Record<'_> {
    /// Total on-disk size of this record (header + payload).
    pub fn total_size(&self) -> u64 {
        HEADER_SIZE + self.stored_len as u64
    }

    /// Whether the materialized length is present.
    pub fn has_materialized_len(&self) -> bool {
        self.flags & FLAG_HAS_MATERIALIZED_LEN != 0
    }
}

/// Encode a record envelope + payload.
pub fn encode(
    tag: RecordTag,
    flags: u16,
    materialized_len: Option<u64>,
    payload: &[u8],
) -> Vec<u8> {
    let stored_len = payload.len() as u32;
    let mut w = Writer::with_capacity(HEADER_SIZE as usize + stored_len as usize);
    w.u8(tag.tag());
    w.u8(RECORD_VERSION);
    w.u16(flags);
    w.u16(HEADER_SIZE as u16);
    w.u32(stored_len);
    match materialized_len {
        Some(ml) => w.u64(ml),
        None => w.u64(0),
    }
    // content_id = BLAKE3(payload) — computed after the header prefix is
    // laid out; we must write the header first without the id, then patch.
    // Write the content id as zeros for now (patched below).
    w.bytes(&[0u8; 32]);
    let header_crc_pos = w.len();
    let header_crc = crc32c(&w.as_slice()[..header_crc_pos]);
    w.u32(header_crc);
    let payload_crc = crc32c(payload);
    w.u32(payload_crc);
    w.bytes(payload);
    let body = w.into_bytes();
    let mut out = body;
    // Patch the content id at its fixed position.
    let cid = ChunkId::of(payload);
    let cid_pos = 1 + 1 + 2 + 2 + 4 + 8; // tag, version, flags, header_len, stored_len, mat_len
    out[cid_pos..cid_pos + 32].copy_from_slice(cid.as_bytes());
    // Recompute header CRC including the id.
    let header_crc = crc32c(&out[..header_crc_pos]);
    out[header_crc_pos..header_crc_pos + 4].copy_from_slice(&header_crc.to_le_bytes());
    out
}

/// Parse a record envelope at `offset` within `segment` bytes.
///
/// Returns `Ok(Some(record))` on success, `Ok(None)` when the remaining
/// bytes are all-zero padding (end of written region), and `Err` on
/// malformed data.
pub fn decode(segment: &[u8], offset: u64) -> Result<Option<Record<'_>>, CodecError> {
    let start = usize::try_from(offset).map_err(|_| CodecError::Malformed)?;
    let avail = segment.len().saturating_sub(start);
    if avail == 0 {
        return Ok(None);
    }
    // All-zero tail = padding / torn region end.
    let head = &segment[start..];
    if head.iter().all(|&b| b == 0) {
        return Ok(None);
    }
    if avail < HEADER_SIZE as usize {
        return Err(CodecError::Truncated);
    }
    let mut r = Reader::new(head);
    let tag = RecordTag::from_u8(r.u8()?).ok_or(CodecError::Malformed)?;
    let version = r.u8()?;
    if version != RECORD_VERSION {
        return Err(CodecError::Malformed);
    }
    let flags = r.u16()?;
    let header_len = r.u16()? as u64;
    if header_len != HEADER_SIZE {
        return Err(CodecError::Malformed);
    }
    let stored_len = r.u32()? as u64;
    let materialized_raw = r.u64()?;
    let content_id = read_id(&mut r)?;
    let header_crc = r.u32()?;
    let payload_crc = r.u32()?;
    // Validate header CRC (over the 50 bytes before the header CRC field).
    let header_end = 1 + 1 + 2 + 2 + 4 + 8 + 32;
    let computed = crc32c(&head[..header_end]);
    if computed != header_crc {
        return Err(CodecError::Malformed);
    }
    // Payload bounds.
    let payload_start = HEADER_SIZE;
    let payload_end = payload_start
        .checked_add(stored_len)
        .ok_or(CodecError::Malformed)?;
    if payload_end > avail as u64 {
        return Err(CodecError::Truncated);
    }
    let payload = &head[payload_start as usize..payload_end as usize];
    if crc32c(payload) != payload_crc {
        return Err(CodecError::Malformed);
    }
    // Content id must match the payload (logical content hash).
    if ChunkId::of(payload) != content_id {
        return Err(CodecError::Malformed);
    }
    let materialized_len = if flags & FLAG_HAS_MATERIALIZED_LEN != 0 {
        Some(materialized_raw)
    } else {
        None
    };
    Ok(Some(Record {
        tag,
        version,
        flags,
        stored_len: stored_len as u32,
        materialized_len,
        content_id,
        payload,
        offset,
    }))
}

/// Walk records sequentially; returns the end offset (next record start).
pub fn next_offset(record: &Record<'_>) -> u64 {
    record
        .offset
        .checked_add(record.total_size())
        .expect("record size overflow (validated at parse time)")
}

fn read_id(r: &mut Reader<'_>) -> Result<ChunkId, CodecError> {
    let b = r.take(32)?;
    Ok(ChunkId::new(b.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_with_and_without_materialized_len() {
        for (tag, ml) in [
            (RecordTag::Data, Some(65536u64)),
            (RecordTag::Model, None),
            (RecordTag::Inode, None),
            (RecordTag::Root, None),
        ] {
            let payload = b"payload bytes for a record".to_vec();
            let bytes = encode(
                tag,
                if ml.is_some() {
                    FLAG_HAS_MATERIALIZED_LEN
                } else {
                    0
                },
                ml,
                &payload,
            );
            let mut offset = 0u64;
            let rec = decode(&bytes, offset).unwrap().unwrap();
            assert_eq!(rec.tag, tag);
            assert_eq!(rec.payload, payload);
            assert_eq!(rec.materialized_len, ml);
            assert_eq!(rec.total_size(), bytes.len() as u64);
            offset = next_offset(&rec);
            // Beyond the record: nothing (empty).
            assert!(decode(&bytes, offset).unwrap().is_none());
        }
    }

    #[test]
    fn content_id_must_match() {
        let payload = b"abc".to_vec();
        let mut bytes = encode(RecordTag::Data, 0, None, &payload);
        // Flip a payload byte; decode must fail on payload CRC or content id.
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        assert!(decode(&bytes, 0).is_err());
    }

    #[test]
    fn truncated_tail() {
        let payload = vec![7u8; 100];
        let bytes = encode(
            RecordTag::Data,
            FLAG_HAS_MATERIALIZED_LEN,
            Some(100),
            &payload,
        );
        for cut in [0usize, 1, HEADER_SIZE as usize - 1, bytes.len() - 1] {
            // Truncated regions that still have a nonzero first byte must
            // error, not panic.
            if bytes[..cut].iter().all(|&b| b == 0) {
                continue;
            }
            let res = decode(&bytes[..cut], 0);
            if cut < HEADER_SIZE as usize {
                assert_eq!(res, Err(CodecError::Truncated));
            } else {
                assert!(res.is_err() || res.unwrap().is_none());
            }
        }
    }

    #[test]
    fn zero_tail_is_end() {
        let payload = vec![1u8; 10];
        let mut bytes = encode(RecordTag::Data, 0, None, &payload);
        bytes.extend_from_slice(&[0u8; 64]);
        let rec = decode(&bytes, 0).unwrap().unwrap();
        let next = next_offset(&rec);
        assert!(decode(&bytes, next).unwrap().is_none());
    }

    #[test]
    fn sequential_walk() {
        let mut bytes = Vec::new();
        let payloads: Vec<Vec<u8>> = (0..5u32).map(|i| vec![i as u8; 16 + i as usize]).collect();
        for (i, p) in payloads.iter().enumerate() {
            let tag = if i % 2 == 0 {
                RecordTag::Data
            } else {
                RecordTag::Model
            };
            bytes.extend_from_slice(&encode(
                tag,
                FLAG_HAS_MATERIALIZED_LEN,
                Some(p.len() as u64),
                p,
            ));
        }
        let mut offset = 0u64;
        for p in payloads.iter() {
            let rec = decode(&bytes, offset).unwrap().expect("record");
            assert_eq!(rec.payload, p);
            assert_eq!(rec.offset, offset);
            offset = next_offset(&rec);
        }
        assert!(decode(&bytes, offset).unwrap().is_none());
    }
}
