//! FUSE inode adaptation: `Inode` ↔ `FileAttr` conversion and the
//! attribute TTL policy.

#![forbid(unsafe_code)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{FileAttr, FileType};

use crate::store::inode::{Inode, InodeData, Timespec, mode};

/// Attribute TTL (kernel caching window). Conservative for v1.
pub const ATTR_TTL: Duration = Duration::from_secs(1);
/// Entry TTL (lookup caching window).
pub const ENTRY_TTL: Duration = Duration::from_secs(1);

fn to_system_time(t: Timespec) -> SystemTime {
    UNIX_EPOCH + Duration::new(t.sec, t.nsec)
}

/// Map an inode to `fuser::FileAttr`.
pub fn attr_for(inode: &Inode, ino: u64) -> FileAttr {
    let kind = match &inode.data {
        InodeData::Directory { .. } => FileType::Directory,
        InodeData::File { .. } => FileType::RegularFile,
        InodeData::Symlink { .. } => FileType::Symlink,
        InodeData::Device => {
            if inode.mode & mode::S_IFMT == mode::S_IFCHR {
                FileType::CharDevice
            } else {
                FileType::BlockDevice
            }
        }
    };
    FileAttr {
        ino: fuser::INodeNo(ino),
        size: inode.size,
        blocks: inode.size.div_ceil(512),
        atime: to_system_time(inode.atime),
        mtime: to_system_time(inode.mtime),
        ctime: to_system_time(inode.ctime),
        crtime: to_system_time(inode.crtime),
        kind,
        perm: (inode.mode & mode::S_IPERM) as u16,
        nlink: inode.nlink,
        uid: inode.uid,
        gid: inode.gid,
        rdev: inode.rdev,
        blksize: 4096,
        flags: 0,
    }
}

/// The `d_type` for a directory entry for an inode kind.
pub fn d_type_for(inode: &Inode) -> u8 {
    match &inode.data {
        InodeData::Directory { .. } => crate::store::directory::dt::DT_DIR,
        InodeData::File { .. } => crate::store::directory::dt::DT_REG,
        InodeData::Symlink { .. } => crate::store::directory::dt::DT_LNK,
        InodeData::Device => crate::store::directory::dt::DT_UNKNOWN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attr_roundtrip_fields() {
        let inode = Inode::new_file(1000, 1001, 0o640);
        let attr = attr_for(&inode, 7);
        assert_eq!(attr.ino, fuser::INodeNo(7));
        assert_eq!(attr.kind, FileType::RegularFile);
        assert_eq!(attr.perm, 0o640);
        assert_eq!(attr.uid, 1000);
        assert_eq!(attr.gid, 1001);
        assert_eq!(attr.nlink, 1);
        assert_eq!(attr.size, 0);
        let dir = Inode::new_dir(0, 0, 0o755);
        assert_eq!(attr_for(&dir, 2).kind, FileType::Directory);
        assert_eq!(attr_for(&dir, 2).nlink, 2);
    }
}
