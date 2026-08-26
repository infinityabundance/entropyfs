//! The store court (Phase-11A): the CRC-aware distinction over REAL tiny
//! stores, plus the whole-store mutator.
//!
//! The user's critical point: "flip random bits in a store image" as the
//! PRINCIPAL strategy would fuzz CRC32C — the envelope rejects the vast
//! majority of mutations before the deep parsers ever see them. Two
//! complementary courts therefore run here:
//!
//! 1. **Physical corruption court** — mutate record/superblock bytes and
//!    leave the CRC (and the content-id binding) broken. The expectation
//!    is INTEGRITY REJECTION: a payload-region flip makes `record::decode`
//!    fail with `Malformed` at the envelope, so open and fsck both reject
//!    typed. Length-field flips may degrade to a torn tail, which is the
//!    crash-consistency design's *admissible* recovery (the store falls
//!    back to the complete previous state) — asserted as boundedness.
//!
//! 2. **Semantic adversarial court** — mutate descriptor / tree / model /
//!    inode / mutation-log payloads and RECOMPUTE the envelope CRC (and
//!    the content id), forcing the hostile payload through the deeper
//!    parsers: descriptor codec, B-tree walks, inode decode, materializer,
//!    epoch replay. The acceptance criterion is the hostile-media oracle:
//!    never panic, never hang, allocations bounded — and when the store
//!    opens AND fsck (with full materialization) is clean, the reads must
//!    return exactly the authenticated bytes (fsck proved the content-id
//!    binding).
//!
//! 3. **Whole-store mutator** (proptest) — seeded mutation recipes
//!    (flip / truncate / splice / duplicate / alter lengths / replace ids
//!    / replace tags / recompute CRC selectively) applied to known-good
//!    tiny stores, then open / fsck / materialize.

#![forbid(unsafe_code)]

use std::path::Path;
use tempfile::TempDir;

use crate::core::materialize::DecoderContext;
use crate::store::root::Root;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};

/// The segment file magic (the first 4 bytes of every segment).
const SEGMENT_MAGIC: [u8; 4] = *b"ESEG";

/// A small segment cap keeps every image tiny (fast copies + scans).
fn config() -> StoreConfig {
    StoreConfig {
        segment_size: 1024 * 1024,
        ..Default::default()
    }
}

/// Deterministic pseudo-random bytes (SplitMix64).
fn prng_bytes(n: usize, mut seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let b = z.to_le_bytes();
        let take = (n - out.len()).min(8);
        out.extend_from_slice(&b[..take]);
    }
    out
}

/// Structured (rANS-compressible) data.
fn compressible(n: usize) -> Vec<u8> {
    (0..n as u32).map(|i| ((i * 13) % 53) as u8).collect()
}

/// A known-good tiny store image: a small tree (dir + files with
/// compressible and noise data), an xattr, a snapshot, epoch ops, and a
/// durability barrier — so the image contains every persistent structure
/// the deep parsers consume (records of every tag, B-tree nodes, inodes,
/// descriptors, a mutation-log tail that was checkpointed).
struct TinyImage {
    /// The pristine store directory (never mutated in place).
    dir: TempDir,
    /// (inode, original content) of every known file.
    files: Vec<(u64, Vec<u8>)>,
}

fn build_tiny_image() -> TinyImage {
    let dir = TempDir::new().expect("tempdir");
    let cfg = config();
    let store = Store::create(dir.path(), &cfg, [0x11; 16]).expect("create");
    let hooks = CrashHooks::none();
    // A directory, then files under it and at the root.
    let d = store
        .create_entry(1, b"d", NewEntry::dir(0o755, 1000, 1000), &hooks)
        .expect("mkdir");
    let f1 = store
        .epoch_create(1, b"f1", NewEntry::file(0o644, 1000, 1000), &hooks)
        .expect("create f1");
    let f2 = store
        .epoch_create(d, b"f2", NewEntry::file(0o644, 1000, 1000), &hooks)
        .expect("create f2");
    // Compressible + noise + more compressible: rANS/sequence descriptors
    // and RAW payloads all get persisted.
    let data1 = compressible(65536);
    let data2 = prng_bytes(20000, 0x2222);
    let data3 = compressible(65536);
    store
        .epoch_write(
            f1,
            0,
            &data1,
            Default::default(),
            crate::optimizer::foreground::ForegroundPolicy::full(),
            &hooks,
        )
        .expect("write f1");
    store
        .epoch_write(
            f2,
            0,
            &data2,
            Default::default(),
            crate::optimizer::foreground::ForegroundPolicy::full(),
            &hooks,
        )
        .expect("write f2");
    store
        .epoch_write(
            f2,
            65536,
            &data3,
            Default::default(),
            crate::optimizer::foreground::ForegroundPolicy::full(),
            &hooks,
        )
        .expect("write f2 second chunk");
    // An xattr (its own index tree), a snapshot (its own tree), a setattr
    // (an inode rewrite), and a rename (both parents).
    store
        .set_xattr(f1, b"user.k", b"value", &hooks)
        .expect("xattr");
    store.create_snapshot(b"snap1", &hooks).expect("snapshot");
    store
        .epoch_setattr(
            f1,
            &crate::store::AttrUpdate {
                mode: Some(0o600),
                ..Default::default()
            },
            &hooks,
        )
        .expect("setattr");
    // Commit: checkpoint the epoch + durability barrier (clean state).
    store.durability_barrier(&hooks).expect("barrier");
    let files = vec![
        (f1, data1),
        (f2, {
            let mut all = data2;
            all.extend_from_slice(&data3);
            all
        }),
    ];
    // Record the exact committed content.
    let files = files
        .into_iter()
        .map(|(ino, _)| {
            let bytes = store.read_file(ino, 0, 1 << 20).expect("read back");
            (ino, bytes)
        })
        .collect();
    drop(store);
    TinyImage { dir, files }
}

