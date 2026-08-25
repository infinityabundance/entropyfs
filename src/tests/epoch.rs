//! Phase-10D metadata writeback epoch tests (store level).
//!
//! The epoch accumulates acknowledged namespace/writeback mutations as
//! `MutationLog` envelopes + staged objects (page-cache durable per ack),
//! with the committed trees still describing the last CHECKPOINT. These
//! tests pin:
//! - overlay visibility: pending mutations are readable before the
//!   checkpoint;
//! - checkpoint convergence: the frozen overlay merges into the trees
//!   ONCE and the committed state matches the ops exactly;
//! - crash recovery: un-checkpointed envelopes replay at open (an
//!   acknowledged op survives a process crash), and the replay is
//!   idempotent across a subsequent checkpoint;
//! - unlink/rename semantics through the overlay;
//! - GC/optimizer coordination (the epoch is flushed before walks that
//!   only see committed roots).

#![forbid(unsafe_code)]

use crate::optimizer::foreground::ForegroundPolicy;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

fn create_store(dir: &TempDir) -> Store {
    let cfg = StoreConfig {
        segment_size: 128 * 1024 * 1024,
        ..Default::default()
    };
    Store::create(dir.path(), &cfg, [0x5d; 16]).unwrap()
}

fn root_ino(store: &Store) -> u64 {
    store.current_root().root_dir_ino
}

/// Deterministic byte-uniform noise (SplitMix64).
fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let b = z.to_le_bytes();
        let take = (n - out.len()).min(8);
        out.extend_from_slice(&b[..take]);
    }
    out
}

/// Sequential text (each 64 KiB chunk shares long matches with the
/// previous — the in-batch SequenceDict chain forms during epoch writes).
fn drift_text(n_chunks: usize) -> Vec<u8> {
    let chunk = 65536usize;
    let mut out = Vec::with_capacity(n_chunks * chunk);
    for c in 0..n_chunks {
        for i in 0..chunk {
            let mut b = b'a' + ((i / 7) % 23) as u8;
            if i % 97 == 0 {
                b = b"fn main() { return 0; }"[i % 23];
            }
            if i == c * 1009 % chunk {
                b = b'X';
            }
            out.push(b);
        }
    }
    out
}

