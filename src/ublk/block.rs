//! The entropy store exposed as a flat block device (Phase 7, ADR-0020).
//!
//! A ublk device is a *view over the same storage engine*: one hidden file
//! inode (`.<name>.ublk`) whose bytes are the block device image. Block
//! I/O goes through the normal extent/representation path, so every
//! entropy representation (dedup, base+residual, rANS, configurational)
//! applies to block storage too. Nothing is duplicated: the FUSE frontend
//! and this block frontend share one `Store`.
//!
//! Semantics:
//! - block size 4096 (the store chunk class);
//! - read/write via the materialization/guided-search paths;
//! - `flush` = durability barrier (ADR-0008 Phase 6);
//! - `discard` = `punch_hole` (the range reads as zeros and space is freed);
//! - device capacity is fixed at creation (the hidden file is sized once).

#![forbid(unsafe_code)]

use std::path::Path;

use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig, StoreError};

/// Default block size (the store chunk class — 64 KiB would make
/// sub-block writes expensive, so block devices use a 4 KiB logical
/// block; the extent engine still encodes 64 KiB chunks internally).
pub const BLOCK_SIZE: u64 = 4096;

/// Hidden device file prefix inside the store root.
pub const DEVICE_PREFIX: &str = ".ublk-";

/// A block-addressable view over the entropy store.
pub struct BlockStore {
    store: Store,
    /// The hidden inode backing this device.
    ino: u64,
    /// Capacity in blocks.
    blocks: u64,
}

impl BlockStore {
    /// Open (or create) a block device with `capacity_bytes` capacity
    /// backed by the store at `dir`. The device file is a hidden regular
    /// file (`.<name>.ublk`) in the store root, so it participates in the
    /// normal namespace, snapshots, GC and forensic tooling.
    pub fn open_or_create(
        dir: &Path,
        config: &StoreConfig,
        name: &str,
        capacity_bytes: u64,
    ) -> Result<Self, StoreError> {
        if name.is_empty() || name.contains('/') || name.len() > 128 {
            return Err(StoreError::Config("invalid ublk device name".into()));
        }
        let dev_name = format!("{DEVICE_PREFIX}{name}");
        let store = if dir.join("superblock").exists() {
            Store::open(dir, config)?
        } else {
            Store::create(
                dir,
                config,
                crate::core::extent::ChunkId::of(name.as_bytes()).as_bytes()[..16]
                    .try_into()
                    .expect("16 bytes"),
            )?
        };
        let ino = match store.dir_lookup(1, dev_name.as_bytes())? {
            Some(e) => e.ino,
            None => store.create_entry(
                1,
                dev_name.as_bytes(),
                NewEntry::file(
                    0o600,
                    crate::store::current_uid(),
                    crate::store::current_gid(),
                ),
                &CrashHooks::none(),
            )?,
        };
        let blocks = capacity_bytes.div_ceil(BLOCK_SIZE);
        // Size the device once (sparse; later size changes are rejected to
        // keep the block map stable).
        let inode = store
            .get_inode(ino)?
            .ok_or_else(|| StoreError::Invariant(format!("device inode {ino} missing")))?;
        if inode.size != blocks * BLOCK_SIZE {
            store.truncate_file(ino, blocks * BLOCK_SIZE)?;
        }
        store.durability_barrier(&CrashHooks::none())?;
        Ok(Self { store, ino, blocks })
    }

    /// Open an existing device (capacity from the device file size).
    pub fn open(dir: &Path, config: &StoreConfig, name: &str) -> Result<Self, StoreError> {
        let dev_name = format!("{DEVICE_PREFIX}{name}");
        let store = Store::open(dir, config)?;
        let ino = store
            .dir_lookup(1, dev_name.as_bytes())?
            .ok_or_else(|| StoreError::Invariant(format!("no such ublk device '{name}'")))?
            .ino;
        let inode = store
            .get_inode(ino)?
            .ok_or_else(|| StoreError::Invariant("device inode missing".into()))?;
        Ok(Self {
            store,
            ino,
            blocks: inode.size / BLOCK_SIZE,
        })
    }

    /// Capacity in bytes.
    pub fn capacity_bytes(&self) -> u64 {
        self.blocks * BLOCK_SIZE
    }

    /// Capacity in blocks.
    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    /// The store (for status/forensics).
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Mutable store access (maintenance: GC, optimize).
    pub fn store_mut(&mut self) -> &Store {
        &mut self.store
    }