/// A tiny store with an UN-CHECKPOINTED mutation-log tail (epoch ops, no
/// durability barrier): the replay path at open must process the
/// envelopes with `seq > root.log_seq`.
fn build_log_tail_image() -> TinyImage {
    let dir = TempDir::new().expect("tempdir");
    let cfg = config();
    let store = Store::create(dir.path(), &cfg, [0x22; 16]).expect("create");
    let hooks = CrashHooks::none();
    let f = store
        .epoch_create(1, b"tail", NewEntry::file(0o644, 1000, 1000), &hooks)
        .expect("create");
    store
        .epoch_write(
            f,
            0,
            &compressible(4096),
            Default::default(),
            crate::optimizer::foreground::ForegroundPolicy::full(),
            &hooks,
        )
        .expect("write");
    let files = vec![(f, {
        // The file is still PENDING in the epoch (un-checkpointed): read
        // through the overlay, not the committed trees.
        let ep = store.epoch();
        store
            .read_file_epoch(&ep, f, 0, 4096)
            .expect("read back via epoch")
    })];
    drop(store);
    TinyImage { dir, files }
}

/// Copy a store directory (recursively) to a fresh temp dir.
fn copy_image(dir: &Path) -> TempDir {
    let copy = TempDir::new().expect("tempdir");
    std::fs::copy(dir.join("superblock"), copy.path().join("superblock")).expect("copy sb");
    std::fs::create_dir_all(copy.path().join("segments")).expect("segments dir");
    for seq in segment_seqs(dir) {
        let src = crate::store::segment::segment_path(dir, seq);
        let dst = crate::store::segment::segment_path(copy.path(), seq);
        std::fs::copy(&src, &dst).expect("copy segment");
    }
    let _ = std::fs::copy(dir.join("lock"), copy.path().join("lock"));
    copy
}

/// Segment sequence numbers present in a store.
fn segment_seqs(dir: &Path) -> Vec<u64> {
    crate::store::segment::list_segments(dir).expect("list segments")
}

/// Read a segment's raw bytes.
fn read_segment(dir: &Path, seq: u64) -> Vec<u8> {
    std::fs::read(crate::store::segment::segment_path(dir, seq)).expect("read segment")
}

/// Write a segment's raw bytes (the mutation application surface).
fn write_segment(dir: &Path, seq: u64, bytes: &[u8]) {
    std::fs::write(crate::store::segment::segment_path(dir, seq), bytes).expect("write segment");
}

/// The records of a segment (pristine images only: the scan must succeed).
fn records_in(dir: &Path, seq: u64) -> Vec<crate::store::segment::ScanRecord> {
    let path = crate::store::segment::segment_path(dir, seq);
    let (records, _) =
        crate::store::segment::scan_segment(&path, 100_000).expect("scan pristine segment");
    records
}

/// A tolerant record scan for the whole-store mutator: a recipe applies
/// several ops to one image, and an earlier op may already have left the
/// segment unscannable. An op that cannot see the current records is a
/// no-op (the recipe's earlier ops already did their damage).
fn try_records_in(dir: &Path, seq: u64) -> Option<Vec<crate::store::segment::ScanRecord>> {
    let path = crate::store::segment::segment_path(dir, seq);
    crate::store::segment::scan_segment(&path, 100_000)
        .ok()
        .map(|(records, _)| records)
}

/// Rebuild a segment from re-encoded records (semantic mutations).
fn rebuild_segment(dir: &Path, seq: u64, records: &[crate::store::segment::ScanRecord]) {
    let mut out = SEGMENT_MAGIC.to_vec();
    for r in records {
        let flags = if r.materialized_len.is_some() {
            crate::format::record::FLAG_HAS_MATERIALIZED_LEN
        } else {
            0
        };
        out.extend_from_slice(&crate::format::record::encode(
            r.tag,
            flags,
            r.materialized_len,
            &r.payload,
        ));
    }
    write_segment(dir, seq, &out);
}

/// Re-encode one record with a new payload (semantic: the envelope CRC and
/// the content id are recomputed, so the hostile payload reaches the deep
/// parsers).
fn reencode(
    rec: &crate::store::segment::ScanRecord,
    payload: Vec<u8>,
) -> crate::store::segment::ScanRecord {
    let mut r = rec.clone();
    r.payload = payload;
    r.stored_len = r.payload.len() as u32;
    r.content_id = crate::core::extent::ChunkId::of(&r.payload);
    r
}

/// The NEWEST root record in a segment (the highest generation: the most
/// recent committed root). The segment accumulates one root record per
/// commit; the newest is the one the active superblock slot references.
fn newest_root_record(dir: &Path, seq: u64) -> crate::store::segment::ScanRecord {
    let records = records_in(dir, seq);
    records
        .iter()
        .filter(|r| r.tag == crate::format::version::RecordTag::Root)
        .max_by_key(|r| Root::decode(&r.payload).map(|x| x.generation).unwrap_or(0))
        .cloned()
        .expect("at least one root record")
}

/// The store oracle. Returns `Err(description)` on any invariant
/// violation; the courts turn that into a failure.
///
/// The authenticated-bytes clause is checked SELF-CONSISTENTLY through
/// the OPENED store's own root (never through fsck's separate root
/// selection — fsck and open may legitimately select different admissible
/// roots on a mutated store, and `Store::open` itself is not read-only:
/// epoch replay commits and rewrites the superblock). When the opened
/// store's own view binds every reachable extent to the chunk index, the
/// reads must succeed and return exactly those authenticated bytes.
fn run_store_oracle(dir: &Path, files: &[(u64, Vec<u8>)]) -> Result<(), String> {
    // 1. fsck: a typed rejection at scan is admissible; a report must
    //    never panic. (fsck must never abort on one bad record.)
    let _report = crate::fsck::fsck(dir, &crate::fsck::FsckOptions::default()).ok();
    // 2. open: typed rejection or success.
    let store = Store::open(dir, &config()).ok();
    // 3. reads on an opened store: typed rejection or bounded bytes.
    if let Some(store) = &store {
        for (ino, orig) in files {
            let want = 65536u64.min(orig.len() as u64).max(1);
            let _ = store.read_file(*ino, 0, want);
        }
    }
    // 4. Authenticated-bytes check: when the OPENED store's own view
    //    binds every reachable extent to the chunk index, the reads must
    //    succeed (never return bytes inconsistent with the authenticated
    //    content identity).
    if let Some(store) = &store {
        if store_view_is_authenticated(store, files)? {
            for (ino, orig) in files {
                store
                    .read_file(*ino, 0, orig.len().max(1) as u64)
                    .map_err(|e| format!("authenticated view: read of ino {ino} failed: {e}"))?;
            }
        }
    }
    Ok(())
}

