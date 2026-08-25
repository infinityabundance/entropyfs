//! Filesystem root object and superblock management (ADR-0008,
//! `docs/format/ondisk-v1.md` §5).

#![forbid(unsafe_code)]

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use crate::core::extent::ChunkId;
use crate::format::codec::{CodecError, Reader, Writer};
use crate::format::superblock::Superblock;
use crate::format::version::{
    FORMAT_MAJOR, FORMAT_MINOR, SUPERBLOCK_SLOT_A_OFFSET, SUPERBLOCK_SLOT_B_OFFSET,
    SUPERBLOCK_SLOT_SIZE,
};
use crate::store::StoreError;

/// The immutable filesystem root payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// Format major.
    pub format_major: u16,
    /// Format minor.
    pub format_minor: u16,
    /// Root of the inode index (ino → inode object).
    pub inode_index_root: ChunkId,
    /// Inode number of the filesystem root directory.
    pub root_dir_ino: u64,
    /// Root of the snapshot tree (name → snapshot entry).
    pub snapshot_tree_root: ChunkId,
    /// Root of the model index (reserved in v1; models live in the object
    /// index).
    pub model_index_root: ChunkId,
    /// Current segment sequence.
    pub segment_seq: u64,
    /// Derived-index epoch (bumped by GC compaction).
    pub index_epoch: u64,
    /// Filesystem UUID.
    pub uuid: [u8; 16],
    /// Commit generation (mirrors the superblock).
    pub generation: u64,
    /// Root of the chunk index (content id → descriptor bytes).
    pub chunk_index_root: ChunkId,
}

impl Default for Root {
    fn default() -> Self {
        Self {
            format_major: FORMAT_MAJOR,
            format_minor: FORMAT_MINOR,
            inode_index_root: ChunkId::ZERO,
            root_dir_ino: 0,
            snapshot_tree_root: ChunkId::ZERO,
            model_index_root: ChunkId::ZERO,
            segment_seq: 0,
            index_epoch: 0,
            uuid: [0u8; 16],
            generation: 0,
            chunk_index_root: ChunkId::ZERO,
        }
    }
}

impl Root {
    /// Encode to the root payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u16(self.format_major);
        w.u16(self.format_minor);
        w.bytes(self.inode_index_root.as_bytes());
        w.u64(self.root_dir_ino);
        w.bytes(self.snapshot_tree_root.as_bytes());
        w.bytes(self.model_index_root.as_bytes());
        w.u64(self.segment_seq);
        w.u64(self.index_epoch);
        w.bytes(&self.uuid);
        w.u64(self.generation);
        w.bytes(self.chunk_index_root.as_bytes());
        w.into_bytes()
    }

    /// Decode a root payload.
    pub fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
        let mut r = Reader::new(bytes);
        let format_major = r.u16()?;
        let format_minor = r.u16()?;
        let inode_index_root = read_id(&mut r)?;
        let root_dir_ino = r.u64()?;
        let snapshot_tree_root = read_id(&mut r)?;
        let model_index_root = read_id(&mut r)?;
        let segment_seq = r.u64()?;
        let index_epoch = r.u64()?;
        let uuid = read16(&mut r)?;
        let generation = r.u64()?;
        let chunk_index_root = read_id(&mut r)?;
        if !r.done() {
            return Err(CodecError::Malformed);
        }
        if format_major != FORMAT_MAJOR {
            return Err(CodecError::Malformed);
        }
        Ok(Self {
            format_major,
            format_minor,
            inode_index_root,
            root_dir_ino,
            snapshot_tree_root,
            model_index_root,
            segment_seq,
            index_epoch,
            uuid,
            generation,
            chunk_index_root,
        })
    }

    /// Content id of the root object.
    pub fn id(&self) -> ChunkId {
        ChunkId::of(&self.encode())
    }
}

fn read16(r: &mut Reader<'_>) -> Result<[u8; 16], CodecError> {
    Ok(r.take(16)?.try_into().unwrap())
}

fn read_id(r: &mut Reader<'_>) -> Result<ChunkId, CodecError> {
    Ok(ChunkId::new(r.take(32)?.try_into().unwrap()))
}

/// The dual-slot superblock view with generation selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuperblockPair {
    /// Slot A (generation even).
    pub a: Option<Superblock>,
    /// Slot B (generation odd).
    pub b: Option<Superblock>,
}

impl SuperblockPair {
    /// Read both slots from the superblock file.
    pub fn read(path: &Path) -> Result<Self, StoreError> {
        let mut file = File::open(path).map_err(|e| StoreError::Io(e.to_string()))?;
        let a = read_slot(&mut file, SUPERBLOCK_SLOT_A_OFFSET)?;
        let b = read_slot(&mut file, SUPERBLOCK_SLOT_B_OFFSET)?;
        Ok(Self { a, b })
    }

    /// Choose the highest valid committed generation.
    pub fn choose(&self) -> Result<Superblock, StoreError> {
        let valid: Vec<Superblock> = [self.a.clone(), self.b.clone()]
            .into_iter()
            .flatten()
            .collect();
        let best = valid.into_iter().max_by_key(|s| s.generation);
        match best {
            Some(s) => Ok(s),
            None => Err(StoreError::Superblock("no valid superblock found".into())),
        }
    }

