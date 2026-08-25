//! FUSE directory adaptation: readdir buffer filling.
//!
//! `.` and `..` are synthesized (never stored). The kernel `offset` is the
//! index into the sorted entry list (with `.` and `..` prepended), which
//! is stable for v1.

#![forbid(unsafe_code)]

use std::os::unix::ffi::OsStrExt;

use fuser::{FileType, INodeNo, ReplyDirectory};

use crate::store::{Store, StoreError};

/// The full, ordered entry list for a directory: `.`, `..`, then the
/// stored entries in name order. Entries are `(ino, d_type, name)`.
pub fn entry_list(store: &Store, ino: u64) -> Result<Vec<(u64, u8, Vec<u8>)>, StoreError> {
    let inode = store
        .get_inode(ino)?
        .ok_or_else(|| StoreError::Invariant(format!("inode {ino} missing")))?;
    let _ = inode;
    let parent = store.parent_of(ino)?;
    let (entries, _) = store.dir_scan(ino, None, usize::MAX)?;
    let mut all: Vec<(u64, u8, Vec<u8>)> = Vec::with_capacity(entries.len() + 2);
    all.push((ino, crate::store::directory::dt::DT_DIR, b".".to_vec()));
    all.push((parent, crate::store::directory::dt::DT_DIR, b"..".to_vec()));
    all.extend(entries.into_iter().map(|(name, e)| (e.ino, e.d_type, name)));
    Ok(all)
}

/// Map a stored `d_type` to a `fuser::FileType`.
pub fn file_type_for(d_type: u8) -> FileType {
    match d_type {
        crate::store::directory::dt::DT_DIR => FileType::Directory,
        crate::store::directory::dt::DT_REG => FileType::RegularFile,
        crate::store::directory::dt::DT_LNK => FileType::Symlink,
        crate::store::directory::dt::DT_CHR => FileType::CharDevice,
        crate::store::directory::dt::DT_BLK => FileType::BlockDevice,
        crate::store::directory::dt::DT_FIFO => FileType::NamedPipe,
        crate::store::directory::dt::DT_SOCK => FileType::Socket,
        _ => FileType::RegularFile,
    }
}

/// Fill a `readdir` reply for `ino` starting at entry index `offset`.
///
/// Returns the next offset (the index after the last entry sent), or
/// `None` when the directory is exhausted.
pub fn fill_reply(
    store: &Store,
    ino: u64,
    offset: u64,
    reply: &mut ReplyDirectory,
) -> Result<Option<u64>, StoreError> {
    let all = entry_list(store, ino)?;
    let mut idx = offset as usize;
    while idx < all.len() {
        let (e_ino, d_type, name) = &all[idx];
        let os_name = std::ffi::OsStr::from_bytes(name);
        if reply.add(
            INodeNo(*e_ino),
            (idx + 1) as u64,
            file_type_for(*d_type),
            os_name,
        ) {
            idx += 1;
        } else {
            // Buffer full; resume at this index next time.
            break;
        }
    }
    if idx >= all.len() {
        Ok(None)
    } else {
        Ok(Some(idx as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::transaction::CrashHooks;
    use crate::store::{Store, StoreConfig};

    #[test]
    fn entry_list_has_dot_dotdot_and_sorted_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg = StoreConfig {
            segment_size: 1024 * 1024,
            ..Default::default()
        };
        let mut store = Store::create(dir.path(), &cfg, [0x66; 16]).unwrap();
        // Root dir is ino 1 (the FUSE mount root); create "sub" and "file".
        store
            .create_entry(
                1,
                b"sub",
                crate::store::NewEntry::dir(0o755, 0, 0),
                &CrashHooks::none(),
            )
            .unwrap();
        store
            .create_entry(
                1,
                b"file",
                crate::store::NewEntry::file(0o644, 0, 0),
                &CrashHooks::none(),
            )
            .unwrap();
        let all = entry_list(&store, 1).unwrap();
        assert_eq!(all[0], (1, 4, b".".to_vec()));
        assert_eq!(all[1], (1, 4, b"..".to_vec()));
        assert_eq!(all[2].2, b"file");
        assert_eq!(all[3].2, b"sub");
    }
}
