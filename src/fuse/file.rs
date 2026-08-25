//! FUSE file data path.
//!
//! The chunk-aligned read-modify-write logic lives in the store engine
//! (`Store::write_region`) so the store never depends on the FUSE layer;
//! this module carries the data-path tests.

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests {
    use crate::store::transaction::CrashHooks;
    use crate::store::{Store, StoreConfig};

    fn test_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg = StoreConfig {
            segment_size: 4 * 1024 * 1024,
            ..Default::default()
        };
        let mut store = Store::create(dir.path(), &cfg, [0x55; 16]).unwrap();
        let inode = crate::store::inode::Inode::new_file(0, 0, 0o644);
        let mut tx = store.begin_tx().unwrap();
        Store::put_inode_in_tx(&mut tx, 3, &inode).unwrap();
        tx.commit(&CrashHooks::none()).unwrap();
        (dir, store)
    }

    #[test]
    fn aligned_full_chunk() {
        let (_dir, mut store) = test_store();
        let data: Vec<u8> = (0..65536u32).map(|i| (i % 251) as u8).collect();
        store.write_region(3, 0, &data).unwrap();
        let read = store.read_file(3, 0, 65536).unwrap();
        assert_eq!(read, data);
    }

    #[test]
    fn partial_chunk_rmw_preserves_neighbors() {
        let (_dir, mut store) = test_store();
        let base: Vec<u8> = (0..65536u32).map(|i| (i % 251) as u8).collect();
        store.write_region(3, 0, &base).unwrap();
        // Overwrite [1000, 2000) only.
        let patch: Vec<u8> = (0..1000u32).map(|i| (i % 7) as u8).collect();
        store.write_region(3, 1000, &patch).unwrap();
        let read = store.read_file(3, 0, 65536).unwrap();
        assert_eq!(&read[..1000], &base[..1000]);
        assert_eq!(&read[1000..2000], &patch[..]);
        assert_eq!(&read[2000..], &base[2000..]);
    }

    #[test]
    fn hole_write_extends_size() {
        let (_dir, mut store) = test_store();
        let data = b"hole-write-data".to_vec();
        store.write_region(3, 200000, &data).unwrap();
        let read = store.read_file(3, 0, 200000 + data.len() as u64).unwrap();
        assert!(read[..200000].iter().all(|&b| b == 0));
        assert_eq!(&read[200000..], &data[..]);
    }

    #[test]
    fn cross_chunk_write() {
        let (_dir, mut store) = test_store();
        let base: Vec<u8> = (0..65536u32).map(|i| (i % 251) as u8).collect();
        store.write_region(3, 0, &base).unwrap();
        // Write spanning the 64 KiB boundary.
        let patch: Vec<u8> = (0..8192u32).map(|i| (i % 13) as u8).collect();
        store.write_region(3, 65000, &patch).unwrap();
        let read = store.read_file(3, 0, 73192).unwrap();
        assert_eq!(&read[..65000], &base[..65000]);
        assert_eq!(&read[65000..73192], &patch[..]);
    }
}
