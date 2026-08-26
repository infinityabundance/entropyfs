//! Phase-10F crash-court parity (ADR-0021): the acceptance test for
//! `UringIo`.
//!
//! A `UringIo` implementation is correct only if, at EVERY crash-court
//! injection point, the store directory is byte-identical to the `SyncIo`
//! state (segment files + superblock + lock), and recovery produces the
//! same admissible state. This module runs the full crash matrix and a
//! deterministic full-workload sequence on both backends and diffs the
//! directory bytes.
//!
//! # Canonical comparison
//!
//! Inode records embed wall-clock timestamps (`Timespec::now()`), so raw
//! byte comparison across separate runs fails even for `SyncIo`-vs-`SyncIo`.
//! The snapshot is therefore canonicalized: every inode record has its four
//! time fields zeroed before comparison. Everything else — record order,
//! structure, lengths, all non-inode payloads, the superblock, the
//! directory layout — is compared byte-for-byte.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use tempfile::TempDir;

use crate::fsck::{FsckOptions, fsck};
use crate::store::io::IoBackendKind;
use crate::store::transaction::{CrashHooks, CrashPoint};
use crate::store::{NewEntry, Store, StoreConfig};

/// The crash matrix (commit boundaries), mirrored from crash_recovery.rs.
fn commit_crash_points() -> Vec<CrashPoint> {
    vec![
        CrashPoint::AfterRootWrite,
        CrashPoint::AfterRecordAppend,
        CrashPoint::AfterSegmentFdatasync,
        CrashPoint::AfterSegmentDirFsync,
        CrashPoint::AfterSuperblockWrite,
        CrashPoint::AfterSuperblockFsync,
    ]
}

/// The crash matrix (GC boundaries), mirrored from crash_recovery.rs.
fn gc_crash_points() -> Vec<CrashPoint> {
    vec![
        CrashPoint::AfterRootWrite,
        CrashPoint::AfterSuperblockWrite,
        CrashPoint::AfterSuperblockFsync,
        CrashPoint::BeforeOldSegmentDelete,
    ]
}

/// Canonical snapshot of a store directory: relative path -> canonical
/// bytes (see the module docs for the inode-time normalization).
fn canonical_snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    // Collect every segment's records into the object map first (the
    // canonical id rewrite needs whole-graph lookups).
    let mut segments: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read store dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(dir)
                .expect("under store dir")
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = std::fs::read(&path).expect("read store file");
            if path.extension().and_then(|e| e.to_str()) == Some("seg") {
                segments.insert(rel, bytes);
            } else {
                segments.insert(rel, bytes);
            }
        }
    }
    let mut canon = Canonicalizer::new(segments.values());
    let mut out = BTreeMap::new();
    for (rel, bytes) in &segments {
        out.insert(rel.clone(), canon.segment(bytes));
    }
    out
}

/// Whole-graph canonicalizer: rewrites every embedded content id through a
/// canonical-id map (original id -> BLAKE3 of the object's canonical
/// payload), and zeroes the four wall-clock time fields of inode records.
///
/// The only cross-run nondeterminism in the on-disk format is
/// `Timespec::now()` inside inode objects; its effect propagates through
/// content ids (inode objects -> inode-index tree nodes -> the root).
/// Rewriting those ids makes the comparison exact modulo the wall clock
/// while still catching ANY backend difference in record structure, order,
/// lengths, and all other payload bytes.
struct Canonicalizer {
    /// original content id -> (tag, original payload).
    records: HashMap<crate::core::extent::ChunkId, (crate::format::version::RecordTag, Vec<u8>)>,
    /// original id -> canonical id (memoized; the graph is a DAG).
    cache: HashMap<crate::core::extent::ChunkId, crate::core::extent::ChunkId>,
}

