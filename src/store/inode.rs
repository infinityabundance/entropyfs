//! Inode structure codec (`docs/format/ondisk-v1.md` §6).

#![forbid(unsafe_code)]

use crate::core::extent::ChunkId;
use crate::format::codec::{CodecError, Reader, Writer};

/// Inode data kinds.
pub const DATA_DIRECTORY: u8 = 0x01;
/// Regular file.
pub const DATA_FILE: u8 = 0x02;
/// Symbolic link.
pub const DATA_SYMLINK: u8 = 0x03;
/// Device node.
pub const DATA_DEVICE: u8 = 0x04;

/// POSIX mode type bits (subset used by EntropyFS).
pub mod mode {
    /// File type mask.
    pub const S_IFMT: u32 = 0o170000;
    /// Directory.
    pub const S_IFDIR: u32 = 0o040000;
    /// Regular file.
    pub const S_IFREG: u32 = 0o100000;
    /// Symlink.
    pub const S_IFLNK: u32 = 0o120000;
    /// Character device.
    pub const S_IFCHR: u32 = 0o020000;
    /// Block device.
    pub const S_IFBLK: u32 = 0o060000;
    /// FIFO.
    pub const S_IFIFO: u32 = 0o010000;
    /// Socket.
    pub const S_IFSOCK: u32 = 0o140000;
    /// Permission bits mask.
    pub const S_IPERM: u32 = 0o7777;
}

/// A nanosecond-resolution timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timespec {
    /// Seconds since the epoch.
    pub sec: u64,
    /// Nanoseconds (0..1e9).
    pub nsec: u32,
}

impl Timespec {
    /// Now (wall clock; metadata only — never affects decodability).
    pub fn now() -> Self {
        let d = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            sec: d.as_secs(),
            nsec: d.subsec_nanos(),
        }
    }

    /// Encode.
    pub fn encode(&self, w: &mut Writer) {
        w.u64(self.sec);
        w.u32(self.nsec);
    }

    /// Decode.
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, CodecError> {
        let sec = r.u64()?;
        let nsec = r.u32()?;
        if nsec >= 1_000_000_000 {
            return Err(CodecError::Malformed);
        }
        Ok(Self { sec, nsec })
    }
}

/// The immutable inode object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inode {
    /// st_mode (type + permissions).
    pub mode: u32,
    /// Owner uid.
    pub uid: u32,
    /// Owner gid.
    pub gid: u32,
    /// Logical size in bytes.
    pub size: u64,
    /// Access time.
    pub atime: Timespec,
    /// Change time.
    pub ctime: Timespec,
    /// Modification time.
    pub mtime: Timespec,
    /// Creation time.
    pub crtime: Timespec,
    /// Link count.
    pub nlink: u32,
    /// Device number (for device inodes).
    pub rdev: u32,
    /// Reserved flags.
    pub flags: u32,
    /// xattr tree root (ZERO = none).
    pub xattr_root: ChunkId,
    /// Data kind.
    pub data_kind: u8,
    /// Data payload (per kind).
    pub data: InodeData,
}

/// Per-kind inode data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InodeData {
    /// Directory: root of the directory B-tree.
    Directory {
        /// Root id of the directory entry B-tree.
        dir_root: ChunkId,
    },
    /// Regular file: root of the extent B-tree.
    File {
        /// Root id of the extent B-tree.
        extent_root: ChunkId,
    },
    /// Symlink: target bytes.
    Symlink {
        /// Link target bytes (size == target length).
        target: Vec<u8>,
    },
    /// Device: (rdev carries the number).
    Device,
}

impl Inode {
    /// New regular file inode.
    pub fn new_file(uid: u32, gid: u32, mode_perms: u32) -> Self {
        let now = Timespec::now();
        Self {
            mode: mode::S_IFREG | (mode_perms & mode::S_IPERM),
            uid,
            gid,
            size: 0,
            atime: now,
            ctime: now,
            mtime: now,
            crtime: now,
            nlink: 1,
            rdev: 0,
            flags: 0,
            xattr_root: ChunkId::ZERO,
            data_kind: DATA_FILE,
            data: InodeData::File {
                extent_root: ChunkId::ZERO,
            },
        }
    }

    /// New directory inode.
    pub fn new_dir(uid: u32, gid: u32, mode_perms: u32) -> Self {
        let now = Timespec::now();
        Self {
            mode: mode::S_IFDIR | (mode_perms & mode::S_IPERM),
            uid,
            gid,
            size: 0,
            atime: now,
            ctime: now,
            mtime: now,
            crtime: now,
            nlink: 2,
            rdev: 0,
            flags: 0,
            xattr_root: ChunkId::ZERO,
            data_kind: DATA_DIRECTORY,
            data: InodeData::Directory {
                dir_root: ChunkId::ZERO,
            },
        }
    }

    /// New symlink inode.
    pub fn new_symlink(target: Vec<u8>, uid: u32, gid: u32) -> Self {
        let now = Timespec::now();
        Self {
            mode: mode::S_IFLNK | 0o777,
            uid,
            gid,
            size: target.len() as u64,
            atime: now,
            ctime: now,
            mtime: now,
            crtime: now,
            nlink: 1,
            rdev: 0,
            flags: 0,
            xattr_root: ChunkId::ZERO,
            data_kind: DATA_SYMLINK,
            data: InodeData::Symlink { target },
        }
    }

    /// File type bits.
    pub fn file_type(&self) -> u32 {
        self.mode & mode::S_IFMT
    }