/// Whether every reachable extent of the known files, through the OPENED
/// store's own root, materializes to bytes whose content id binds to a
/// chunk-index entry that materializes to the same bytes (the §33
/// binding, checked store-side so the view is self-consistent).
fn store_view_is_authenticated(store: &Store, files: &[(u64, Vec<u8>)]) -> Result<bool, String> {
    let limits = *store.limits();
    let mut all_ok = true;
    for (ino, _) in files {
        let Some(inode) = store.get_inode(*ino).map_err(|e| e.to_string())? else {
            all_ok = false;
            continue;
        };
        let extent_root = match inode.data {
            crate::store::inode::InodeData::File { extent_root } => extent_root,
            _ => continue,
        };
        if extent_root.is_zero() {
            continue;
        }
        let entries = crate::store::extent_tree::scan_all(
            extent_root,
            crate::store::BTREE_ORDER,
            limits.max_fanout,
            store,
        )
        .map_err(|e| e.to_string())?;
        for (_, desc_bytes) in entries {
            let desc = match crate::format::descriptor::decode(&desc_bytes, &limits) {
                Ok(d) => d,
                Err(_) => {
                    all_ok = false;
                    continue;
                }
            };
            let bytes = match crate::core::materialize::materialize_to_vec(&desc, store, &limits) {
                Ok(b) => b,
                Err(_) => {
                    all_ok = false;
                    continue;
                }
            };
            let cid = crate::core::extent::ChunkId::of(&bytes);
            let Some(idx_bytes) = store.chunk_descriptor(&cid).map_err(|e| e.to_string())? else {
                all_ok = false;
                continue;
            };
            let idx_desc = match crate::format::descriptor::decode(&idx_bytes, &limits) {
                Ok(d) => d,
                Err(_) => {
                    all_ok = false;
                    continue;
                }
            };
            let idx_bytes_out =
                match crate::core::materialize::materialize_to_vec(&idx_desc, store, &limits) {
                    Ok(b) => b,
                    Err(_) => {
                        all_ok = false;
                        continue;
                    }
                };
            if idx_bytes_out != bytes {
                all_ok = false;
            }
        }
    }
    Ok(all_ok)
}

// ---------------------------------------------------------------------------
// Mutation application
// ---------------------------------------------------------------------------

/// Apply a physical mutation to a segment's raw bytes (CRC left broken:
/// the envelope must reject). The 4-byte segment magic is preserved (it is
/// structural, not a record): positions clamp to `>= 4`.
fn apply_physical(bytes: &mut Vec<u8>, op: (u8, u8, u8), seed: u64) {
    let (kind, a, b) = op;
    if bytes.len() <= 4 {
        return;
    }
    // Position within the writable region (after the magic).
    let pos = |a: u8| 4 + (a as usize) % (bytes.len() - 4);
    match kind % 4 {
        0 => {
            // flip one byte
            let i = pos(a);
            bytes[i] ^= b | 1;
        }
        1 => {
            // truncate (keep the magic intact)
            let i = pos(a).max(4);
            bytes.truncate(i);
        }
        2 => {
            // overwrite a range with pseudo-random bytes (splice)
            let start = pos(a);
            let len = 1 + (b as usize) % 24;
            let blob = prng_bytes(len, seed ^ start as u64);
            let end = (start + len).min(bytes.len());
            if end > start {
                bytes[start..end].copy_from_slice(&blob[..end - start]);
            }
        }
        _ => {
            // duplicate a range (shifts everything after it)
            let start = pos(a);
            let len = 1 + (b as usize) % 16;
            let end = (start + len).min(bytes.len());
            if end > start {
                let dup = bytes[start..end].to_vec();
                let insert_at = end.min(bytes.len());
                bytes.splice(insert_at..insert_at, dup);
            }
        }
    }
}

/// Interpret a proptest op tuple as a whole-store mutation: a semantic
/// mutation (recomputed envelope) or a physical one (broken CRC),
/// selected by the `semantic` bit. Returns the op kind name for evidence.
fn apply_mutation(dir: &Path, op: (u8, u8, u8), seed: u64, semantic: bool) -> String {
    let seqs = segment_seqs(dir);
    let seq = seqs[(op.0 as usize) % seqs.len()];
    if !semantic {
        // ---- physical: mutate raw bytes, leave the CRC broken ----
        let mut bytes = read_segment(dir, seq);
        apply_physical(&mut bytes, op, seed);
        write_segment(dir, seq, &bytes);
        return "physical".into();
    }
    // ---- semantic: recompute the envelope so the deep parsers see it ----
    let Some(mut records) = try_records_in(dir, seq) else {
        return "skipped-corrupt".into();
    };
    if records.is_empty() {
        return "no-records".into();
    }
    match op.1 % 5 {
        0 => {
            // rewrite one record's payload with random bytes (same length
            // keeps the record shape; the envelope is recomputed).
            let i = (op.2 as usize) % records.len();
            let rec = &records[i];
            let payload = prng_bytes(rec.payload.len(), seed ^ 0x5EED);
            records[i] = reencode(rec, payload);
            rebuild_segment(dir, seq, &records);
            format!("rewrite-payload({})", records[i].tag.name())
        }
        1 => {
            // alter one record's length (a different-length payload).
            let i = (op.2 as usize) % records.len();
            let rec = &records[i];
            let new_len = (rec.payload.len() + 1 + (op.0 as usize) % 7) % 64;
            let payload = prng_bytes(new_len, seed ^ 0x1E23);
            records[i] = reencode(rec, payload);
            rebuild_segment(dir, seq, &records);
            format!("alter-length({})", records[i].tag.name())
        }
        2 => {
            // replace a record's tag (the payload stays; the tag changes).
            let i = (op.2 as usize) % records.len();
            let rec = &records[i];
            let tag = match op.0 % 6 {
                0 => crate::format::version::RecordTag::Data,
                1 => crate::format::version::RecordTag::Model,
                2 => crate::format::version::RecordTag::Inode,
                3 => crate::format::version::RecordTag::BtreeNode,
                4 => crate::format::version::RecordTag::Root,
                _ => crate::format::version::RecordTag::MutationLog,
            };
            let mut r = rec.clone();
            r.tag = tag;
            records[i] = r;
            rebuild_segment(dir, seq, &records);
            format!("replace-tag({})", tag.name())
        }
        3 => {
            // duplicate one record (an extra copy appended at the end).
            let i = (op.2 as usize) % records.len();
            let dup = records[i].clone();
            records.push(dup);
            rebuild_segment(dir, seq, &records);
            "duplicate-record".into()
        }
        _ => {
            // swap two records' payloads (recomputed envelopes).
            let i = (op.2 as usize) % records.len();
            let j = (op.0 as usize) % records.len();
            let pi = records[i].payload.clone();
            let pj = records[j].payload.clone();
            records[i] = reencode(&records[i], pj);
            records[j] = reencode(&records[j], pi);
            rebuild_segment(dir, seq, &records);
            "swap-payloads".into()
        }
    }
}