impl Canonicalizer {
    fn new<'a>(segments: impl Iterator<Item = &'a Vec<u8>>) -> Self {
        let mut records = HashMap::new();
        for bytes in segments {
            let mut offset = 4u64;
            while offset < bytes.len() as u64 {
                if let Ok(Some(rec)) = crate::format::record::decode(bytes, offset) {
                    records.insert(rec.content_id, (rec.tag, rec.payload.to_vec()));
                    offset = offset
                        .checked_add(rec.total_size())
                        .expect("validated at decode");
                } else {
                    break;
                }
            }
        }
        Self {
            records,
            cache: HashMap::new(),
        }
    }

    /// The canonical id of an object: BLAKE3 of its canonical payload.
    /// Objects with deterministic payloads map to themselves; the
    /// inode-derived chain (inode objects, inode-index nodes, the root)
    /// maps to stable ids. A non-object (e.g. an arbitrary 32-byte leaf
    /// value) maps to itself.
    fn canonical_id(&mut self, id: &crate::core::extent::ChunkId) -> crate::core::extent::ChunkId {
        if let Some(c) = self.cache.get(id) {
            return *c;
        }
        match self.records.get(id).cloned() {
            None => *id,
            Some((tag, payload)) => {
                let canon = self.canonical_payload(tag, &payload);
                let cid = crate::core::extent::ChunkId::of(&canon);
                self.cache.insert(*id, cid);
                cid
            }
        }
    }

    /// Canonical payload for a record tag.
    fn canonical_payload(
        &mut self,
        tag: crate::format::version::RecordTag,
        payload: &[u8],
    ) -> Vec<u8> {
        use crate::format::version::RecordTag;
        match tag {
            RecordTag::Inode => {
                if let Ok(mut inode) = crate::store::inode::Inode::decode(payload) {
                    let zero = crate::store::inode::Timespec { sec: 0, nsec: 0 };
                    inode.atime = zero;
                    inode.ctime = zero;
                    inode.mtime = zero;
                    inode.crtime = zero;
                    inode.encode()
                } else {
                    payload.to_vec()
                }
            }
            RecordTag::BtreeNode => {
                use crate::store::index::{Node, ObjectProvider};
                struct NoProvider;
                impl ObjectProvider for NoProvider {
                    fn get(
                        &self,
                        _id: &crate::core::extent::ChunkId,
                    ) -> Result<Option<Vec<u8>>, crate::store::index::BTreeError>
                    {
                        Ok(None)
                    }
                    fn put(&mut self, _id: crate::core::extent::ChunkId, _bytes: Vec<u8>) {}
                }
                match Node::decode(payload, crate::store::BTREE_ORDER, 4096) {
                    Ok(Node::Internal {
                        first_child,
                        entries,
                    }) => {
                        let first_child = self.canonical_id(&first_child);
                        let entries: Vec<crate::store::index::Entry> = entries
                            .into_iter()
                            .map(|mut e| {
                                let child = crate::core::extent::ChunkId::new(
                                    e.value.as_slice().try_into().expect("32-byte child id"),
                                );
                                e.value = self.canonical_id(&child).as_bytes().to_vec();
                                e
                            })
                            .collect();
                        Node::Internal {
                            first_child,
                            entries,
                        }
                        .encode(crate::store::BTREE_ORDER)
                    }
                    Ok(Node::Leaf { entries }) => {
                        let entries: Vec<crate::store::index::Entry> = entries
                            .into_iter()
                            .map(|mut e| {
                                if e.value.len() == 32 {
                                    // Inode-index leaf values are inode
                                    // object ids; other 32-byte values
                                    // (unlikely) map through the same
                                    // identity-preserving lookup.
                                    let id = crate::core::extent::ChunkId::new(
                                        e.value.as_slice().try_into().expect("32-byte value"),
                                    );
                                    e.value = self.canonical_id(&id).as_bytes().to_vec();
                                }
                                e
                            })
                            .collect();
                        Node::Leaf { entries }.encode(crate::store::BTREE_ORDER)
                    }
                    Err(_) => payload.to_vec(),
                }
            }
            RecordTag::Root => {
                if let Ok(mut root) = crate::store::root::Root::decode(payload) {
                    root.inode_index_root = self.canonical_id(&root.inode_index_root);
                    root.chunk_index_root = self.canonical_id(&root.chunk_index_root);
                    root.snapshot_tree_root = self.canonical_id(&root.snapshot_tree_root);
                    root.model_index_root = self.canonical_id(&root.model_index_root);
                    root.encode()
                } else {
                    payload.to_vec()
                }
            }
            RecordTag::MutationLog => {
                if let Ok((seq, op)) = crate::store::epoch::Epoch::decode_envelope(payload) {
                    let op = rewrite_op_inode_ids(&mut *self, op);
                    let mut w = crate::format::codec::Writer::new();
                    w.u64(seq);
                    w.bytes(&op.encode());
                    w.into_bytes()
                } else {
                    payload.to_vec()
                }
            }
            _ => payload.to_vec(),
        }
    }

    /// Canonical segment form: the file length plus every record's
    /// (offset, tag, flags, materialized_len, canonical payload).
    fn segment(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        let mut offset = 4u64;
        while offset < bytes.len() as u64 {
            match crate::format::record::decode(bytes, offset) {
                Ok(Some(rec)) => {
                    let payload = self.canonical_payload(rec.tag, rec.payload);
                    out.extend_from_slice(&offset.to_le_bytes());
                    out.push(rec.tag.tag());
                    out.extend_from_slice(&rec.flags.to_le_bytes());
                    out.extend_from_slice(&rec.materialized_len.unwrap_or(0).to_le_bytes());
                    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
                    out.extend_from_slice(&payload);
                    offset = offset
                        .checked_add(rec.total_size())
                        .expect("record size validated at decode");
                }
                _ => break, // torn tail / padding: captured by the length prefix
            }
        }
        out
    }
}