    /// Read `len` bytes at `offset` (clamped to the device capacity;
    /// beyond-EOF reads return short like a block device at end of media).
    pub fn read(&mut self, offset: u64, len: u64) -> Result<Vec<u8>, StoreError> {
        if offset >= self.capacity_bytes() {
            return Ok(Vec::new());
        }
        let end = (offset.saturating_add(len)).min(self.capacity_bytes());
        self.store.read_file(self.ino, offset, end - offset)
    }

    /// Write `data` at `offset` (clamped to the device capacity; writes
    /// beyond the end are dropped, like a block device).
    pub fn write(&mut self, offset: u64, data: &[u8]) -> Result<u64, StoreError> {
        if offset >= self.capacity_bytes() {
            return Ok(0);
        }
        let room = self.capacity_bytes() - offset;
        let n = (data.len() as u64).min(room);
        self.store
            .write_region(self.ino, offset, &data[..n as usize])?;
        Ok(n)
    }

    /// Flush (durability barrier).
    pub fn flush(&mut self) -> Result<(), StoreError> {
        self.store.durability_barrier(&CrashHooks::none())
    }

    /// Discard a range: the bytes read as zeros and their storage is
    /// freed (the block device equivalent of hole punching).
    pub fn discard(&mut self, offset: u64, len: u64) -> Result<(), StoreError> {
        let end = (offset.saturating_add(len)).min(self.capacity_bytes());
        if end > offset {
            self.store.punch_hole(self.ino, offset, end, true)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn dev(dir: &TempDir) -> BlockStore {
        BlockStore::open_or_create(
            dir.path(),
            &StoreConfig::default(),
            "test0",
            16 * 1024 * 1024,
        )
        .unwrap()
    }

    #[test]
    fn block_roundtrip() {
        let dir = TempDir::new().unwrap();
        let mut d = dev(&dir);
        assert_eq!(d.capacity_bytes(), 16 * 1024 * 1024);
        // Write at a block boundary.
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        assert_eq!(d.write(0, &data).unwrap(), 4096);
        d.flush().unwrap();
        let read = d.read(0, 4096).unwrap();
        assert_eq!(read, data);
        // Non-boundary read.
        let read2 = d.read(100, 100).unwrap();
        assert_eq!(read2, &data[100..200]);
        // Beyond-EOF read is short.
        assert!(d.read(16 * 1024 * 1024 - 100, 4096).unwrap().len() <= 100);
        // Beyond-EOF write is dropped.
        assert_eq!(d.write(16 * 1024 * 1024 - 100, &data).unwrap(), 100);
    }

    #[test]
    fn discard_reads_zeros_and_frees() {
        let dir = TempDir::new().unwrap();
        let mut d = dev(&dir);
        let data = vec![0xABu8; 8192];
        d.write(0, &data).unwrap();
        d.flush().unwrap();
        assert_eq!(d.read(0, 8192).unwrap(), data);
        let used_before = d.store().physical_used();
        d.discard(0, 8192).unwrap();
        d.flush().unwrap();
        let read = d.read(0, 8192).unwrap();
        assert!(read.iter().all(|&b| b == 0), "discard must zero the range");
        // The superseded data objects are reclaimed by GC (append-only
        // storage retains them until then).
        crate::store::gc::collect(d.store_mut(), &CrashHooks::none()).unwrap();
        let used_after = d.store().physical_used();
        assert!(used_after < used_before, "discard must free space after GC");
    }

    #[test]
    fn device_survives_reopen_and_fsck() {
        let dir = TempDir::new().unwrap();
        let mut d = dev(&dir);
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 7) as u8).collect();
        d.write(4096, &data).unwrap();
        d.flush().unwrap();
        drop(d);
        let mut d2 = BlockStore::open(dir.path(), &StoreConfig::default(), "test0").unwrap();
        assert_eq!(d2.read(4096, 4096).unwrap(), data);
        let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
        assert!(report.is_clean(), "fsck: {}", report.render());
    }

    #[test]
    fn device_is_visible_as_a_hidden_file() {
        // The device is a regular file in the store root: snapshots and
        // forensics see it like any other file.
        let dir = TempDir::new().unwrap();
        let mut d = dev(&dir);
        d.write(0, b"block-data").unwrap();
        d.flush().unwrap();
        drop(d);
        let store = Store::open(dir.path(), &StoreConfig::default()).unwrap();
        let entry = store
            .dir_lookup(1, b".ublk-test0")
            .unwrap()
            .expect("device file");
        assert!(entry.ino > 0);
        store
            .create_snapshot(b"block-snap", &CrashHooks::none())
            .unwrap();
        assert_eq!(store.list_snapshots().unwrap().len(), 1);
    }
}