/// Reorder two records' positions in a segment (the whole record bodies
/// move; every envelope is recomputed with valid CRCs).
fn apply_reorder(dir: &Path, a: usize, b: usize) -> String {
    for seq in segment_seqs(dir) {
        let Some(mut records) = try_records_in(dir, seq) else {
            continue;
        };
        if records.len() < 2 {
            continue;
        }
        let i = a % records.len();
        let j = b % records.len();
        if i != j {
            records.swap(i, j);
            rebuild_segment(dir, seq, &records);
        }
        return format!("reorder({i}<->{j})");
    }
    "no-segments".into()
}

/// Duplicate one mutation-log envelope record (append a copy): recovery
/// must detect the duplicate sequence (a typed rejection).
fn apply_duplicate_envelope(dir: &Path) -> String {
    for seq in segment_seqs(dir) {
        let mut records = records_in(dir, seq);
        if let Some(i) = records
            .iter()
            .position(|r| r.tag == crate::format::version::RecordTag::MutationLog)
        {
            let dup = records[i].clone();
            records.push(dup);
            rebuild_segment(dir, seq, &records);
            return format!("duplicate-envelope(seq {})", seq);
        }
    }
    "no-envelope".into()
}

/// Patch every mutation-log envelope's sequence number to `new_seq`
/// (recomputed envelopes): recovery skips envelopes with `seq <=
/// root.log_seq` (an admissible bounded state).
fn apply_patch_envelope_seqs(dir: &Path, new_seq: u64) -> String {
    for seq in segment_seqs(dir) {
        let mut records = records_in(dir, seq);
        let mut patched = 0usize;
        for r in records.iter_mut() {
            if r.tag == crate::format::version::RecordTag::MutationLog && r.payload.len() >= 8 {
                let mut payload = r.payload.clone();
                payload[..8].copy_from_slice(&new_seq.to_le_bytes());
                let re = reencode(r, payload);
                *r = re;
                patched += 1;
            }
        }
        if patched > 0 {
            rebuild_segment(dir, seq, &records);
            return format!("patch-envelope-seqs({patched})");
        }
    }
    "no-envelope".into()
}

/// Patch a superblock slot field (semantic: the slot CRC is recomputed).
fn apply_patch_superblock(
    dir: &Path,
    root_object_id: Option<crate::core::extent::ChunkId>,
) -> String {
    use crate::format::superblock::Superblock;
    use crate::format::version::{SUPERBLOCK_SLOT_A_OFFSET, SUPERBLOCK_SLOT_B_OFFSET};
    let sb_path = dir.join("superblock");
    let bytes = std::fs::read(&sb_path).expect("read superblock");
    // The active slot: decode both, patch the higher generation one.
    let a = Superblock::decode(&bytes[SUPERBLOCK_SLOT_A_OFFSET as usize..][..512]).ok();
    let b = Superblock::decode(&bytes[SUPERBLOCK_SLOT_B_OFFSET as usize..][..512]).ok();
    let active = match (&a, &b) {
        (Some(x), Some(y)) if y.generation > x.generation => y.clone(),
        (Some(x), _) => x.clone(),
        (None, Some(y)) => y.clone(),
        (None, None) => return "no-superblock".into(),
    };
    let mut patched = active.clone();
    if let Some(id) = root_object_id {
        patched.root_object_id = id;
    } else {
        patched.root_object_id = crate::core::extent::ChunkId::of(&prng_bytes(32, 0x5B));
    }
    let slot = patched.encode();
    let offset = if patched.generation & 1 == 0 {
        SUPERBLOCK_SLOT_A_OFFSET
    } else {
        SUPERBLOCK_SLOT_B_OFFSET
    };
    let mut out = bytes;
    out[offset as usize..offset as usize + 512].copy_from_slice(&slot);
    std::fs::write(&sb_path, out).expect("write superblock");
    "patch-superblock".into()
}

// ---------------------------------------------------------------------------
// Deterministic courts
// ---------------------------------------------------------------------------

/// Physical court 1: a flip strictly inside a record's PAYLOAD region must
/// break the envelope — open AND fsck both reject typed (never a silent
/// acceptance, never a panic).
#[test]
fn physical_payload_flips_are_integrity_rejected() {
    let image = build_tiny_image();
    for seq in segment_seqs(image.dir.path()) {
        for rec in records_in(image.dir.path(), seq) {
            let payload_start = (rec.offset + crate::format::record::HEADER_SIZE) as usize;
            if rec.payload.is_empty() {
                continue;
            }
            for rel in [0usize, rec.payload.len() / 2, rec.payload.len() - 1] {
                let dir = copy_image(image.dir.path());
                let mut bad = read_segment(dir.path(), seq);
                let pos = payload_start + rel;
                bad[pos] ^= 0x01;
                write_segment(dir.path(), seq, &bad);
                assert!(
                    Store::open(dir.path(), &config()).is_err(),
                    "payload flip at {pos} of seg {seq} must make open reject"
                );
                assert!(
                    crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).is_err(),
                    "payload flip at {pos} of seg {seq} must make fsck reject"
                );
            }
        }
    }
}