    /// The inactive slot (the one a new generation commits to).
    pub fn inactive_slot(&self, next_generation: u64) -> u64 {
        match next_generation & 1 {
            0 => SUPERBLOCK_SLOT_A_OFFSET,
            _ => SUPERBLOCK_SLOT_B_OFFSET,
        }
    }
}

fn read_slot(file: &mut File, offset: u64) -> Result<Option<Superblock>, StoreError> {
    use std::io::Read;
    let mut buf = [0u8; SUPERBLOCK_SLOT_SIZE as usize];
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| StoreError::Io(e.to_string()))?;
    let mut read = 0usize;
    while read < buf.len() {
        let n = file
            .read(&mut buf[read..])
            .map_err(|e| StoreError::Io(e.to_string()))?;
        if n == 0 {
            break;
        }
        read += n;
    }
    if read == 0 {
        return Ok(None); // fresh file
    }
    if read < buf.len() {
        return Err(StoreError::Superblock("truncated superblock slot".into()));
    }
    match Superblock::decode(&buf) {
        Ok(sb) => Ok(Some(sb)),
        Err(_) => Ok(None), // torn/invalid slot: ignore
    }
}

/// Write a superblock slot and fsync it.
pub fn write_slot(path: &Path, offset: u64, sb: &Superblock, sync: bool) -> Result<(), StoreError> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(path)
        .map_err(|e| StoreError::Io(e.to_string()))?;
    let slot = sb.encode();
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| StoreError::Io(e.to_string()))?;
    file.write_all(&slot)
        .map_err(|e| StoreError::Io(e.to_string()))?;
    if sync {
        file.sync_all().map_err(|e| StoreError::Io(e.to_string()))?;
    }
    Ok(())
}

/// Read a single superblock slot by offset (used by fsck).
pub fn read_slot_at(path: &Path, offset: u64) -> Result<Option<Superblock>, StoreError> {
    let mut file = File::open(path).map_err(|e| StoreError::Io(e.to_string()))?;
    read_slot(&mut file, offset)
}

/// The superblock view used during mounts (re-export alias).
pub use crate::format::superblock::slot_for_generation as slot_for_gen;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_roundtrip() {
        let root = Root {
            inode_index_root: ChunkId::of(b"inodes"),
            root_dir_ino: 2,
            snapshot_tree_root: ChunkId::of(b"snaps"),
            chunk_index_root: ChunkId::of(b"chunks"),
            segment_seq: 7,
            index_epoch: 3,
            uuid: [9u8; 16],
            generation: 42,
            ..Default::default()
        };
        let bytes = root.encode();
        let back = Root::decode(&bytes).unwrap();
        assert_eq!(back, root);
        assert_eq!(root.id(), ChunkId::of(&bytes));
    }

    #[test]
    fn corrupt_root_rejected() {
        let root = Root::default();
        let mut bytes = root.encode();
        // Corrupt the format_major field (bytes 0..2): decode must reject.
        bytes[0] ^= 0xFF;
        assert!(Root::decode(&bytes).is_err());
        // A corrupt hash byte decodes fine structurally (hashes are
        // validated against the payload by fsck, not the codec).
        let mut bytes2 = root.encode();
        bytes2[5] ^= 0xFF;
        assert!(Root::decode(&bytes2).is_ok());
    }

    #[test]
    fn generation_selection() {
        // Build a fake superblock file with two slots at generations 1 and 2.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("superblock");
        let sb1 = Superblock {
            generation: 1,
            root_object_id: ChunkId::of(b"root1"),
            ..Default::default()
        };
        let sb2 = Superblock {
            generation: 2,
            root_object_id: ChunkId::of(b"root2"),
            ..Default::default()
        };
        write_slot(&path, SUPERBLOCK_SLOT_A_OFFSET, &sb1, true).unwrap();
        write_slot(&path, SUPERBLOCK_SLOT_B_OFFSET, &sb2, true).unwrap();
        let pair = SuperblockPair::read(&path).unwrap();
        let chosen = pair.choose().unwrap();
        assert_eq!(chosen.generation, 2);
        assert_eq!(chosen.root_object_id, ChunkId::of(b"root2"));
    }

    #[test]
    fn torn_slot_ignored() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("superblock");
        let sb1 = Superblock {
            generation: 1,
            ..Default::default()
        };
        write_slot(&path, SUPERBLOCK_SLOT_A_OFFSET, &sb1, true).unwrap();
        // Write garbage into slot B (torn write).
        let mut file = OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(SUPERBLOCK_SLOT_B_OFFSET))
            .unwrap();
        file.write_all(&[0xAA; SUPERBLOCK_SLOT_SIZE as usize])
            .unwrap();
        drop(file);
        let pair = SuperblockPair::read(&path).unwrap();
        assert!(pair.b.is_none());
        assert_eq!(pair.choose().unwrap().generation, 1);
    }
}