#[test]
fn epoch_create_write_setattr_roundtrip_and_checkpoint() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let hooks = &CrashHooks::none();
    let root = root_ino(&store);

    // Epoch creates (files + a directory).
    let a = store
        .epoch_create(root, b"a", NewEntry::file(0o644, 1000, 1000), hooks)
        .unwrap();
    let b = store
        .epoch_create(root, b"b", NewEntry::file(0o644, 1000, 1000), hooks)
        .unwrap();
    let d = store
        .epoch_create(root, b"d", NewEntry::dir(0o755, 1000, 1000), hooks)
        .unwrap();

    // Overlay reads see the pending entries before any checkpoint.
    let ep = store.epoch();
    assert!(store.dir_lookup_epoch(&ep, root, b"a").unwrap().is_some());
    assert!(store.dir_lookup_epoch(&ep, root, b"b").unwrap().is_some());
    assert!(store.dir_lookup_epoch(&ep, root, b"d").unwrap().is_some());
    assert!(store.get_inode_epoch(&ep, a).unwrap().is_some());
    assert!(store.get_inode_epoch(&ep, d).unwrap().unwrap().is_dir());
    drop(ep);

    // Epoch writes: a small file (1 chunk) and a multi-chunk file.
    let small = b"hello epoch world".to_vec();
    store
        .epoch_write(
            a,
            0,
            &small,
            OptimizeOptions::default(),
            ForegroundPolicy::full(),
            hooks,
        )
        .unwrap();
    let big = drift_text(6);
    store
        .epoch_write(
            b,
            0,
            &big,
            OptimizeOptions::default(),
            ForegroundPolicy::full(),
            hooks,
        )
        .unwrap();

    // Overlay reads reproduce the exact bytes.
    let ep = store.epoch();
    assert_eq!(
        store.read_file_epoch(&ep, a, 0, 1024).unwrap(),
        small,
        "overlay read of a small file"
    );
    assert_eq!(
        store.read_file_epoch(&ep, b, 0, big.len() as u64).unwrap(),
        big,
        "overlay read of a multi-chunk file"
    );
    drop(ep);

    // A non-size setattr through the epoch.
    let updated = store
        .epoch_setattr(
            a,
            &crate::store::AttrUpdate {
                mode: Some(0o600),
                ..Default::default()
            },
            hooks,
        )
        .unwrap();
    assert_eq!(updated.mode & 0o777, 0o600);

    // Checkpoint: the frozen overlay merges into the trees once.
    store.epoch_checkpoint(hooks).unwrap();
    assert!(store.epoch().is_empty(), "checkpoint clears the epoch");

    // Committed state matches the ops exactly.
    let back_a = store.read_file(a, 0, small.len() as u64).unwrap();
    assert_eq!(back_a, small);
    let back_b = store.read_file(b, 0, big.len() as u64).unwrap();
    assert_eq!(back_b, big);
    let inode_a = store.get_inode(a).unwrap().unwrap();
    assert_eq!(inode_a.mode & 0o777, 0o600);
    assert_eq!(inode_a.size, small.len() as u64);
    assert_eq!(store.dir_lookup(root, b"d").unwrap().unwrap().ino, d);

    // fsck clean.
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn epoch_unlink_rename_semantics() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let hooks = &CrashHooks::none();
    let root = root_ino(&store);

    let a = store
        .epoch_create(root, b"a", NewEntry::file(0o644, 1000, 1000), hooks)
        .unwrap();
    let b = store
        .epoch_create(root, b"b", NewEntry::file(0o644, 1000, 1000), hooks)
        .unwrap();

    // Rename a -> c through the overlay.
    let out = store.epoch_rename(root, b"a", root, b"c", hooks).unwrap();
    assert_eq!(out.src_ino, a);
    assert!(out.replaced_dst_ino.is_none());
    let ep = store.epoch();
    assert!(store.dir_lookup_epoch(&ep, root, b"a").unwrap().is_none());
    assert!(store.dir_lookup_epoch(&ep, root, b"c").unwrap().is_some());
    drop(ep);

    // Rename c over b (replace).
    let out = store.epoch_rename(root, b"c", root, b"b", hooks).unwrap();
    assert_eq!(out.replaced_dst_ino, Some(b));
    let ep = store.epoch();
    assert!(store.dir_lookup_epoch(&ep, root, b"c").unwrap().is_none());
    assert_eq!(
        store
            .dir_lookup_epoch(&ep, root, b"b")
            .unwrap()
            .unwrap()
            .ino,
        a
    );
    drop(ep);

    // Unlink b (the moved a).
    let removed = store.epoch_unlink(root, b"b", false, hooks).unwrap();
    assert_eq!(removed, a);
    let ep = store.epoch();
    assert!(store.dir_lookup_epoch(&ep, root, b"b").unwrap().is_none());
    drop(ep);

    // Directory type rules through the overlay: cannot rmdir a file.
    assert!(store.epoch_unlink(root, b"missing", true, hooks).is_err());
    // Cannot rename a file over a directory.
    let d = store
        .epoch_create(root, b"d", NewEntry::dir(0o755, 1000, 1000), hooks)
        .unwrap();
    let e = store
        .epoch_create(root, b"e", NewEntry::file(0o644, 1000, 1000), hooks)
        .unwrap();
    assert!(store.epoch_rename(root, b"e", root, b"d", hooks).is_err());
    // Cannot rmdir a non-empty directory: put a file inside d.
    let inner = store
        .epoch_create(d, b"f", NewEntry::file(0o644, 1000, 1000), hooks)
        .unwrap();
    assert!(store.epoch_unlink(d, b"f", true, hooks).is_err());
    // Removing the inner file then rmdir works.
    store.epoch_unlink(d, b"f", false, hooks).unwrap();
    assert!(store.epoch_unlink(d, b"f", true, hooks).is_err()); // gone now
    store.epoch_unlink(root, b"d", true, hooks).unwrap();
    let _ = (e, inner);

    // Checkpoint + verify + fsck.
    store.epoch_checkpoint(hooks).unwrap();
    assert!(store.dir_lookup(root, b"a").unwrap().is_none());
    assert!(store.dir_lookup(root, b"c").unwrap().is_none());
    assert!(store.dir_lookup(root, b"b").unwrap().is_none());
    assert!(store.dir_lookup(root, b"d").unwrap().is_none());
    assert!(store.get_inode(a).unwrap().is_none(), "removed inode");
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn epoch_crash_recovery_replays_uncheckpointed_log() {
    let dir = TempDir::new().unwrap();
    let hooks = &CrashHooks::none();

    let a;
    let data = drift_text(3);
    {
        let store = create_store(&dir);
        let root = store.current_root().root_dir_ino;
        // Epoch ops WITHOUT a checkpoint, then the store is dropped (a
        // process crash: the log envelopes + objects are page-cache
        // durable; the committed trees still describe the initial root).
        a = store
            .epoch_create(root, b"a", NewEntry::file(0o644, 1000, 1000), hooks)
            .unwrap();
        store
            .epoch_write(
                a,
                0,
                &data,
                OptimizeOptions::default(),
                ForegroundPolicy::full(),
                hooks,
            )
            .unwrap();
        store
            .epoch_create(root, b"b", NewEntry::dir(0o755, 1000, 1000), hooks)
            .unwrap();
        // Intentionally NO checkpoint: the crash leaves the log tail.
    }

    // Reopen: the log replays, so the acknowledged ops survive.
    let store = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    let root = store.current_root().root_dir_ino;
    assert_eq!(store.read_file(a, 0, data.len() as u64).unwrap(), data);
    assert!(store.dir_lookup(root, b"b").unwrap().is_some());
    // The replay committed a checkpoint root with the consumed sequence.
    assert!(store.current_root().log_seq > 0, "replay consumed the log");
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());

    // A second open (after the replay's checkpoint) must not re-apply
    // anything (idempotent recovery).
    drop(store);
    let store = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    assert_eq!(store.read_file(a, 0, data.len() as u64).unwrap(), data);
    assert!(store.dir_lookup(root, b"b").unwrap().is_some());
}