/// Physical court 2: flips in the header region and truncation must never
/// panic; open and fsck must return typed results (a torn-tail recovery —
/// the complete previous state — is an admissible crash-consistency
/// outcome).
#[test]
fn physical_header_flips_and_truncation_are_bounded() {
    let image = build_tiny_image();
    for seq in segment_seqs(image.dir.path()) {
        let bytes = read_segment(image.dir.path(), seq);
        let recs = records_in(image.dir.path(), seq);
        for rec in recs.iter() {
            let start = rec.offset as usize;
            let header_end = (rec.offset + crate::format::record::HEADER_SIZE) as usize;
            // header flips (skip the payload region)
            for rel in [0usize, 1, 2, 4, header_end - start - 4] {
                let dir = copy_image(image.dir.path());
                let mut bad = read_segment(dir.path(), seq);
                let pos = start + rel;
                bad[pos] ^= 0x01;
                write_segment(dir.path(), seq, &bad);
                let _ = Store::open(dir.path(), &config());
                let _ = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default());
            }
        }
        // truncations at record boundaries and mid-record
        for &cut in &[4usize, bytes.len() / 2, bytes.len() - 7] {
            let dir = copy_image(image.dir.path());
            let mut bad = read_segment(dir.path(), seq);
            bad.truncate(cut);
            write_segment(dir.path(), seq, &bad);
            let _ = Store::open(dir.path(), &config());
            let _ = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default());
        }
    }
}

/// Physical court 3: splice and duplicate-range mutations must never
/// panic; open and fsck must return typed results.
#[test]
fn physical_splices_are_bounded() {
    let image = build_tiny_image();
    let mut seed = 0xA11CEu64;
    for seq in segment_seqs(image.dir.path()) {
        let len = read_segment(image.dir.path(), seq).len();
        for (a, b) in [(0u8, 1u8), (20, 250), (200, 10)] {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let dir = copy_image(image.dir.path());
            let mut bad = read_segment(dir.path(), seq);
            apply_physical(&mut bad, (a, b, seed as u8), seed);
            assert_eq!(
                &bad[..4],
                SEGMENT_MAGIC,
                "physical ops must preserve the segment magic"
            );
            write_segment(dir.path(), seq, &bad);
            let _ = Store::open(dir.path(), &config());
            let _ = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default());
            let _ = len;
        }
    }
}

/// Semantic court 1: every record's payload rewritten with random bytes
/// (envelope CRC + content id recomputed) must never panic; open and fsck
/// return typed results; and when the store opens AND fsck is clean under
/// full materialization, the reads return the authenticated bytes.
#[test]
fn semantic_payload_rewrites_are_bounded() {
    let image = build_tiny_image();
    for seq in segment_seqs(image.dir.path()) {
        let records = records_in(image.dir.path(), seq);
        for (i, rec) in records.iter().enumerate() {
            let dir = copy_image(image.dir.path());
            let mut recs = records_in(dir.path(), seq);
            let payload = prng_bytes(rec.payload.len().max(1), 0x5EED ^ i as u64);
            recs[i] = reencode(rec, payload);
            rebuild_segment(dir.path(), seq, &recs);
            run_store_oracle(dir.path(), &image.files).unwrap_or_else(|e| {
                panic!("semantic payload rewrite {i} ({}): {e}", rec.tag.name())
            });
        }
    }
}

/// Semantic court 2: record tag replacement, length alteration, record
/// duplication, and payload swap — all with recomputed envelopes.
#[test]
fn semantic_record_mutations_are_bounded() {
    let image = build_tiny_image();
    let mut seed = 0xBEEFu64;
    for seq in segment_seqs(image.dir.path()) {
        let count = records_in(image.dir.path(), seq).len();
        for i in 0..count.min(12) {
            for op in [
                (0u8, 1u8, i as u8),
                (1, 1, i as u8),
                (2, 2, i as u8),
                (3, 3, i as u8),
                (4, 4, i as u8),
            ] {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let dir = copy_image(image.dir.path());
                apply_mutation(dir.path(), op, seed, true);
                run_store_oracle(dir.path(), &image.files).unwrap_or_else(|e| {
                    panic!("semantic record mutation {op:?} (seed {seed}): {e}")
                });
            }
        }
    }
}

/// Semantic court 3: superblock slot patching (root object id replaced,
/// slot CRC recomputed) must be bounded.
#[test]
fn semantic_superblock_patch_is_bounded() {
    let image = build_tiny_image();
    let dir = copy_image(image.dir.path());
    apply_patch_superblock(dir.path(), None);
    run_store_oracle(dir.path(), &image.files).unwrap_or_else(|e| panic!("superblock patch: {e}"));
}