    /// Whether this is a directory.
    pub fn is_dir(&self) -> bool {
        self.file_type() == mode::S_IFDIR
    }

    /// Whether this is a regular file.
    pub fn is_file(&self) -> bool {
        self.file_type() == mode::S_IFREG
    }

    /// Whether this is a symlink.
    pub fn is_symlink(&self) -> bool {
        self.file_type() == mode::S_IFLNK
    }

    /// Encode to the inode payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(self.mode);
        w.u32(self.uid);
        w.u32(self.gid);
        w.u64(self.size);
        self.atime.encode(&mut w);
        self.ctime.encode(&mut w);
        self.mtime.encode(&mut w);
        self.crtime.encode(&mut w);
        w.u32(self.nlink);
        w.u32(self.rdev);
        w.u32(self.flags);
        w.bytes(self.xattr_root.as_bytes());
        w.u8(self.data_kind);
        match &self.data {
            InodeData::Directory { dir_root } => w.bytes(dir_root.as_bytes()),
            InodeData::File { extent_root } => w.bytes(extent_root.as_bytes()),
            InodeData::Symlink { target } => w.bytes32(target).expect("symlink target fits u32"),
            InodeData::Device => {}
        }
        w.into_bytes()
    }

    /// Decode an inode payload.
    pub fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
        let mut r = Reader::new(bytes);
        let mode = r.u32()?;
        let uid = r.u32()?;
        let gid = r.u32()?;
        let size = r.u64()?;
        let atime = Timespec::decode(&mut r)?;
        let ctime = Timespec::decode(&mut r)?;
        let mtime = Timespec::decode(&mut r)?;
        let crtime = Timespec::decode(&mut r)?;
        let nlink = r.u32()?;
        let rdev = r.u32()?;
        let flags = r.u32()?;
        let xattr_root = read_id(&mut r)?;
        let data_kind = r.u8()?;
        let data = match data_kind {
            DATA_DIRECTORY => InodeData::Directory {
                dir_root: read_id(&mut r)?,
            },
            DATA_FILE => InodeData::File {
                extent_root: read_id(&mut r)?,
            },
            DATA_SYMLINK => InodeData::Symlink {
                target: r.bytes32()?.to_vec(),
            },
            DATA_DEVICE => InodeData::Device,
            _ => return Err(CodecError::Malformed),
        };
        if !r.done() {
            return Err(CodecError::Malformed);
        }
        // Type bits must be consistent with the data kind.
        let ft = mode & mode::S_IFMT;
        let expected = match data_kind {
            DATA_DIRECTORY => mode::S_IFDIR,
            DATA_FILE => mode::S_IFREG,
            DATA_SYMLINK => mode::S_IFLNK,
            DATA_DEVICE => mode::S_IFCHR | mode::S_IFBLK,
            _ => return Err(CodecError::Malformed),
        };
        if ft != expected
            && !(data_kind == DATA_DEVICE && ft != 0 && (ft & (mode::S_IFCHR | mode::S_IFBLK)) != 0)
        {
            return Err(CodecError::Malformed);
        }
        Ok(Self {
            mode,
            uid,
            gid,
            size,
            atime,
            ctime,
            mtime,
            crtime,
            nlink,
            rdev,
            flags,
            xattr_root,
            data_kind,
            data,
        })
    }

    /// Content id of the inode object.
    pub fn id(&self) -> ChunkId {
        ChunkId::of(&self.encode())
    }
}

fn read_id(r: &mut Reader<'_>) -> Result<ChunkId, CodecError> {
    Ok(ChunkId::new(r.take(32)?.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_inode_roundtrip() {
        let ino = Inode::new_file(1000, 1000, 0o644);
        let bytes = ino.encode();
        let back = Inode::decode(&bytes).unwrap();
        assert_eq!(back, ino);
        assert!(back.is_file());
    }

    #[test]
    fn dir_inode_roundtrip() {
        let ino = Inode::new_dir(1000, 1000, 0o755);
        let bytes = ino.encode();
        let back = Inode::decode(&bytes).unwrap();
        assert_eq!(back, ino);
        assert!(back.is_dir());
        assert_eq!(back.nlink, 2);
    }

    #[test]
    fn symlink_roundtrip() {
        let ino = Inode::new_symlink(b"/some/target".to_vec(), 0, 0);
        let bytes = ino.encode();
        let back = Inode::decode(&bytes).unwrap();
        assert_eq!(back, ino);
        assert!(back.is_symlink());
        assert_eq!(back.size, 12);
    }

    #[test]
    fn corrupt_inode_rejected() {
        let ino = Inode::new_file(1, 1, 0o600);
        let bytes = ino.encode();
        for flip in [0usize, 10, bytes.len() - 1] {
            let mut bad = bytes.clone();
            bad[flip] ^= 0xFF;
            // Either a typed error or a valid different inode — never a
            // panic.
            let _ = Inode::decode(&bad);
        }
    }

    #[test]
    fn inconsistent_type_rejected() {
        let ino = Inode::new_file(1, 1, 0o600);
        let mut bytes = ino.encode();
        // Corrupt the mode type bits to DIR while data kind stays FILE.
        let mode = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let bad_mode = (mode & !mode::S_IFMT) | mode::S_IFDIR;
        bytes[0..4].copy_from_slice(&bad_mode.to_le_bytes());
        assert!(Inode::decode(&bytes).is_err());
    }
}