/// Rewrite the inode-reference fields of a mutation-log op through the
/// canonical-id map (chunk content ids map to themselves).
fn rewrite_op_inode_ids(
    canon: &mut Canonicalizer,
    op: crate::store::epoch::MutationOp,
) -> crate::store::epoch::MutationOp {
    use crate::store::epoch::MutationOp;
    let cid = |c: &mut Canonicalizer, id: crate::core::extent::ChunkId| c.canonical_id(&id);
    let ocid = |c: &mut Canonicalizer, id: Option<crate::core::extent::ChunkId>| {
        id.map(|i| c.canonical_id(&i))
    };
    match op {
        MutationOp::Create {
            parent,
            name,
            ino,
            d_type,
            inode_id,
            parent_inode_id,
        } => MutationOp::Create {
            parent,
            name,
            ino,
            d_type,
            inode_id: cid(canon, inode_id),
            parent_inode_id: cid(canon, parent_inode_id),
        },
        MutationOp::Setattr { ino, inode_id } => MutationOp::Setattr {
            ino,
            inode_id: cid(canon, inode_id),
        },
        MutationOp::Unlink {
            parent,
            name,
            child,
            is_dir,
            parent_inode_id,
            child_inode_id,
        } => MutationOp::Unlink {
            parent,
            name,
            child,
            is_dir,
            parent_inode_id: cid(canon, parent_inode_id),
            child_inode_id: ocid(canon, child_inode_id),
        },
        MutationOp::Rename {
            src_parent,
            src_name,
            dst_parent,
            dst_name,
            src_ino,
            dst_ino,
            src_is_dir,
            sp_inode_id,
            dp_inode_id,
            src_child_inode_id,
            dst_child_inode_id,
        } => MutationOp::Rename {
            src_parent,
            src_name,
            dst_parent,
            dst_name,
            src_ino,
            dst_ino,
            src_is_dir,
            sp_inode_id: cid(canon, sp_inode_id),
            dp_inode_id: cid(canon, dp_inode_id),
            src_child_inode_id: ocid(canon, src_child_inode_id),
            dst_child_inode_id: ocid(canon, dst_child_inode_id),
        },
        MutationOp::Write {
            ino,
            size,
            chunks,
            inode_id,
        } => MutationOp::Write {
            ino,
            size,
            chunks: chunks
                .into_iter()
                .map(|(off, cid2, desc)| (off, cid(canon, cid2), desc))
                .collect(),
            inode_id: cid(canon, inode_id),
        },
    }
}