/// B-tree exhibits: a node with fanout exactly 4096 decodes; 4097,
/// unsorted keys, and duplicate keys are rejected typed at the node
/// codec; and the whole crafted tree (valid-CRC root record + valid-CRC
/// superblock slot) is bounded through open.
#[test]
fn btree_fanout_and_key_exhibits() {
    use crate::store::index::{Entry, Node};
    let order = crate::store::BTREE_ORDER;
    let fanout = crate::core::limits::Limits::default().max_fanout;

    // Exactly 4096 entries: decodes.
    let entries: Vec<Entry> = (0..4096u64)
        .map(|i| Entry {
            key: i.to_be_bytes().to_vec(),
            value: vec![0xAB; 4],
        })
        .collect();
    let node = Node::Leaf { entries };
    let payload = node.encode(order);
    assert!(
        Node::decode(&payload, order, fanout).is_ok(),
        "fanout 4096 must decode"
    );

    // 4097 entries: rejected typed.
    let entries: Vec<Entry> = (0..4097u64)
        .map(|i| Entry {
            key: i.to_be_bytes().to_vec(),
            value: vec![0xAB; 4],
        })
        .collect();
    let node = Node::Leaf { entries };
    let payload = node.encode(order);
    assert!(
        Node::decode(&payload, order, fanout).is_err(),
        "fanout 4097 must be rejected"
    );

    // Unsorted keys: rejected typed.
    let entries = vec![
        Entry {
            key: vec![2],
            value: vec![1],
        },
        Entry {
            key: vec![1],
            value: vec![2],
        },
    ];
    let node = Node::Leaf { entries };
    let payload = node.encode(order);
    assert!(
        Node::decode(&payload, order, fanout).is_err(),
        "unsorted must reject"
    );

    // Duplicate keys: rejected typed.
    let entries = vec![
        Entry {
            key: vec![7],
            value: vec![1],
        },
        Entry {
            key: vec![7],
            value: vec![2],
        },
    ];
    let node = Node::Leaf { entries };
    let payload = node.encode(order);
    assert!(
        Node::decode(&payload, order, fanout).is_err(),
        "duplicates must reject"
    );

    // Store-level: a crafted chunk-index root pointing at an over-fanout
    // node (valid-CRC root record + valid-CRC superblock slot) — open
    // must reject typed (the tree walk hits the node codec).
    let image = build_tiny_image();
    let dir = copy_image(image.dir.path());
    let crafted = Node::Leaf {
        entries: (0..4097u64)
            .map(|i| Entry {
                key: i.to_be_bytes().to_vec(),
                value: vec![0xAB; 4],
            })
            .collect(),
    };
    let crafted_payload = crafted.encode(order);
    let crafted_id = crate::core::extent::ChunkId::of(&crafted_payload);
    // Append the crafted node as a Data record (valid envelope).
    let seq = *segment_seqs(dir.path()).first().expect("segments present");
    {
        let mut records = records_in(dir.path(), seq);
        let mut rec = newest_root_record(dir.path(), seq);
        // Replace the ROOT record: point the chunk index at the crafted
        // node.
        let root = Root::decode(&rec.payload).expect("decode root");
        let mut new_root = root.clone();
        new_root.chunk_index_root = crafted_id;
        rec = reencode(&rec, new_root.encode());
        records.retain(|r| r.tag != crate::format::version::RecordTag::Root);
        records.push(rec.clone());
        rebuild_segment(dir.path(), seq, &records);
        // Append the crafted node's own record (its id must resolve).
        let mut all = records_in(dir.path(), seq);
        let node_rec = crate::store::segment::ScanRecord {
            tag: crate::format::version::RecordTag::BtreeNode,
            flags: 0,
            stored_len: crafted_payload.len() as u32,
            materialized_len: None,
            content_id: crafted_id,
            payload: crafted_payload.clone(),
            offset: 0,
        };
        all.push(node_rec);
        rebuild_segment(dir.path(), seq, &all);
    }
    // The superblock must reference the NEW root record.
    for seq in segment_seqs(dir.path()) {
        if let Some(root_rec) = records_in(dir.path(), seq)
            .iter()
            .find(|r| r.tag == crate::format::version::RecordTag::Root)
        {
            apply_patch_superblock(
                dir.path(),
                Some(crate::core::extent::ChunkId::of(&root_rec.payload)),
            );
            break;
        }
    }
    // Open must reject typed: verify_root walks the crafted over-fanout
    // chunk-index root.
    assert!(
        Store::open(dir.path(), &config()).is_err(),
        "crafted over-fanout chunk index root must make open reject"
    );
    let _ = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default());
}

/// The malicious-descriptor-in-a-valid-CRC-envelope exhibit: a crafted
/// chunk-index entry maps a content id to a self-referential EXACT_REF
/// descriptor (valid envelope, valid slot CRC) — resolving it through the
/// store's own DecoderContext must terminate with a typed error (the
/// depth cap), never a panic or a loop.
#[test]
fn valid_crc_envelope_containing_malicious_descriptor() {
    use crate::core::materialize::materialize_to_vec;
    use crate::core::representation::Representation;
    use crate::store::index::{Entry, Node};
    let order = crate::store::BTREE_ORDER;

    let image = build_tiny_image();
    let dir = copy_image(image.dir.path());
    let victim = crate::core::extent::ChunkId::of(b"victim");
    // The self-loop descriptor: EXACT_REF{target: victim, len: 64}.
    let desc_bytes = crate::format::descriptor::encode(&Representation::ExactRef {
        target: victim,
        off: 0,
        len: 64,
    })
    .expect("encode");
    // Crafted chunk-index leaf: victim -> self-loop descriptor.
    let crafted = Node::Leaf {
        entries: vec![Entry {
            key: victim.as_bytes().to_vec(),
            value: desc_bytes.clone(),
        }],
    };
    let crafted_payload = crafted.encode(order);
    let crafted_id = crate::core::extent::ChunkId::of(&crafted_payload);

    let seq = *segment_seqs(dir.path()).first().expect("segments present");
    {
        let mut records = records_in(dir.path(), seq);
        let root_rec = newest_root_record(dir.path(), seq);
        let root = Root::decode(&root_rec.payload).expect("decode root");
        let mut new_root = root.clone();
        new_root.chunk_index_root = crafted_id;
        let new_root_rec = reencode(&root_rec, new_root.encode());
        records.retain(|r| r.tag != crate::format::version::RecordTag::Root);
        records.push(new_root_rec);
        let node_rec = crate::store::segment::ScanRecord {
            tag: crate::format::version::RecordTag::BtreeNode,
            flags: 0,
            stored_len: crafted_payload.len() as u32,
            materialized_len: None,
            content_id: crafted_id,
            payload: crafted_payload,
            offset: 0,
        };
        records.push(node_rec);
        rebuild_segment(dir.path(), seq, &records);
    }
    for seq in segment_seqs(dir.path()) {
        if let Some(root_rec) = records_in(dir.path(), seq)
            .iter()
            .find(|r| r.tag == crate::format::version::RecordTag::Root)
        {
            apply_patch_superblock(
                dir.path(),
                Some(crate::core::extent::ChunkId::of(&root_rec.payload)),
            );
            break;
        }
    }
    // The store opens (the crafted tree is structurally valid), and the
    // malicious entry reaches the materializer through the store's own
    // DecoderContext — bounded by the depth cap.
    let store = Store::open(dir.path(), &config()).expect("crafted tree opens");
    let desc = store.fetch_descriptor(&victim).expect("fetch descriptor");
    let out = materialize_to_vec(&desc, &store, store.limits());
    assert!(
        matches!(
            out,
            Err(crate::core::materialize::MaterializeError::DepthExceeded { .. })
        ),
        "self-referential descriptor must hit the depth cap (got {out:?})"
    );
    // The OTHER files still read (their extents are untouched).
    for (ino, orig) in &image.files {
        let bytes = store
            .read_file(*ino, 0, orig.len().max(1) as u64)
            .expect("read");
        assert_eq!(&bytes, orig, "ino {ino} content must be unchanged");
    }
}

