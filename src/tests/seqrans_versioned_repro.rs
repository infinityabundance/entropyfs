//! H2 versioned-corpus regression: sequential drift versions written
//! through the real write path must round-trip byte-exactly after every
//! version, including the rebase-on-write flatten path (which re-encodes
//! deep base chains through the unguided `encode_chunk`).
//!
//! This caught two real defects during the SequenceRans landing:
//! 1. an encoder bug where a match length leaving a 1..=3-byte tail after
//!    131-byte copy chunking produced an invalid 0x7F command byte;
//! 2. the flatten-on-write path committing unguided descriptors without
//!    the §32 materialize gate (now enforced via `Store::validate_update`).

#![forbid(unsafe_code)]

use crate::evidence::corpus;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x51; 16]).unwrap()
}

#[test]
fn versioned_corpus_roundtrips_after_each_version() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let ino = store
        .create_entry(
            1,
            b"f",
            NewEntry::file(0o644, 1000, 1000),
            &CrashHooks::none(),
        )
        .unwrap();
    // 4 versions of a 1 MiB structured file: exercises all four chunk
    // classes across base chains that reach the flatten threshold (the
    // v3/v4 class-2 chunks previously broke the walk).
    let corpus = corpus::versioned(1, 4);
    assert!(corpus.versions.len() >= 2);
    for (v, version) in corpus.versions.iter().enumerate() {
        store.write_region(ino, 0, version).unwrap();
        let back = store.read_file(ino, 0, version.len() as u64).unwrap();
        assert_eq!(back, *version, "version {v}: read-back mismatch");
    }
    // And with the deferred-durability batch path (the FUSE write path),
    // regions placed non-overlapping.
    let stride = corpus.versions[0].len() as u64;
    let mut writes: Vec<(u64, Vec<u8>)> = Vec::new();
    for (v, version) in corpus.versions.iter().enumerate() {
        writes.push((v as u64 * stride, version.clone()));
    }
    store
        .write_region_batch(ino, &writes, Default::default())
        .unwrap();
    for (v, version) in corpus.versions.iter().enumerate() {
        let back = store
            .read_file(ino, v as u64 * stride, version.len() as u64)
            .unwrap();
        assert_eq!(back, *version, "batch version {v}: read-back mismatch");
    }
}
