//! Advisory POSIX file locking (in-process, single-mount semantics).
//!
//! v1 implements POSIX record locks in the daemon process: all FUSE
//! requests for one mount are served by this daemon, so a per-inode range
//! table gives correct advisory semantics among processes using the same
//! mount. The kernel already provides local lock fallback; these tables
//! make locking work consistently with our own read/write paths.
//!
//! Linux `flock` constants: F_RDLCK = 0, F_WRLCK = 1, F_UNLCK = 2.

#![forbid(unsafe_code)]

use std::collections::HashMap;

/// A POSIX record lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordLock {
    /// Lock owner (FUSE lock_owner).
    pub owner: u64,
    /// Start offset (inclusive).
    pub start: u64,
    /// End offset (inclusive; u64::MAX = EOF).
    pub end: u64,
    /// Lock type: 0 = F_RDLCK, 1 = F_WRLCK.
    pub typ: i32,
    /// Process id (reported by getlk).
    pub pid: u32,
}

/// The advisory lock table.
#[derive(Debug, Default)]
pub struct LockTable {
    /// ino → held locks.
    locks: HashMap<u64, Vec<RecordLock>>,
}

/// F_RDLCK.
pub const F_RDLCK: i32 = 0;
/// F_WRLCK.
pub const F_WRLCK: i32 = 1;
/// F_UNLCK.
pub const F_UNLCK: i32 = 2;

impl LockTable {
    /// New empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a lock of `typ` conflicts with an existing lock.
    fn conflicts(typ: i32, existing: &RecordLock) -> bool {
        // A write lock conflicts with everything; a read lock conflicts
        // only with write locks.
        typ == F_WRLCK || existing.typ == F_WRLCK
    }

    fn ranges_overlap(a: &RecordLock, start: u64, end: u64) -> bool {
        a.start <= end && start <= a.end
    }

    /// Acquire or release a lock (`typ` = F_UNLCK releases). Returns
    /// `Ok(true)` when acquired, `Ok(false)` when blocked (for
    /// non-blocking calls), `Err` when the range is invalid.
    pub fn setlk(
        &mut self,
        ino: u64,
        owner: u64,
        start: u64,
        end: u64,
        typ: i32,
        pid: u32,
    ) -> Result<bool, String> {
        if end < start {
            return Err("lock end before start".into());
        }
        let locks = self.locks.entry(ino).or_default();
        if typ == F_UNLCK {
            locks.retain(|l| !(l.owner == owner && Self::ranges_overlap(l, start, end)));
            return Ok(true);
        }
        if typ != F_RDLCK && typ != F_WRLCK {
            return Err(format!("unknown lock type {typ}"));
        }
        let probe = RecordLock {
            owner,
            start,
            end,
            typ,
            pid,
        };
        for existing in locks.iter() {
            if existing.owner == owner {
                // Same owner: the new lock replaces/merges (POSIX merges
                // same-process locks). Replace the covered range.
                continue;
            }
            if Self::ranges_overlap(existing, start, end) && Self::conflicts(typ, existing) {
                return Ok(false);
            }
        }
        // Remove same-owner locks overlapping the range, then add.
        locks.retain(|l| !(l.owner == owner && Self::ranges_overlap(l, start, end)));
        locks.push(probe);
        Ok(true)
    }

    /// Report the first conflicting lock in `[start, end]`, or `None`.
    pub fn getlk(
        &self,
        ino: u64,
        owner: u64,
        start: u64,
        end: u64,
        typ: i32,
    ) -> Option<RecordLock> {
        let locks = self.locks.get(&ino)?;
        for existing in locks.iter() {
            if existing.owner == owner {
                continue;
            }
            if Self::ranges_overlap(existing, start, end) && Self::conflicts(typ, existing) {
                return Some(*existing);
            }
        }
        None
    }

    /// Drop all locks held by `owner` (flush).
    pub fn release_owner(&mut self, owner: u64) {
        for locks in self.locks.values_mut() {
            locks.retain(|l| l.owner != owner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_lock_blocks_read_and_write() {
        let mut t = LockTable::new();
        assert!(t.setlk(1, 10, 0, 100, F_WRLCK, 1).unwrap());
        // Another owner's read conflicts with the write lock.
        assert!(!t.setlk(1, 20, 50, 60, F_RDLCK, 2).unwrap());
        assert!(!t.setlk(1, 20, 50, 60, F_WRLCK, 2).unwrap());
        // Outside the range: fine.
        assert!(t.setlk(1, 20, 200, 300, F_RDLCK, 2).unwrap());
        // Same owner can extend (POSIX ranges are inclusive on both ends:
        // [0,199] avoids sharing byte 200 with the other owner's lock).
        assert!(t.setlk(1, 10, 0, 199, F_WRLCK, 1).unwrap());
    }

    #[test]
    fn read_locks_share() {
        let mut t = LockTable::new();
        assert!(t.setlk(1, 10, 0, 100, F_RDLCK, 1).unwrap());
        assert!(t.setlk(1, 20, 0, 100, F_RDLCK, 2).unwrap());
        assert!(!t.setlk(1, 30, 50, 60, F_WRLCK, 3).unwrap());
    }

    #[test]
    fn unlock_and_getlk() {
        let mut t = LockTable::new();
        t.setlk(1, 10, 0, 100, F_WRLCK, 1).unwrap();
        let got = t.getlk(1, 20, 50, 60, F_RDLCK);
        assert!(got.is_some());
        assert_eq!(got.unwrap().pid, 1);
        t.setlk(1, 10, 0, 100, F_UNLCK, 1).unwrap();
        assert!(t.getlk(1, 20, 50, 60, F_RDLCK).is_none());
    }

    #[test]
    fn flush_releases_owner() {
        let mut t = LockTable::new();
        t.setlk(1, 10, 0, 100, F_WRLCK, 1).unwrap();
        t.setlk(1, 20, 0, 100, F_RDLCK, 2).unwrap();
        t.release_owner(10);
        assert!(t.getlk(1, 30, 0, 100, F_RDLCK).is_none());
    }
}