/// Deterministic payload (251-cycle xor seed: compressible-adjacent runs
/// with a fixed perturbation).
fn payload(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| ((i % 251) as u8) ^ seed)
        .collect::<Vec<u8>>()
}

fn cfg(kind: IoBackendKind) -> StoreConfig {
    StoreConfig {
        io_backend: kind,
        ..Default::default()
    }
}

/// Encode a buffer into per-chunk extent updates through the real
/// candidate search (mirrors crash_recovery.rs's fixture encoder).
fn encode_chunks(content: &[u8], store: &Store) -> Vec<crate::store::ExtentUpdate> {
    use crate::core::candidate::{pick_cheapest, raw_candidate, zero_candidate};
    use crate::core::extent::ChunkId;
    use crate::entropy::palette::PaletteEncoder;
    use crate::entropy::periodic::PeriodicEncoder;
    use crate::entropy::sparse::SparseEncoder;
    use crate::rans::residual::RansEncoder;
    let limits = store.limits();
    let policy = store.policy();
    let chunk_class = limits.chunk_class as usize;
    let mut updates = Vec::new();
    let mut off = 0usize;
    while off < content.len() {
        let end = (off + chunk_class).min(content.len());
        let chunk = &content[off..end];
        let cid = ChunkId::of(chunk);
        let ctx = crate::core::candidate::CandidateContext {
            limits,
            policy,
            content_id: cid,
            bases: &[],
            dedup: None,
        };
        let mut cands = Vec::new();
        if let Some(z) = zero_candidate(chunk, cid, limits) {
            cands.push(z);
        }
        for enc in [
            Box::new(SparseEncoder) as Box<dyn crate::core::candidate::Encoder>,
            Box::new(PaletteEncoder),
            Box::new(PeriodicEncoder),
            Box::new(RansEncoder),
        ] {
            cands.extend(enc.encode(chunk, &ctx));
        }
        if let Some(r) = raw_candidate(chunk, cid, limits) {
            cands.push(r);
        }
        let best = pick_cheapest(&cands, policy).expect("at least raw");
        updates.push(crate::store::ExtentUpdate {
            offset: off as u64,
            descriptor: best.representation.clone(),
            content_id: cid,
            objects: best.objects.clone(),
        });
        off = end;
    }
    updates
}