#[test]
fn epoch_checkpoint_after_replay_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let hooks = &CrashHooks::none();

    let a;
    let data = noise(2 * 65536, 0xabcd);
    {
        let store = create_store(&dir);
        let root = store.current_root().root_dir_ino;
        a = store
            .epoch_create(root, b"a", NewEntry::file(0o644, 1000, 1000), hooks)
            .unwrap();
        store
            .epoch_write(
                a,
                0,
                &data,
                OptimizeOptions::default(),
                ForegroundPolicy::full(),
                hooks,
            )
            .unwrap();
        // drop without checkpoint (crash).
    }
    let store = Store::open(dir.path(), &StoreConfig::default()).unwrap();
    assert_eq!(store.read_file(a, 0, data.len() as u64).unwrap(), data);
    // A further explicit checkpoint is a no-op (the replay already
    // checkpointed) and the state stays byte-exact.
    store.epoch_checkpoint(hooks).unwrap();
    assert_eq!(store.read_file(a, 0, data.len() as u64).unwrap(), data);
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn epoch_flushes_before_gc() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let hooks = &CrashHooks::none();
    let root = root_ino(&store);

    // Epoch state with staged objects that only the log references.
    let a = store
        .epoch_create(root, b"a", NewEntry::file(0o644, 1000, 1000), hooks)
        .unwrap();
    let data = noise(65536, 0x1234);
    store
        .epoch_write(
            a,
            0,
            &data,
            OptimizeOptions::default(),
            ForegroundPolicy::full(),
            hooks,
        )
        .unwrap();

    // GC must flush the epoch first (its reachability walk only sees
    // committed roots; the staged objects would look like garbage).
    crate::store::gc::collect(&store, hooks).unwrap();
    assert!(store.epoch().is_empty(), "GC flushed the epoch");
    assert_eq!(store.read_file(a, 0, data.len() as u64).unwrap(), data);
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn epoch_sequential_writes_form_chains_and_stay_exact() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let hooks = &CrashHooks::none();
    let root = root_ino(&store);

    let a = store
        .epoch_create(root, b"a", NewEntry::file(0o644, 1000, 1000), hooks)
        .unwrap();
    // Sequential appends: each write's RMW sees the epoch's pending bytes.
    let mut data = Vec::new();
    for i in 0..6 {
        let chunk = drift_text(1);
        let off = (i * 65536) as u64;
        store
            .epoch_write(
                a,
                off,
                &chunk,
                OptimizeOptions::default(),
                ForegroundPolicy::full(),
                hooks,
            )
            .unwrap();
        data.extend_from_slice(&chunk);
    }
    // A partial overwrite in the middle must compose with the epoch state.
    let patch = b"PATCHED-CONTENT".to_vec();
    store
        .epoch_write(
            a,
            100,
            &patch,
            OptimizeOptions::default(),
            ForegroundPolicy::full(),
            hooks,
        )
        .unwrap();
    data[100..100 + patch.len()].copy_from_slice(&patch);

    let ep = store.epoch();
    let before = store.read_file_epoch(&ep, a, 0, data.len() as u64).unwrap();
    assert_eq!(before, data, "overlay read before the checkpoint");
    drop(ep);

    store.epoch_checkpoint(hooks).unwrap();
    assert_eq!(
        store.read_file(a, 0, data.len() as u64).unwrap(),
        data,
        "committed read after the checkpoint"
    );
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}

