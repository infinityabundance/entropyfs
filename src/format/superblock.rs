//! Superblock codec: two independently checksummed slots, commit by
//! generation (ADR-0008, `docs/format/ondisk-v1.md` §2).

#![forbid(unsafe_code)]

use crate::core::extent::ChunkId;
use crate::format::codec::{CodecError, Reader, Writer, crc32c};
use crate::format::features::FeatureBits;
use crate::format::version::{
    FORMAT_MAJOR, FORMAT_MINOR, SUPERBLOCK_MAGIC, SUPERBLOCK_SLOT_SIZE, SUPERBLOCK_VERSION,
};

/// A decoded superblock slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Superblock {
    /// Slot struct version.
    pub version: u8,
    /// Format major.
    pub format_major: u16,
    /// Format minor.
    pub format_minor: u16,
    /// compat feature bits.
    pub compat: u64,
    /// ro_compat feature bits.
    pub ro_compat: u64,
    /// incompat feature bits.
    pub incompat: u64,
    /// Filesystem UUID.
    pub uuid: [u8; 16],
    /// Commit generation.
    pub generation: u64,
    /// Root object id (BLAKE3 of the root object payload).
    pub root_object_id: ChunkId,
    /// Current segment sequence.
    pub segment_seq: u64,
    /// Creation time (unix nanos, informational).
    pub created_unix_ns: u64,
    /// Reserved flags.
    pub flags: u32,
    /// Extension bytes (reserved; must be all zero in v1).
    pub extension: [u8; 248],
}

impl Default for Superblock {
    fn default() -> Self {
        Self {
            version: SUPERBLOCK_VERSION,
            format_major: FORMAT_MAJOR,
            format_minor: FORMAT_MINOR,
            compat: 0,
            ro_compat: 0,
            incompat: 0,
            uuid: [0u8; 16],
            generation: 0,
            root_object_id: ChunkId::ZERO,
            segment_seq: 0,
            created_unix_ns: 0,
            flags: 0,
            extension: [0u8; 248],
        }
    }
}

impl Superblock {
    /// Feature bits view.
    pub fn features(&self) -> FeatureBits {
        FeatureBits {
            compat: self.compat,
            ro_compat: self.ro_compat,
            incompat: self.incompat,
        }
    }

    /// Encode to a full 512-byte slot (checksum in the last 4 bytes).
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(SUPERBLOCK_SLOT_SIZE as usize);
        w.bytes(&SUPERBLOCK_MAGIC);
        w.u8(self.version);
        w.u16(self.format_major);
        w.u16(self.format_minor);
        w.u64(self.compat);
        w.u64(self.ro_compat);
        w.u64(self.incompat);
        w.bytes(&self.uuid);
        w.u64(self.generation);
        w.bytes(self.root_object_id.as_bytes());
        w.u64(self.segment_seq);
        w.u64(self.created_unix_ns);
        w.u32(self.flags);
        // extension_len (u16) + extension (248 bytes)
        w.u16(0);
        w.bytes(&self.extension);
        // Pad to 508 bytes (checksum occupies the last 4).
        let mut out = w.into_bytes();
        debug_assert!(out.len() <= 508);
        out.resize(508, 0);
        let crc = crc32c(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        debug_assert_eq!(out.len() as u64, SUPERBLOCK_SLOT_SIZE);
        out
    }

    /// Decode a full 512-byte slot. Typed errors; never panics.
    pub fn decode(slot: &[u8]) -> Result<Self, CodecError> {
        if slot.len() != SUPERBLOCK_SLOT_SIZE as usize {
            return Err(CodecError::Malformed);
        }
        // Checksum covers the first 508 bytes.
        let stored_crc = u32::from_le_bytes(slot[508..512].try_into().unwrap());
        if crc32c(&slot[..508]) != stored_crc {
            return Err(CodecError::Malformed);
        }
        let mut r = Reader::new(&slot[..508]);
        let magic = r.take(8)?;
        if magic != SUPERBLOCK_MAGIC {
            return Err(CodecError::Malformed);
        }
        let version = r.u8()?;
        if version != SUPERBLOCK_VERSION {
            return Err(CodecError::Malformed);
        }
        let format_major = r.u16()?;
        let format_minor = r.u16()?;
        let compat = r.u64()?;
        let ro_compat = r.u64()?;
        let incompat = r.u64()?;
        let uuid = read16(&mut r)?;
        let generation = r.u64()?;
        let root_object_id = read32(&mut r)?;
        let segment_seq = r.u64()?;
        let created_unix_ns = r.u64()?;
        let flags = r.u32()?;
        let extension_len = r.u16()? as usize;
        let mut extension = [0u8; 248];
        let ext = r.take(extension_len)?;
        extension[..extension_len].copy_from_slice(ext);
        // Remaining bytes must be zero padding.
        if r.remaining() != 0 && !r.clone().take(r.remaining())?.iter().all(|&b| b == 0) {
            return Err(CodecError::Malformed);
        }
        Ok(Superblock {
            version,
            format_major,
            format_minor,
            compat,
            ro_compat,
            incompat,
            uuid,
            generation,
            root_object_id,
            segment_seq,
            created_unix_ns,
            flags,
            extension,
        })
    }
}

fn read16(r: &mut Reader<'_>) -> Result<[u8; 16], CodecError> {
    Ok(r.take(16)?.try_into().unwrap())
}

fn read32(r: &mut Reader<'_>) -> Result<ChunkId, CodecError> {
    Ok(ChunkId::new(r.take(32)?.try_into().unwrap()))
}

/// Which superblock slot a generation commits to (A = even, B = odd).
pub fn slot_for_generation(generation: u64) -> u8 {
    (generation & 1) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let sb = Superblock {
            uuid: [7u8; 16],
            generation: 42,
            root_object_id: ChunkId::of(b"root"),
            segment_seq: 3,
            created_unix_ns: 123456789,
            incompat: 0b101,
            ..Default::default()
        };
        let slot = sb.encode();
        assert_eq!(slot.len() as u64, SUPERBLOCK_SLOT_SIZE);
        let back = Superblock::decode(&slot).unwrap();
        assert_eq!(back, sb);
    }

    #[test]
    fn slot_selection() {
        assert_eq!(slot_for_generation(0), 0);
        assert_eq!(slot_for_generation(1), 1);
        assert_eq!(slot_for_generation(2), 0);
        assert_eq!(slot_for_generation(3), 1);
    }

    #[test]
    fn corrupt_slot_rejected() {
        let sb = Superblock::default();
        let slot = sb.encode();
        // flip bytes in the body
        for pos in [0usize, 10, 100, 300] {
            let mut bad = slot.clone();
            bad[pos] ^= 0xFF;
            assert!(Superblock::decode(&bad).is_err());
        }
        // flip a checksum byte
        let mut bad = slot.clone();
        bad[510] ^= 0xFF;
        assert!(Superblock::decode(&bad).is_err());
        // wrong magic
        let mut bad = slot.clone();
        bad[0] = b'X';
        assert!(Superblock::decode(&bad).is_err());
        // truncated
        assert!(Superblock::decode(&slot[..400]).is_err());
    }
}
