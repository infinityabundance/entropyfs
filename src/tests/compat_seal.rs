//! Phase 12E.3 compatibility-seal courts: the normative behavior of the
//! compat / ro_compat / incompat gates, the read-only fallback, and the
//! typed compatibility errors — implementation-tested exactly as the
//! release gates require ("unknown COMPAT/RO_COMPAT/INCOMPAT behavior is
//! tested exactly").
//!
//! # What these courts pin
//!
//! - unknown `incompat` bit ⇒ every open refused, typed error;
//! - unknown `ro_compat` bit ⇒ writable open refused, read-only open
//!   permitted (the documented RO fallback, honored since 12E.3);
//! - a read-only open performs NO writes (every write funnel returns
//!   `StoreError::ReadOnly`);
//! - fsck reports the RO fallback as a warning, never an error;
//! - the engine inherits the same gates (`EngineOpenOptions.read_only`).

#![forbid(unsafe_code)]

use std::path::Path;

use crate::engine::{Engine, EngineOpenOptions, ErrorCode};
use crate::format::features::{AccessMode, Feature, FeatureBits};
use crate::format::superblock::Superblock;
use crate::store::{Store, StoreConfig, StoreError};

/// A fresh scratch store directory.
fn store_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("entropyfs-compat-seal")
        .tempdir()
        .expect("tempdir")
}

/// Craft a store whose superblock carries an unknown `ro_compat` bit
/// (the defined-but-unimplemented `ENCRYPTED` bit: unknown to this build,
/// so the RO fallback applies — never silently misread). The store must
/// be CLOSED (flock released) before this runs; it rewrites slot A in
/// place, preserving slot B and the file tail.
fn craft_ro_compat_store(dir: &Path) {
    let path = dir.join("superblock");
    let mut bytes = std::fs::read(&path).expect("read superblock");
    let mut sb = Superblock::decode(&bytes[0..512]).expect("decode slot A");
    sb.ro_compat |= Feature::Encrypted.mask();
    let enc = sb.encode();
    bytes[0..512].copy_from_slice(&enc);
    std::fs::write(&path, &bytes).expect("write superblock");
}

/// Craft a store whose superblock carries an unknown `incompat` bit
/// (a high bit no build has ever defined).
fn craft_unknown_incompat_store(dir: &Path) {
    let path = dir.join("superblock");
    let mut bytes = std::fs::read(&path).expect("read superblock");
    let mut sb = Superblock::decode(&bytes[0..512]).expect("decode slot A");
    sb.incompat |= 1u64 << 63;
    let enc = sb.encode();
    bytes[0..512].copy_from_slice(&enc);
    std::fs::write(&path, &bytes).expect("write superblock");
}

/// A store that has been put through the engine and checkpointed, so its
/// COMMITTED state contains a blob (read-only opens observe the last
/// durable checkpoint — the mutation log is not replayed, a write).
fn sealed_engine_store(dir: &Path) -> crate::engine::BlobId {
    let engine = Engine::create(dir, &EngineOpenOptions::default()).expect("engine create");
    let id = engine.put_blob(b"sealed ro-compat payload").expect("put");
    engine.sync().expect("sync");
    // Checkpoint so the blob is in the committed root, not only the log
    // (a read-only open does not replay the log).
    engine.compact().expect("compact");
    engine.close().expect("close");
    id
}

/// Resolve the blob file's inode through the namespace (the blob is NOT
/// inode 2 — that is the namespace directory itself).
fn blob_file_ino(store: &Store, payload: &[u8]) -> u64 {
    let cid = crate::core::extent::ChunkId::of(payload);
    let name = format!("{cid}").into_bytes();
    let ep = store.epoch();
    let dir_ino = store
        .dir_lookup_epoch(&ep, 1, b".engine")
        .expect("namespace")
        .expect("namespace exists")
        .ino;
    store
        .dir_lookup_epoch(&ep, dir_ino, &name)
        .expect("lookup")
        .expect("blob entry exists")
        .ino
}

#[test]
fn unknown_incompat_refuses_every_open_with_typed_error() {
    let dir = store_dir();
    let engine = Engine::create(dir.path(), &EngineOpenOptions::default()).expect("create");
    engine.close().expect("close");
    craft_unknown_incompat_store(dir.path());
    for read_only in [false, true] {
        let config = StoreConfig {
            read_only,
            ..Default::default()
        };
        match Store::open(dir.path(), &config) {
            Err(StoreError::IncompatibleFormat(e)) => {
                assert_eq!(e.unknown_incompat, 1u64 << 63);
                assert_eq!(e.unknown_ro_compat, 0);
            }
            other => panic!("expected typed refusal for read_only={read_only}, got {other:?}"),
        }
    }
}

#[test]
fn unknown_ro_compat_refuses_rw_and_permits_ro() {
    let dir = store_dir();
    let id = sealed_engine_store(dir.path());
    craft_ro_compat_store(dir.path());

    // Writable open: refused with the typed error (access = ReadWrite).
    let config = StoreConfig::default();
    match Store::open(dir.path(), &config) {
        Err(StoreError::IncompatibleFormat(e)) => {
            assert_eq!(e.unknown_ro_compat, Feature::Encrypted.mask());
            assert_eq!(e.access, AccessMode::ReadWrite);
            assert_eq!(e.format_major, crate::format::version::FORMAT_MAJOR);
        }
        other => panic!("expected rw refusal, got {other:?}"),
    }

    // Read-only open: permitted; every write funnel is a typed ReadOnly.
    let ro = StoreConfig {
        read_only: true,
        ..Default::default()
    };
    let store = Store::open(dir.path(), &ro).expect("ro open permitted");
    assert!(matches!(store.begin_tx(), Err(StoreError::ReadOnly)));
    assert!(matches!(
        store.durability_barrier(&crate::store::transaction::CrashHooks::none()),
        Err(StoreError::ReadOnly)
    ));
    assert!(matches!(
        store.epoch_checkpoint(&crate::store::transaction::CrashHooks::none()),
        Err(StoreError::ReadOnly)
    ));
    // Reads work: the committed blob is reachable.
    let ino = blob_file_ino(&store, b"sealed ro-compat payload");
    let out = store
        .read_file_epoch(&store.epoch(), ino, 0, 64)
        .expect("ro read");
    assert_eq!(out, b"sealed ro-compat payload");
    // The blob id resolves through the namespace.
    let cid = crate::core::extent::ChunkId::of(b"sealed ro-compat payload");
    assert_eq!(cid.as_bytes(), id.as_bytes());
    drop(store);
}