#[test]
fn epoch_duplicate_content_dedups_at_checkpoint() {
    let dir = TempDir::new().unwrap();
    let store = create_store(&dir);
    let hooks = &CrashHooks::none();
    let root = root_ino(&store);

    // Two files with IDENTICAL content written through the epoch.
    let chunk = noise(65536, 0xfeed);
    let a = store
        .epoch_create(root, b"a", NewEntry::file(0o644, 1000, 1000), hooks)
        .unwrap();
    let b = store
        .epoch_create(root, b"b", NewEntry::file(0o644, 1000, 1000), hooks)
        .unwrap();
    for (ino, _) in [(a, 0u64), (b, 0u64)] {
        store
            .epoch_write(
                ino,
                0,
                &chunk,
                OptimizeOptions::default(),
                ForegroundPolicy::full(),
                hooks,
            )
            .unwrap();
    }
    store.epoch_checkpoint(hooks).unwrap();
    assert_eq!(store.read_file(a, 0, chunk.len() as u64).unwrap(), chunk);
    assert_eq!(store.read_file(b, 0, chunk.len() as u64).unwrap(), chunk);
    // The chunk index has ONE descriptor for the content (the second
    // epoch write re-encoded it, but the checkpoint's chunk-index merge
    // keeps the first pending occurrence).
    let cid = crate::core::extent::ChunkId::of(&chunk);
    let descs = crate::store::index::scan_all(
        store.current_root().chunk_index_root,
        crate::store::BTREE_ORDER,
        store.limits().max_fanout,
        &store,
    )
    .unwrap();
    assert_eq!(
        descs
            .iter()
            .filter(|(k, _)| k.as_slice() == cid.as_bytes())
            .count(),
        1,
        "the checkpoint must merge duplicate chunk-index entries"
    );
    let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());
}