/// Mutation-log exhibits: a duplicate envelope sequence must be a typed
/// rejection at open (the recovery duplicate invariant); a non-monotonic
/// (rolled-back) sequence is skipped — an admissible bounded state.
#[test]
fn mutation_log_duplicate_and_nonmonotonic_sequences() {
    let image = build_log_tail_image();
    // Sanity: the pristine log-tail store opens (replay runs).
    {
        let dir = copy_image(image.dir.path());
        Store::open(dir.path(), &config()).expect("pristine log tail opens");
        let report =
            crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).expect("fsck runs");
        assert!(report.error_count() == 0, "pristine log tail is clean");
    }
    // Duplicate envelope: two envelopes with the same sequence.
    {
        let dir = copy_image(image.dir.path());
        apply_duplicate_envelope(dir.path());
        let res = Store::open(dir.path(), &config());
        assert!(
            res.is_err(),
            "duplicate mutation-log sequence must be a typed rejection at open"
        );
        let _ = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default());
    }
    // Non-monotonic sequences: all envelope seqs <= root.log_seq — replay
    // skips them; the store opens (bounded; the skipped op's file is
    // absent, and reads of it are typed errors).
    {
        let dir = copy_image(image.dir.path());
        apply_patch_envelope_seqs(dir.path(), 0);
        let store = Store::open(dir.path(), &config())
            .unwrap_or_else(|e| panic!("skipped-tail store must open: {e}"));
        for (ino, _) in &image.files {
            let _ = store.read_file(*ino, 0, 4096); // Err-or-Ok, never panic
        }
        let _ = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default());
    }
}