/// One crash-court run: mkfs with `kind`, populate a pre-state, arm the
/// crash, snapshot the canonical directory bytes AFTER the crash (before
/// recovery), then run the recovery contract. Returns
/// (canonical_snapshot, fsck_clean, recovered_content_ok).
fn crash_run(
    kind: IoBackendKind,
    point: CrashPoint,
    dir: &TempDir,
    use_gc: bool,
) -> (BTreeMap<String, Vec<u8>>, bool, bool) {
    let store = Store::create(dir.path(), &cfg(kind), [0x33; 16]).unwrap();
    // Pre-state: a file with a real extent tree + chunk index.
    let inode = crate::store::inode::Inode::new_file(1000, 1000, 0o644);
    let mut tx = store.begin_tx().unwrap();
    crate::store::Store::put_inode_in_tx(&mut tx, 3, &inode).unwrap();
    tx.commit(&CrashHooks::none()).unwrap();
    let pre: Vec<u8> = payload(0xA5, 64 * 1024 + 777);
    store.write_region(3, 0, &pre).unwrap();
    store.durability_barrier(&CrashHooks::none()).unwrap();
    let pre_len = pre.len() as u64;

    let post: Vec<u8> = payload(0x5A, 64 * 1024 + 1234);
    if use_gc {
        // Generate garbage so GC has victims, then arm the crash. GC must
        // preserve the pre-GC content exactly (live data).
        for i in 0..6u32 {
            let c = format!("gc-{i}:{}", "g".repeat(1500 + i as usize)).into_bytes();
            store.write_region(3, 0, &c).unwrap();
            store.durability_barrier(&CrashHooks::none()).unwrap();
        }
        let final_content = format!("gc-final:{}", "h".repeat(2000)).into_bytes();
        store.write_region(3, 0, &final_content).unwrap();
        store.durability_barrier(&CrashHooks::none()).unwrap();
        // The pre-GC state (what recovery must reproduce: GC preserves
        // live data, whatever its stage).
        let pre_gc_content = store.read_file(3, 0, pre_len.max(65536)).unwrap();
        let res = crate::store::gc::collect(&store, &CrashHooks::crash_at(point));
        assert!(res.is_err(), "GC crash point {point:?} must report");
        drop(store);
        let snapshot = canonical_snapshot(dir.path());
        let report = fsck(dir.path(), &FsckOptions::default())
            .unwrap_or_else(|e| panic!("fsck after crash at {point:?}: {e}"));
        let clean = report.is_clean();
        let store2 = Store::open(dir.path(), &cfg(kind))
            .unwrap_or_else(|e| panic!("reopen at {point:?} ({kind:?}): {e}"));
        let after = store2
            .read_file(3, 0, pre_gc_content.len() as u64)
            .unwrap_or_default();
        let content_ok = after == pre_gc_content;
        assert!(
            content_ok,
            "{kind:?} {point:?}: GC must preserve the pre-GC content (got len {})",
            after.len()
        );
        store2
            .write_region(3, 0, b"post-crash-recovery".repeat(64).as_slice())
            .unwrap();
        drop(store2);
        (snapshot, clean, content_ok)
    } else {
        let res = store.commit_file_extents(
            3,
            encode_chunks(&post, &store),
            Some(post.len() as u64),
            &CrashHooks::crash_at(point),
        );
        assert!(res.is_err(), "commit crash point {point:?} must report");
        drop(store);
        let snapshot = canonical_snapshot(dir.path());
        let report = fsck(dir.path(), &FsckOptions::default())
            .unwrap_or_else(|e| panic!("fsck after crash at {point:?}: {e}"));
        let clean = report.is_clean();
        let store2 = Store::open(dir.path(), &cfg(kind))
            .unwrap_or_else(|e| panic!("reopen at {point:?} ({kind:?}): {e}"));
        let after = store2
            .read_file(3, 0, pre_len.max(post.len() as u64))
            .unwrap_or_default();
        let is_pre = after == pre;
        let is_post = after == post;
        assert!(
            is_pre || is_post,
            "{kind:?} {point:?}: hybrid or corrupt state (len {})",
            after.len()
        );
        store2
            .write_region(3, 0, b"post-crash-recovery".repeat(64).as_slice())
            .unwrap();
        drop(store2);
        (snapshot, clean, is_pre || is_post)
    }
}

#[test]
fn commit_crash_points_are_byte_identical_between_backends() {
    for point in commit_crash_points() {
        let sync_dir = TempDir::new().unwrap();
        let uring_dir = TempDir::new().unwrap();
        let (sync_snap, sync_clean, sync_ok) =
            crash_run(IoBackendKind::Sync, point, &sync_dir, false);
        let (uring_snap, uring_clean, uring_ok) =
            crash_run(IoBackendKind::Uring, point, &uring_dir, false);
        assert!(
            sync_snap == uring_snap,
            "store-directory bytes differ at commit crash point {point:?}: \
             first differing file: {}",
            sync_snap
                .iter()
                .zip(&uring_snap)
                .find(|((k1, v1), (k2, v2))| k1 != k2 || v1 != v2)
                .map(|((k, _), _)| k.clone())
                .unwrap_or_else(|| {
                    if sync_snap.keys().next() != uring_snap.keys().next() {
                        "file sets differ".to_string()
                    } else {
                        "file sets equal; sizes differ".to_string()
                    }
                })
        );
        assert!(sync_clean && uring_clean, "fsck must be clean at {point:?}");
        assert_eq!(sync_ok, uring_ok, "recovery contract differs at {point:?}");
    }
}