#[test]
fn read_only_open_observes_committed_state_only() {
    let dir = store_dir();
    // An un-checkpointed put: acknowledged in the mutation log but NO
    // barrier/sync has run (the durability barrier checkpoints by
    // design), so the blob is log-only.
    let engine = Engine::create(dir.path(), &EngineOpenOptions::default()).expect("create");
    let id = engine.put_blob(b"log-only blob").expect("put");
    // NOTE: no sync() — a barrier would checkpoint and commit the blob.
    engine.close().expect("close");

    // RO open skips replay: the log-only blob is not visible (documented:
    // RO observes the last durable checkpoint).
    let ro = StoreConfig {
        read_only: true,
        ..Default::default()
    };
    let store = Store::open(dir.path(), &ro).expect("ro open");
    // The log-only blob is not visible (no replay): its name resolves
    // to nothing in the committed namespace.
    {
        let cid = crate::core::extent::ChunkId::of(b"log-only blob");
        let name = format!("{cid}").into_bytes();
        let ep = store.epoch();
        let dir_ino = store
            .dir_lookup_epoch(&ep, 1, b".engine")
            .expect("namespace")
            .expect("namespace exists")
            .ino;
        assert!(
            store
                .dir_lookup_epoch(&ep, dir_ino, &name)
                .expect("lookup")
                .is_none()
        );
    }
    drop(store);

    // The same store opened RW replays and sees it.
    let rw = StoreConfig::default();
    let store = Store::open(dir.path(), &rw).expect("rw open");
    let ino = blob_file_ino(&store, b"log-only blob");
    let bytes = store
        .read_file_epoch(&store.epoch(), ino, 0, 64)
        .expect("read after replay");
    assert_eq!(bytes, b"log-only blob");
    let _ = id;
    drop(store);
}

#[test]
fn ro_compat_store_fsck_reports_warning_not_error() {
    let dir = store_dir();
    sealed_engine_store(dir.path());
    craft_ro_compat_store(dir.path());
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default())
        .expect("fsck runs on an ro_compat store");
    // The unknown ro_compat bit is a WARNING (read-only fallback), never
    // an error — the store remains fsck-clean.
    assert!(
        report.is_clean(),
        "ro_compat store must stay clean: {:?}",
        report.issues
    );
    assert_eq!(report.warning_count(), 1);
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.message.contains("ro_compat"))
    );
}

#[test]
fn engine_inherits_the_gates() {
    let dir = store_dir();
    sealed_engine_store(dir.path());
    craft_ro_compat_store(dir.path());

    // RW engine open: refused (typed IncompatibleFormat).
    match Engine::open(dir.path(), &EngineOpenOptions::default()) {
        Err(e) => assert_eq!(e.code(), ErrorCode::IncompatibleFormat),
        Ok(_) => panic!("expected rw refusal"),
    }

    // RO engine open: reads work, writes are typed Unsupported.
    let opts = EngineOpenOptions {
        read_only: true,
        ..Default::default()
    };
    let engine = Engine::open(dir.path(), &opts).expect("ro engine open");
    let cid = crate::core::extent::ChunkId::of(b"sealed ro-compat payload");
    let id = crate::engine::BlobId::from(cid);
    assert!(engine.contains(id).expect("contains"));
    assert_eq!(
        engine.get_blob(id).expect("get"),
        b"sealed ro-compat payload"
    );
    assert_eq!(
        engine.put_blob(b"nope").expect_err("ro put").code(),
        ErrorCode::Unsupported
    );
    assert_eq!(
        engine.sync().expect_err("ro sync").code(),
        ErrorCode::Unsupported
    );
    assert_eq!(
        engine.compact().expect_err("ro compact").code(),
        ErrorCode::Unsupported
    );
    engine.close().expect("close");
}

#[test]
fn engine_read_only_open_requires_namespace() {
    let dir = store_dir();
    // A raw store with no engine namespace.
    let config = StoreConfig::default();
    let store = Store::create(dir.path(), &config, [7u8; 16]).expect("create");
    drop(store);
    let opts = EngineOpenOptions {
        read_only: true,
        ..Default::default()
    };
    match Engine::open(dir.path(), &opts) {
        Err(e) => assert_eq!(e.code(), ErrorCode::NotFound),
        Ok(_) => panic!("expected NotFound for missing namespace"),
    }
}

#[test]
fn feature_bits_accessor_reports_superblock() {
    let dir = store_dir();
    sealed_engine_store(dir.path());
    let config = StoreConfig::default();
    let store = Store::open(dir.path(), &config).expect("open");
    let bits = store.feature_bits();
    let expected = FeatureBits::empty();
    let _ = expected;
    // The store has used the mutation log (incompat bit 15).
    assert_ne!(bits.incompat & Feature::MutationLog.mask(), 0);
    assert_eq!(bits.ro_compat, 0);
    drop(store);
}