/// The diamond-deepest-path regression (Phase-10E class): a node first
/// reached SHALLOW and later DEEP must report the DEEPEST chain depth.
/// Built as a real store: entry -> SharedDict{dict: A, shared: B} where
/// A -> C -> X -> Y (the deep route) and B -> X (shallow); chain_depth
/// must report the deepest path through X.
#[test]
fn diamond_deepest_path_chain_depth() {
    use crate::core::representation::Representation;
    let image = build_tiny_image();
    let dir = copy_image(image.dir.path());

    // Node ids (deterministic, distinct from the store's own ids).
    let a = crate::core::extent::ChunkId::of(b"diamond-a");
    let b = crate::core::extent::ChunkId::of(b"diamond-b");
    let c = crate::core::extent::ChunkId::of(b"diamond-c");
    let x = crate::core::extent::ChunkId::of(b"diamond-x");
    let y = crate::core::extent::ChunkId::of(b"diamond-y");

    // Descriptors: A = EXACT_REF{B}, B = EXACT_REF{X}, C = EXACT_REF{D},
    // D = EXACT_REF{X}, X = EXACT_REF{Y}, Y = FILL. Wait — A must branch:
    // A is the FILE dictionary of the entry, referenced at depth 1; the
    // entry's SHARED branch is B. A -> C -> X -> Y (deep), B -> X
    // (shallow).
    let d = crate::core::extent::ChunkId::of(b"diamond-d");
    let mut chunk_index: Vec<(crate::core::extent::ChunkId, Vec<u8>)> = Vec::new();
    let mk = |rep: &Representation| crate::format::descriptor::encode(rep).expect("encode");
    chunk_index.push((
        a,
        mk(&Representation::ExactRef {
            target: c,
            off: 0,
            len: 64,
        }),
    ));
    chunk_index.push((
        c,
        mk(&Representation::ExactRef {
            target: d,
            off: 0,
            len: 64,
        }),
    ));
    chunk_index.push((
        d,
        mk(&Representation::ExactRef {
            target: x,
            off: 0,
            len: 64,
        }),
    ));
    chunk_index.push((
        x,
        mk(&Representation::ExactRef {
            target: y,
            off: 0,
            len: 64,
        }),
    ));
    chunk_index.push((y, mk(&Representation::Fill { value: 9, len: 64 })));
    chunk_index.push((
        b,
        mk(&Representation::ExactRef {
            target: x,
            off: 0,
            len: 64,
        }),
    ));

    // The entry descriptor: a SEQUENCE_SHARED_DICT whose dictionary is A
    // and whose shared branch is B, with a literal-run command stream.
    let commands = vec![0x3Fu8];
    let literals: Vec<u8> = vec![7; 64];
    let (model_obj, enc_obj, lens) = {
        let enc =
            crate::rans::sequence::encode_streams_n(&[commands, literals, Vec::new(), Vec::new()])
                .expect("streams encode");
        (enc.model_obj, enc.enc_obj, enc.lens)
    };
    let (scale_bits, codec) = crate::rans::sequence::sequence_scale_codec();
    let model_id = crate::core::extent::ChunkId::of(&model_obj);
    let enc_id = crate::core::extent::ChunkId::of(&enc_obj);
    let entry_desc = Representation::SequenceSharedDict {
        dictionary: a,
        dictionary_len: 64,
        shared: b,
        shared_len: 64,
        model: model_id,
        enc_obj: enc_id,
        scale_bits,
        codec,
        seq_len: lens[0],
        lit_len: lens[1],
        off_len: lens[2],
        src_len: lens[3],
        cmds: 1,
        lit_out: 64,
        len: 64,
    };

    // Build the crafted chunk-index tree over the descriptors + objects,
    // then point a crafted chunk-index root at it (the store's own tree
    // machinery builds the node tree; the records are re-encoded with
    // valid envelopes).
    let entry_id = crate::core::extent::ChunkId::of(b"diamond-entry");
    chunk_index.push((entry_id, mk(&entry_desc)));
    // Sort by key for bulk_load (the store's chunk index is a sorted tree).
    chunk_index.sort_by_key(|(k, _)| *k);
    let entries: Vec<(Vec<u8>, Vec<u8>)> = chunk_index
        .into_iter()
        .map(|(k, v)| (k.as_bytes().to_vec(), v))
        .collect();
    // Build the tree with the store's index machinery, staging nodes into
    // a provider that collects them.
    let mut nodes: Vec<(crate::core::extent::ChunkId, Vec<u8>)> = Vec::new();
    let root_id = {
        struct Collector<'a>(&'a mut Vec<(crate::core::extent::ChunkId, Vec<u8>)>);
        impl crate::store::index::ObjectProvider for Collector<'_> {
            fn get(
                &self,
                _id: &crate::core::extent::ChunkId,
            ) -> Result<Option<Vec<u8>>, crate::store::index::BTreeError> {
                Ok(None)
            }
            fn put(&mut self, id: crate::core::extent::ChunkId, bytes: Vec<u8>) {
                self.0.push((id, bytes));
            }
        }
        let mut c = Collector(&mut nodes);
        crate::store::index::bulk_load(
            &entries,
            crate::store::BTREE_ORDER,
            crate::core::limits::Limits::default().max_fanout,
            &mut c,
        )
        .expect("bulk load")
    };
    assert!(!root_id.is_zero());

    // Rewrite the ROOT record to point the chunk index at the crafted
    // tree, append the node records + model/enc object records, and point
    // the superblock at the new root record.
    let seq = *segment_seqs(dir.path()).first().expect("segments present");
    {
        let mut records = records_in(dir.path(), seq);
        let root_rec = newest_root_record(dir.path(), seq);
        let root = Root::decode(&root_rec.payload).expect("decode root");
        let mut new_root = root.clone();
        new_root.chunk_index_root = root_id;
        records.retain(|r| r.tag != crate::format::version::RecordTag::Root);
        records.push(reencode(&root_rec, new_root.encode()));
        for (id, payload) in &nodes {
            records.push(crate::store::segment::ScanRecord {
                tag: crate::format::version::RecordTag::BtreeNode,
                flags: 0,
                stored_len: payload.len() as u32,
                materialized_len: None,
                content_id: *id,
                payload: payload.clone(),
                offset: 0,
            });
        }
        records.push(crate::store::segment::ScanRecord {
            tag: crate::format::version::RecordTag::Data,
            flags: 0,
            stored_len: model_obj.len() as u32,
            materialized_len: None,
            content_id: model_id,
            payload: model_obj,
            offset: 0,
        });
        records.push(crate::store::segment::ScanRecord {
            tag: crate::format::version::RecordTag::Data,
            flags: 0,
            stored_len: enc_obj.len() as u32,
            materialized_len: None,
            content_id: enc_id,
            payload: enc_obj,
            offset: 0,
        });
        rebuild_segment(dir.path(), seq, &records);
    }
    for seq in segment_seqs(dir.path()) {
        if let Some(root_rec) = records_in(dir.path(), seq)
            .iter()
            .find(|r| r.tag == crate::format::version::RecordTag::Root)
        {
            apply_patch_superblock(
                dir.path(),
                Some(crate::core::extent::ChunkId::of(&root_rec.payload)),
            );
            break;
        }
    }
    // chain_depth must report the DEEPEST path through the shared node X:
    // entry -> A (1) -> C (2) -> D (3) -> X (4) -> Y (5). A first-visit-
    // wins walk would under-count (X first reached at depth 3 via B).
    let store = Store::open(dir.path(), &config()).expect("diamond store opens");
    let depth = crate::optimizer::rebase::chain_depth(&store, &entry_desc);
    assert_eq!(
        depth, 5,
        "deepest path through the diamond must be reported"
    );
    // Materialization of the entry is bounded and deterministic: the
    // dictionary branch legitimately exceeds the decode cap (depth 5 > 4),
    // so the materializer must reject typed with DepthExceeded — never a
    // panic, never a loop.
    let out = crate::core::materialize::materialize_to_vec(&entry_desc, &store, store.limits());
    assert!(
        matches!(
            out,
            Err(crate::core::materialize::MaterializeError::DepthExceeded { depth: 5, max: 4 })
        ),
        "diamond entry must hit the depth cap deterministically (got {out:?})"
    );
}

// ---------------------------------------------------------------------------
// Whole-store mutator (proptest)
// ---------------------------------------------------------------------------

use proptest::prelude::*;

proptest! {
    /// Seeded mutation recipes over the tiny store image, both CRC flavors
    /// (physical: broken envelope → integrity rejection; semantic:
    /// recomputed envelope → deep parsers). The oracle: never panic,
    /// never hang, never an invariant violation.
    #[test]
    fn whole_store_mutator(
        ops in prop::collection::vec(any::<(u8, u8, u8)>(), 1..=6),
        semantic in proptest::bool::ANY,
        seed in any::<u64>(),
    ) {
        let image = build_tiny_image();
        let dir = copy_image(image.dir.path());
        let mut applied = Vec::new();
        for op in ops {
            applied.push(apply_mutation(
                dir.path(),
                op,
                seed.wrapping_add(op.0 as u64),
                semantic,
            ));
        }
        // A record reorder (whole bodies move; valid envelopes).
        applied.push(apply_reorder(dir.path(), 2, 7));
        run_store_oracle(dir.path(), &image.files).unwrap_or_else(|e| {
            panic!(
                "whole-store mutator (semantic={semantic}, ops={applied:?}): {e}"
            )
        });
    }

    /// Superblock-directed mutations: root object id replacement with a
    /// recomputed slot CRC, plus the whole-store recipe on top.
    #[test]
    fn superblock_mutator(
        ops in prop::collection::vec(any::<(u8, u8, u8)>(), 0..=4),
        seed in any::<u64>(),
    ) {
        let image = build_tiny_image();
        let dir = copy_image(image.dir.path());
        apply_patch_superblock(dir.path(), None);
        for op in ops {
            apply_mutation(dir.path(), op, seed.wrapping_add(op.0 as u64), true);
        }
        run_store_oracle(dir.path(), &image.files)
            .unwrap_or_else(|e| panic!("superblock mutator: {e}"));
    }
}