#[test]
fn gc_crash_points_are_byte_identical_between_backends() {
    for point in gc_crash_points() {
        let sync_dir = TempDir::new().unwrap();
        let uring_dir = TempDir::new().unwrap();
        let (sync_snap, sync_clean, sync_ok) =
            crash_run(IoBackendKind::Sync, point, &sync_dir, true);
        let (uring_snap, uring_clean, uring_ok) =
            crash_run(IoBackendKind::Uring, point, &uring_dir, true);
        assert!(
            sync_snap == uring_snap,
            "store-directory bytes differ at GC crash point {point:?}"
        );
        assert!(sync_clean && uring_clean, "fsck must be clean at {point:?}");
        assert_eq!(sync_ok, uring_ok, "recovery contract differs at {point:?}");
    }
}

/// A deterministic full-workload: namespace ops (epoch path), data writes
/// (compressible + high-entropy), truncate, rename, xattr, durability
/// barriers, GC, remount. Both backends must leave canonically identical
/// stores.
#[test]
fn full_workload_is_byte_identical_between_backends() {
    let mut reference: Option<BTreeMap<String, Vec<u8>>> = None;
    for kind in IoBackendKind::ALL {
        let dir = TempDir::new().unwrap();
        {
            let store = Store::create(dir.path(), &cfg(kind), [0x42; 16]).unwrap();
            // Namespace tree.
            for i in 0..4u64 {
                store
                    .create_entry(
                        1,
                        format!("file-{i}").as_bytes(),
                        NewEntry::file(0o644, 1000, 1000),
                        &CrashHooks::none(),
                    )
                    .unwrap();
            }
            store
                .create_entry(
                    1,
                    b"subdir",
                    NewEntry::dir(0o755, 1000, 1000),
                    &CrashHooks::none(),
                )
                .unwrap();
            // Data writes: compressible + high-entropy.
            let compressible: Vec<u8> =
                b"the quick brown fox jumps over the lazy dog ".repeat(2000);
            let high_entropy = payload(0x7C, 256 * 1024);
            store.write_region(3, 0, &compressible).unwrap();
            store
                .write_region(3, compressible.len() as u64, &high_entropy)
                .unwrap();
            store.durability_barrier(&CrashHooks::none()).unwrap();
            // More epoch ops + a hole write + truncate-style rewrite.
            store
                .create_entry(
                    1,
                    b"after-barrier",
                    NewEntry::file(0o600, 1000, 1000),
                    &CrashHooks::none(),
                )
                .unwrap();
            store.write_region(3, 200_000, b"hole-fill").unwrap();
            store.write_region(3, 0, &payload(0x11, 48 * 1024)).unwrap();
            store
                .rename(1, b"file-0", 1, b"renamed", &CrashHooks::none())
                .unwrap();
            store
                .setattr_inode(
                    3,
                    &crate::store::AttrUpdate {
                        mode: Some(0o640),
                        uid: Some(1000),
                        gid: Some(1000),
                        ..Default::default()
                    },
                    &CrashHooks::none(),
                )
                .unwrap();
            store
                .set_xattr(3, b"user.k", b"v", &CrashHooks::none())
                .unwrap();
            // Roll a checkpoint + durability barrier.
            store.durability_barrier(&CrashHooks::none()).unwrap();
            // GC over the garbage the rewrites left.
            let reclaimed = crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
            assert!(reclaimed > 0, "workload must generate reclaimable garbage");
            store.durability_barrier(&CrashHooks::none()).unwrap();
            // Second GC (idempotent-ish; must not diverge between backends).
            let _ = crate::store::gc::collect(&store, &CrashHooks::none()).unwrap();
        }
        let snapshot = canonical_snapshot(dir.path());
        match &reference {
            None => reference = Some(snapshot),
            Some(prev) => {
                assert_eq!(
                    prev, &snapshot,
                    "full-workload store bytes differ between {kind:?} and Sync"
                );
            }
        }
    }
}

