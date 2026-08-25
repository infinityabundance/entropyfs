//! Physical record integrity: validation of the on-disk record envelope.
//!
//! Every segment record carries a header CRC32C (over the fixed header
//! prefix including the content id) and a payload CRC32C, plus a content
//! id that must equal BLAKE3(payload). `format::record::decode` enforces
//! all three at parse time; this module owns the verification *interface*
//! used by fsck and scrub, and the aggregate envelope-check over a whole
//! segment.

#![forbid(unsafe_code)]

use crate::core::extent::ChunkId;
use crate::format::codec::CodecError;
use crate::format::record;
use crate::store::segment::SegmentError;

/// Verify one record envelope (header CRC, payload CRC, content id) given
/// the raw segment bytes. Returns the record on success.
pub fn verify_envelope<'a>(
    segment: &'a [u8],
    offset: u64,
) -> Result<Option<record::Record<'a>>, CodecError> {
    record::decode(segment, offset)
}

/// Aggregate envelope verification for a whole segment file: walks every
/// record, returning the count of valid records and the offset of the
/// first clean end (torn tail). Errors on mid-file corruption.
pub fn verify_segment(
    path: &std::path::Path,
    limit_records: u64,
) -> Result<(u64, u64), SegmentError> {
    let (records, end) = crate::store::segment::scan_segment(path, limit_records)?;
    Ok((records.len() as u64, end))
}

/// Physical record hash: the content id embedded in a record envelope.
pub fn record_content_id(segment: &[u8], offset: u64) -> Result<Option<ChunkId>, CodecError> {
    Ok(verify_envelope(segment, offset)?.map(|r| r.content_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::record::{FLAG_HAS_MATERIALIZED_LEN, encode};
    use crate::format::version::RecordTag;

    #[test]
    fn envelope_verify_detects_flip() {
        let payload = b"integrity payload".to_vec();
        let mut bytes = encode(
            RecordTag::Data,
            FLAG_HAS_MATERIALIZED_LEN,
            Some(payload.len() as u64),
            &payload,
        );
        let rec = verify_envelope(&bytes, 0).unwrap().unwrap();
        assert_eq!(rec.payload, payload);
        // Flip a payload byte: both payload CRC and content id break.
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        assert!(verify_envelope(&bytes, 0).is_err());
        // Flip a header byte: header CRC breaks.
        let mut bytes2 = encode(RecordTag::Data, 0, None, &payload);
        bytes2[10] ^= 0x01;
        assert!(verify_envelope(&bytes2, 0).is_err());
    }
}
