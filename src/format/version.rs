//! Format version constants and registries (`docs/format/compatibility.md`).

#![forbid(unsafe_code)]

/// Filesystem format major version.
pub const FORMAT_MAJOR: u16 = 1;
/// Filesystem format minor version.
pub const FORMAT_MINOR: u16 = 0;
/// Superblock magic: `ENTR0FS\0`.
pub const SUPERBLOCK_MAGIC: [u8; 8] = *b"ENTR0FS\0";
/// Superblock slot struct version.
pub const SUPERBLOCK_VERSION: u8 = 1;
/// Segment record format version.
pub const RECORD_VERSION: u8 = 1;
/// Segment file magic: `ESEG`.
pub const SEGMENT_MAGIC: [u8; 4] = *b"ESEG";
/// Superblock slot size (bytes).
pub const SUPERBLOCK_SLOT_SIZE: u64 = 512;
/// Offset of superblock slot A.
pub const SUPERBLOCK_SLOT_A_OFFSET: u64 = 0;
/// Offset of superblock slot B.
pub const SUPERBLOCK_SLOT_B_OFFSET: u64 = 4096;
/// Fixed segment record header size (v1): tag+ver+flags+header_len(2)+
/// stored_len(4)+materialized_len(8)+content_id(32)+header_crc(4)+
/// payload_crc(4) = 58.
pub const RECORD_HEADER_SIZE: u64 = 58;
/// Padding record tag.
pub const TAG_PAD: u8 = 0x7F;

/// Record tags (`docs/format/ondisk-v1.md` §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RecordTag {
    /// DATA — arbitrary payload referenced by descriptors.
    Data = 0x01,
    /// MODEL — encoded rANS model.
    Model = 0x02,
    /// INODE — encoded inode.
    Inode = 0x03,
    /// BTREE — persistent B-tree node.
    BtreeNode = 0x04,
    /// ROOT — encoded filesystem root.
    Root = 0x05,
    /// XATTR — xattr value payload.
    Xattr = 0x06,
    /// PAD — zero padding; never referenced.
    Pad = 0x7F,
}

impl RecordTag {
    /// Decode a persisted tag.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::Data),
            0x02 => Some(Self::Model),
            0x03 => Some(Self::Inode),
            0x04 => Some(Self::BtreeNode),
            0x05 => Some(Self::Root),
            0x06 => Some(Self::Xattr),
            0x7F => Some(Self::Pad),
            _ => None,
        }
    }

    /// Persisted tag.
    pub const fn tag(self) -> u8 {
        self as u8
    }

    /// Human-readable name.
    pub const fn name(self) -> &'static str {
        match self {
            RecordTag::Data => "data",
            RecordTag::Model => "model",
            RecordTag::Inode => "inode",
            RecordTag::BtreeNode => "btree",
            RecordTag::Root => "root",
            RecordTag::Xattr => "xattr",
            RecordTag::Pad => "pad",
        }
    }
}