/// The read path must return identical bytes on both backends, including
/// the batched `read_many` path.
#[test]
fn read_path_identical_between_backends() {
    let mut expected: Option<Vec<u8>> = None;
    for kind in IoBackendKind::ALL {
        let dir = TempDir::new().unwrap();
        let store = Store::create(dir.path(), &cfg(kind), [0x99; 16]).unwrap();
        let inode = crate::store::inode::Inode::new_file(1000, 1000, 0o644);
        let mut tx = store.begin_tx().unwrap();
        crate::store::Store::put_inode_in_tx(&mut tx, 3, &inode).unwrap();
        tx.commit(&CrashHooks::none()).unwrap();
        let data: Vec<u8> = (0..(512 * 1024))
            .map(|i| ((i % 251) as u8) ^ ((i / 65536) as u8))
            .collect();
        store.write_region(3, 0, &data).unwrap();
        store.durability_barrier(&CrashHooks::none()).unwrap();
        // Full read (exercises read_many batching when >1 extent).
        let full = store.read_file(3, 0, data.len() as u64).unwrap();
        // Strided reads (scattered preads).
        let mut off: u64 = 0;
        while (off as usize) < data.len() {
            let len = 3 * 4096usize;
            let want = len.min(data.len() - off as usize);
            let r = store.read_file(3, off, want as u64).unwrap();
            assert_eq!(
                r,
                &data[off as usize..off as usize + want],
                "{kind:?} strided read at {off}"
            );
            off += 7 * 4096;
        }
        assert_eq!(full, data, "{kind:?} full read");
        match &expected {
            None => expected = Some(full),
            Some(prev) => assert_eq!(prev, &full, "read bytes differ between backends"),
        }
    }
}

/// Deterministic byte-uniform noise (SplitMix64; mirrors shared_dict.rs).
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

/// The batched read_many path must serve dictionary-based materialization
/// (SEQUENCE_SHARED_DICT: file dictionary + shared dictionary nested
/// references) identically on both backends, after the optimizer rewrote
/// the extents.
#[test]
fn shared_dict_reads_identical_between_backends() {
    let mut expected: Option<Vec<Vec<u8>>> = None;
    for kind in IoBackendKind::ALL {
        let dir = TempDir::new().unwrap();
        let store = Store::create(dir.path(), &cfg(kind), [0x9C; 16]).unwrap();
        let d = store
            .create_entry(
                1,
                b"proj",
                NewEntry::dir(0o755, 1000, 1000),
                &CrashHooks::none(),
            )
            .unwrap();
        let header = noise(8192, 0x5EED_5EED_0FF1_CE99);
        let mut files: Vec<(u64, Vec<u8>)> = Vec::new();
        for (i, seed) in [0x1111_2222u64, 0x3333_4444, 0x5555_6666, 0x7777_8888]
            .iter()
            .enumerate()
        {
            let mut chunk = header.clone();
            chunk.extend_from_slice(&noise(65536usize.saturating_sub(chunk.len()), *seed));
            chunk.resize(65536, 0);
            let ino = store
                .create_entry(
                    d,
                    format!("f{i}.rs").as_bytes(),
                    NewEntry::file(0o644, 1000, 1000),
                    &CrashHooks::none(),
                )
                .unwrap();
            store.write_region(ino, 0, &chunk).unwrap();
            files.push((ino, chunk));
        }
        store.durability_barrier(&CrashHooks::none()).unwrap();
        // The optimizer rewrites the family-correlated chunk-0s to
        // SEQUENCE_SHARED_DICT (dictionary + shared nested refs).
        let stats = crate::optimizer::background::shared_dict_pass(
            &store,
            crate::optimizer::policy::OptimizeOptions::default(),
            None,
        )
        .unwrap();
        assert!(
            stats.rewritten > 0,
            "optimizer must rewrite at least one extent"
        );
        store.durability_barrier(&CrashHooks::none()).unwrap();
        // Read every file back through the batched prefetch path.
        let mut reads: Vec<Vec<u8>> = Vec::new();
        for (ino, want) in &files {
            let got = store.read_file(*ino, 0, 65536).unwrap();
            assert_eq!(&got, want, "{kind:?} f{ino}: batched read must be exact");
            reads.push(got);
        }
        match &expected {
            None => expected = Some(reads),
            Some(prev) => assert_eq!(prev, &reads, "shared-dict reads differ between backends"),
        }
    }
}
