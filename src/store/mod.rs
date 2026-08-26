//! The store: crash-consistent persistent immutable object store
//! (ADR-0007/0008). Mounts, recovers, reads, writes, and accounts.

#![forbid(unsafe_code)]

pub mod directory;
pub mod epoch;
pub mod extent_tree;
pub mod gc;
pub mod index;
pub mod inode;
pub mod io;
pub mod object;
pub mod physical;
pub mod recovery;
pub mod root;
pub mod segment;
pub mod snapshot;
pub mod transaction;
pub mod workers;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::core::candidate::ObjectRecord;
use crate::core::cost::Policy;
use crate::core::extent::ChunkId;
use crate::core::limits::Limits;
use crate::core::materialize::{DecoderContext, MaterializeError, materialize};
use crate::core::representation::{RansCodec, Representation, UniverseId};
use crate::format::record::{FLAG_HAS_MATERIALIZED_LEN, encode as encode_record};
use crate::format::superblock::Superblock;
use crate::format::version::{RecordTag, SUPERBLOCK_SLOT_A_OFFSET, SUPERBLOCK_SLOT_B_OFFSET};
use directory::DirEntry;
use index::{BTreeError, ObjectProvider};
use inode::{Inode, InodeData};
use object::{Location, ObjectIndex, StoreStats};
use root::{Root, SuperblockPair};
use segment::SegmentWriter;

/// Store-level errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// Segment-layer failure.
    Segment(segment::SegmentError),
    /// Superblock/root failure.
    Superblock(String),
    /// Index (B-tree) failure.
    Index(String),
    /// Object missing.
    MissingObject(crate::core::extent::ChunkId),
    /// Chunk descriptor missing.
    MissingChunk(crate::core::extent::ChunkId),
    /// Descriptor decode failure.
    Descriptor(String),
    /// Store not open / not created.
    NotOpen,
    /// Store already mounted (lock held).
    Locked,
    /// Invalid configuration.
    Config(String),
    /// Resource limit exceeded.
    Limit(String),
    /// Store is full (ENOSPC equivalent), with context.
    Full(String),
    /// I/O failure.
    Io(String),
    /// Commit protocol violation (crash-court simulation).
    CrashSimulated(String),
    /// Invariant violation (bug).
    Invariant(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for StoreError {}

impl From<segment::SegmentError> for StoreError {
    fn from(e: segment::SegmentError) -> Self {
        StoreError::Segment(e)
    }
}

impl From<crate::format::codec::CodecError> for StoreError {
    fn from(e: crate::format::codec::CodecError) -> Self {
        StoreError::Descriptor(e.to_string())
    }
}

impl From<BTreeError> for StoreError {
    fn from(e: BTreeError) -> Self {
        StoreError::Index(e.to_string())
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}

/// Store configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StoreConfig {
    /// Segment size cap (bytes); segments roll over when full.
    pub segment_size: u64,
    /// GC emergency reserve ratio of physical capacity.
    pub gc_reserve_ratio: f64,
    /// High watermark ratio triggering accelerated GC.
    pub gc_high_watermark: f64,
    /// GC target: compact segments with live ratio below this.
    pub gc_target_ratio: f64,
    /// Maximum records scanned per segment (defense).
    pub max_records_per_segment: u64,
    /// Resource limits.
    pub limits: Limits,
    /// Cost policy.
    pub policy: Policy,
    /// Capacity override (tests/embedded): caps the reported physical
    /// capacity below the real device so watermark/ENOSPC logic can be
    /// exercised deterministically. Never exceeds the physical device
    /// (honesty rule, §22).
    pub capacity_override: Option<u64>,
    /// Owner uid of the filesystem root directory (the mounting user).
    pub root_uid: u32,
    /// Owner gid of the filesystem root directory.
    pub root_gid: u32,
    /// Phase-10B foreground representation policy: how much search CPU
    /// the write path spends per chunk. Ablations construct their own
    /// `OptimizeOptions` and run with the full policy; the mounted
    /// filesystem carries this one.
    pub foreground: crate::optimizer::foreground::ForegroundPolicy,
    /// Phase-10F storage transport (ADR-0021): `Sync` (reference engine,
    /// default) or `Uring` (io_uring performance path). A transport choice,
    /// not an on-disk format: a store is equally mountable with either.
    pub io_backend: crate::store::io::IoBackendKind,
    /// Phase-10F io_uring submission queue capacity (ops per submit
    /// batch; `UringIo` only).
    pub io_uring_entries: u32,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            segment_size: 128 * 1024 * 1024,
            gc_reserve_ratio: 0.04,
            gc_high_watermark: 0.92,
            gc_target_ratio: 0.6,
            max_records_per_segment: 10_000_000,
            limits: Limits::default(),
            policy: Policy::balanced(),
            capacity_override: None,
            root_uid: current_uid(),
            root_gid: current_gid(),
            foreground: crate::optimizer::foreground::ForegroundPolicy::default(),
            io_backend: crate::store::io::IoBackendKind::Sync,
            io_uring_entries: 256,
        }
    }
}

/// B-tree fanout (order).
pub const BTREE_ORDER: u16 = 64;

/// Maximum tracked DSFB chunks before eviction (performance-only state;
/// dropping it never affects bytes).
pub const DSFB_MAX_CHUNKS: usize = 100_000;

/// One extent update within a file-region commit.
#[derive(Debug, Clone)]
pub struct ExtentUpdate {
    /// Logical offset of the extent.
    pub offset: u64,
    /// Chosen representation.
    pub descriptor: Representation,
    /// Logical content id of the chunk bytes.
    pub content_id: ChunkId,
    /// New objects to persist.
    pub objects: Vec<ObjectRecord>,
}

/// The write/commit coordinator state: root, superblock, generation and
/// feature bits. Readers snapshot it under a read lock; the commit path
/// publishes under a write lock while holding `Store::commit_lock`.
struct CommitState {
    root: Root,
    superblock: Superblock,
    generation: u64,
    features_in_use: u64,
}

/// Per-inode mutation locks (sharded mutexes keyed by inode number).
///
/// Lock order: `inode_lock → commit_lock`. Readers never take either.
/// File-data writes and truncates hold the inode lock for their whole
/// prepare+commit sequence so two writers to the same file cannot
/// interleave their read-modify-write; writers to different files
/// serialize only on the short commit lock.
pub struct InodeLockTable {
    shards: Box<[std::sync::Mutex<()>]>,
}

/// Number of inode-lock shards (a power of two).
const INODE_LOCK_SHARDS: usize = 256;

impl Default for InodeLockTable {
    fn default() -> Self {
        let mut shards = Vec::with_capacity(INODE_LOCK_SHARDS);
        for _ in 0..INODE_LOCK_SHARDS {
            shards.push(std::sync::Mutex::new(()));
        }
        Self {
            shards: shards.into_boxed_slice(),
        }
    }
}

impl InodeLockTable {
    /// Lock the shard for `ino` (serializes mutations of one inode).
    pub fn lock(&self, ino: u64) -> std::sync::MutexGuard<'_, ()> {
        self.shards[(ino as usize) & (INODE_LOCK_SHARDS - 1)]
            .lock()
            .expect("inode lock poisoned")
    }
}

/// The filesystem store.
///
/// Concurrency model (ADR-0013, Phase 8): reads traverse immutable
/// content-addressed state (a root snapshot + the append-only object
/// index) with no global lock; writes prepare candidates concurrently and
/// serialize only the short transaction application + root publication on
/// `commit_lock`. GC is offline (unmounted), so the mounted object index
/// never shrinks under a reader.
pub struct Store {
    dir: PathBuf,
    config: StoreConfig,
    /// In-memory derived object index (sharded `RwLock`; append-only while
    /// mounted).
    object_index: std::sync::Arc<ObjectIndex>,
    /// Commit state: root, superblock, generation, feature bits.
    commit: std::sync::RwLock<CommitState>,
    /// The commit coordinator: serializes transaction application + root
    /// publication. Held from `begin_tx` through commit; also taken by the
    /// durability barrier so an fsync observes every commit that started
    /// before it.
    commit_lock: std::sync::Mutex<()>,
    /// Current segment writer (serialized append; kept open from mount).
    segment: std::sync::Mutex<Option<SegmentWriter>>,
    /// Statistics.
    stats: std::sync::Mutex<StoreStats>,
    /// Advisory lock file.
    _lock: File,
    /// Bounded decoded-model cache (performance only).
    model_cache: std::sync::Mutex<crate::cache::model::ModelCache>,
    /// DSFB storage observer (performance-only; zero decoding authority).
    /// Bounded by `DSFB_MAX_CHUNKS`; dropping it affects only search
    /// ordering, never bytes (ADR-0004).
    dsfb: std::sync::Mutex<crate::dsfb::observer::StorageObserver>,
    /// Per-inode mutation locks (file-data writes and truncates).
    inode_locks: std::sync::Arc<InodeLockTable>,
    /// Phase-10A write-path phase timings (diagnostic only).
    perf: std::sync::Arc<crate::perf::Timings>,
    /// Phase-10B foreground representation policy.
    foreground: crate::optimizer::foreground::ForegroundPolicy,
    /// Phase-11E probe gate: when set, this store's search/decode sites
    /// submit their work to the process-wide [`workers::POOL`] instead of
    /// the 11C semaphore. PROBE-ONLY: set by the probe test AFTER binding
    /// the pool to this store's Arc; every other store (and the FUSE
    /// daemon) keeps the flag false and the semaphore path untouched. The
    /// per-store flag — not a global gate — is what lets the probe run
    /// inside the shared test binary without hijacking other tests' writes.
    worker_pool: std::sync::atomic::AtomicBool,
    /// Phase-10D active metadata writeback epoch (pending namespace/write-
    /// back mutations between checkpoints; see `store/epoch.rs`).
    epoch: std::sync::Mutex<crate::store::epoch::Epoch>,
    /// Phase-11C lock-free mirror of the epoch's pending-op count
    /// (`epoch.seq − root.log_seq`). Written under the epoch guard (every
    /// envelope-staging op and the checkpoint update it), read WITHOUT the
    /// guard by the checkpoint-threshold check — removing the per-write
    /// epoch-mutex acquisition 11B measured as `epoch_wait` (20% of
    /// 16-thread request time).
    epoch_pending: std::sync::atomic::AtomicU64,
    /// Phase-10F storage transport (ADR-0021): the reference synchronous
    /// engine (`SyncIo`) or the io_uring performance path (`UringIo`).
    /// Every file mutation and payload read goes through this backend; the
    /// crash courts run against both and must produce identical
    /// store-directory bytes at every injection point.
    io: std::sync::Arc<dyn crate::store::io::IoBackend>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cs = self.commit.read().expect("commit state poisoned");
        f.debug_struct("Store")
            .field("dir", &self.dir)
            .field("generation", &cs.generation)
            .field("root", &cs.root)
            .finish_non_exhaustive()
    }
}

/// The guard-independent input of a batched materialization's decode
/// half (Phase-11C): the extent offsets, the decoded descriptors, the
/// prefetched objects, and the reference closure's nested descriptors.
/// Everything is OWNED — produced under the epoch guard (when the read
/// is overlay-aware) and decoded WITHOUT it.
#[derive(Debug)]
pub(crate) struct PreparedRead {
    starts: Vec<u64>,
    descs: Vec<Representation>,
    objects: std::collections::HashMap<ChunkId, Vec<u8>>,
    descriptors: std::collections::HashMap<ChunkId, Vec<u8>>,
    offset: u64,
    end: u64,
    avail: usize,
}

impl PreparedRead {
    /// The merged extents' start offsets, in order (tests: how many
    /// extents a read window collected).
    #[cfg(test)]
    pub(crate) fn starts(&self) -> &[u64] {
        &self.starts
    }
}

impl Store {
    /// Create a new filesystem (mkfs).
    pub fn create(dir: &Path, config: &StoreConfig, uuid: [u8; 16]) -> Result<Self, StoreError> {
        if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
            return Err(StoreError::Config(format!(
                "store directory {} is not empty",
                dir.display()
            )));
        }
        std::fs::create_dir_all(dir)?;
        std::fs::create_dir_all(dir.join("segments"))?;
        let lock = open_lock(dir)?;
        // Phase-10F: the transport is fixed at mkfs/mount time (a runtime
        // choice, not an on-disk format).
        let io = crate::store::io::build_backend(config.io_backend, dir, config.io_uring_entries)?;
        // Initial root.  Ino 1 is the filesystem root so FUSE's mount
        // root (always ino 1) maps 1:1 to the store (ADR-0002).
        let root = Root {
            uuid,
            root_dir_ino: 1,
            generation: 0,
            ..Default::default()
        };
        // Initial superblock in slot A (generation 0 is even); written
        // through the backend and made durable (the pre-10F `write_slot`
        // with sync).
        let sb = Superblock {
            uuid,
            generation: 0,
            segment_seq: 0,
            ..Default::default()
        };
        io.write_superblock_slot(SUPERBLOCK_SLOT_A_OFFSET, &sb.encode())?;
        io.fsync_superblock()?;
        // Root object record lives in segment 0.
        let store = Self {
            dir: dir.to_path_buf(),
            config: *config,
            object_index: std::sync::Arc::new(ObjectIndex::new()),
            commit: std::sync::RwLock::new(CommitState {
                root,
                superblock: sb,
                generation: 0,
                features_in_use: 0,
            }),
            commit_lock: std::sync::Mutex::new(()),
            segment: std::sync::Mutex::new(None),
            stats: std::sync::Mutex::new(StoreStats::default()),
            _lock: lock,
            model_cache: std::sync::Mutex::new(crate::cache::model::ModelCache::new(64)),
            dsfb: std::sync::Mutex::new(crate::dsfb::observer::StorageObserver::default()),
            inode_locks: std::sync::Arc::new(InodeLockTable::default()),
            perf: std::sync::Arc::new(crate::perf::Timings::default()),
            foreground: config.foreground,
            worker_pool: std::sync::atomic::AtomicBool::new(false),
            epoch: std::sync::Mutex::new(crate::store::epoch::Epoch::default()),
            epoch_pending: std::sync::atomic::AtomicU64::new(0),
            io,
        };
        store.open_segment(0)?;
        // Create the root directory inode (ino 1) and commit the initial
        // root through the normal transaction protocol, so the store is
        // mountable (verify_root requires the root dir inode to exist).
        // The root is owned by the mounting user (config).
        {
            let mut tx = store.begin_tx()?;
            let root_inode = Inode::new_dir(config.root_uid, config.root_gid, 0o755);
            Store::put_inode_in_tx(&mut tx, 1, &root_inode)?;
            tx.commit(&crate::store::transaction::CrashHooks::none())?;
        }
        store
            .stats
            .lock()
            .expect("stats poisoned")
            .physical_capacity = store.physical_capacity();
        Ok(store)
    }

    /// Open (mount) an existing store: recovery + derived index rebuild.
    pub fn open(dir: &Path, config: &StoreConfig) -> Result<Self, StoreError> {
        let lock = open_lock(dir)?;
        // Phase-10F: the transport is a per-invocation runtime choice; the
        // on-disk format is identical for both backends.
        let io = crate::store::io::build_backend(config.io_backend, dir, config.io_uring_entries)?;
        let sb_path = dir.join("superblock");
        let pair = SuperblockPair::read(&sb_path)?;
        // Rebuild the object index from segments (needed to resolve the
        // root object; also the source for the recovery fallback).
        let object_index = ObjectIndex::new();
        let segments = segment::list_segments(dir)?;
        for seq in &segments {
            let path = segment::segment_path(dir, *seq);
            let (records, _) = segment::scan_segment(&path, config.max_records_per_segment)?;
            for rec in records {
                object_index.insert(
                    rec.content_id,
                    Location {
                        segment_seq: *seq,
                        offset: rec.offset,
                        stored_len: rec.stored_len as u64,
                        materialized_len: rec.materialized_len,
                        tag: rec.tag,
                    },
                );
            }
        }
        // Choose the committed superblock. With deferred durability the
        // inactive slot is written before its segment data is fsync'd; a
        // power loss can therefore leave the newest slot referencing a
        // lost root record. Recovery validates the chosen slot's root and
        // falls back to the newest valid ROOT record found in the
        // segments — a complete earlier transaction (ADR-0008: recovery
        // may observe the complete previous or complete new transaction,
        // never an impossible hybrid).
        let (sb, root) = Self::choose_root(&pair, dir, &object_index, config)?;
        let store = Self {
            dir: dir.to_path_buf(),
            config: *config,
            object_index: std::sync::Arc::new(object_index),
            commit: std::sync::RwLock::new(CommitState {
                root: root.clone(),
                superblock: sb.clone(),
                generation: sb.generation,
                features_in_use: sb.incompat,
            }),
            commit_lock: std::sync::Mutex::new(()),
            segment: std::sync::Mutex::new(None),
            stats: std::sync::Mutex::new(StoreStats::default()),
            _lock: lock,
            model_cache: std::sync::Mutex::new(crate::cache::model::ModelCache::new(64)),
            dsfb: std::sync::Mutex::new(crate::dsfb::observer::StorageObserver::default()),
            inode_locks: std::sync::Arc::new(InodeLockTable::default()),
            perf: std::sync::Arc::new(crate::perf::Timings::default()),
            foreground: config.foreground,
            worker_pool: std::sync::atomic::AtomicBool::new(false),
            epoch: std::sync::Mutex::new(crate::store::epoch::Epoch::default()),
            epoch_pending: std::sync::atomic::AtomicU64::new(0),
            io,
        };
        // Phase-10D: replay any un-checkpointed mutation log tail left by
        // a process crash (the last checkpoint root is authoritative; the
        // log records with a higher sequence are the acknowledged-but-
        // unmerged mutations).
        store
            .stats
            .lock()
            .expect("stats poisoned")
            .physical_capacity = store.physical_capacity();
        store.open_segment(sb.segment_seq)?;
        // Deep-verify the chosen root quickly (structural).
        recovery::verify_root(&store)?;
        // Phase-10D: replay any un-checkpointed mutation log tail left by
        // a process crash (the last checkpoint root is authoritative; log
        // envelopes with a higher sequence are the acknowledged-but-
        // unmerged mutations). The replay commits its own checkpoint root
        // and runs a durability barrier, so the mounted state is fully
        // consistent.
        store.epoch_replay()?;
        Ok(store)
    }

    /// Recovery root selection: prefer the highest-generation superblock
    /// slot whose root object decodes with a matching generation; fall
    /// back to the newest valid ROOT record in the segments (see
    /// [`Store::open`]).
    fn choose_root(
        pair: &SuperblockPair,
        dir: &Path,
        object_index: &ObjectIndex,
        config: &StoreConfig,
    ) -> Result<(Superblock, Root), StoreError> {
        // 1. Superblock slots, highest generation first.
        let mut slots: Vec<Superblock> = [pair.a.clone(), pair.b.clone()]
            .into_iter()
            .flatten()
            .collect();
        slots.sort_by_key(|s| std::cmp::Reverse(s.generation));
        for sb in slots {
            if let Some(root) = load_root_for(&sb, dir, object_index)? {
                if root.generation == sb.generation {
                    return Ok((sb, root));
                }
            }
        }
        // 2. Fallback: the newest valid root record in the segments (the
        //    last complete transaction; power loss may have destroyed the
        //    slot-referenced roots of un-fsynced commits).
        segment::scan_newest_root(dir, config.max_records_per_segment)
            .map_err(|e| StoreError::Io(e.to_string()))?
            .ok_or_else(|| {
                StoreError::Superblock(
                    "no valid root: slot roots missing and no root record in segments".into(),
                )
            })
    }

    /// The store directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The resource limits.
    pub fn limits(&self) -> &Limits {
        &self.config.limits
    }

    /// The cost policy.
    pub fn policy(&self) -> &Policy {
        &self.config.policy
    }

    /// The store configuration.
    pub fn config(&self) -> &StoreConfig {
        &self.config
    }

    /// The current committed root (a snapshot copy; readers never hold
    /// the commit lock across a traversal).
    pub fn current_root(&self) -> Root {
        self.commit
            .read()
            .expect("commit state poisoned")
            .root
            .clone()
    }

    /// The committed generation.
    pub fn generation(&self) -> u64 {
        self.commit
            .read()
            .expect("commit state poisoned")
            .generation
    }

    /// Current segment sequence.
    pub fn current_segment_seq(&self) -> u64 {
        self.segment
            .lock()
            .expect("segment poisoned")
            .as_ref()
            .map(|w| w.seq())
            .unwrap_or(0)
    }

    /// The object index (derived; for GC/fsck and the read path).
    pub fn object_index(&self) -> &ObjectIndex {
        &self.object_index
    }

    /// Phase-10A write-path phase timings (diagnostic).
    pub fn perf(&self) -> &std::sync::Arc<crate::perf::Timings> {
        &self.perf
    }

    /// The Phase-10B foreground representation policy.
    pub fn foreground_policy(&self) -> crate::optimizer::foreground::ForegroundPolicy {
        self.foreground
    }

    /// Phase-11E: route this store's search/decode work through the
    /// process-wide [`workers::POOL`] instead of the 11C semaphore. The
    /// caller MUST have bound the pool to this store's `Arc` first
    /// (`workers::POOL.bind(&store)`); the pool is a global resource, so
    /// only one store may be pool-active at a time (the probe test holds
    /// `workers::tests::POOL_LOCK`). Every other store keeps the flag
    /// false and the semaphore path untouched — this is what lets the
    /// probe run inside the shared test binary without hijacking other
    /// tests' writes, and keeps the FUSE daemon's default unchanged until
    /// a mount opts in (`--worker-pool N`).
    pub fn enable_worker_pool(&self) {
        self.worker_pool
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// The committed stats (copied: `StoreStats` is `Copy`).
    pub fn stats(&self) -> StoreStats {
        *self.stats.lock().expect("stats poisoned")
    }

    /// Feature bits in use.
    pub fn features_in_use(&self) -> u64 {
        self.commit
            .read()
            .expect("commit state poisoned")
            .features_in_use
    }

    /// The DSFB search plan for a chunk (trust-ordered, budget-bounded).
    pub fn dsfb_plan(
        &self,
        key: &crate::dsfb::features::ChunkKey,
    ) -> crate::dsfb::selection::SearchPlan {
        self.dsfb.lock().expect("dsfb poisoned").plan(key)
    }

    /// DSFB trust for one channel of a chunk.
    pub fn dsfb_trust(
        &self,
        key: &crate::dsfb::features::ChunkKey,
        channel: crate::dsfb::features::Channel,
    ) -> f64 {
        self.dsfb.lock().expect("dsfb poisoned").trust(key, channel)
    }

    /// Feed the DSFB observer (performance-only state). Bounded eviction
    /// keeps the observer from growing without limit.
    pub fn dsfb_observe(
        &self,
        key: crate::dsfb::features::ChunkKey,
        measurements: &[(crate::dsfb::features::Channel, f64)],
        winner: crate::dsfb::features::Channel,
        outcome_quality: f64,
    ) -> crate::dsfb::drift::Regime {
        let mut dsfb = self.dsfb.lock().expect("dsfb poisoned");
        let regime = dsfb.observe(key, measurements, winner, outcome_quality);
        if dsfb.len() > DSFB_MAX_CHUNKS {
            dsfb.evict_one();
        }
        regime
    }

    /// Observer statistics (for `status`).
    pub fn dsfb_stats(&self) -> crate::dsfb::observer::ObserverStats {
        self.dsfb.lock().expect("dsfb poisoned").stats
    }

    /// Materialize the chunk at `offset` of `ino` as a candidate base, but
    /// only when its content id resolves in the chunk index (a future
    /// reader resolves `BaseResidual.base` through the chunk index, so an
    /// unresolvable base would be undecodable). Depth reflects the base
    /// chunk's own chain depth so chains are cost-accounted.
    pub fn base_chunk_at(
        &self,
        ino: u64,
        offset: u64,
        len: usize,
    ) -> Result<Option<crate::core::candidate::BaseChunk>, StoreError> {
        if len == 0 {
            return Ok(None);
        }
        let bytes = self.read_file(ino, offset, len as u64)?;
        if bytes.is_empty() {
            return Ok(None);
        }
        // A shorter-than-requested result is a valid prefix base (the
        // EOF tail): the shift-aware delta family accepts bases of any
        // length (insertions make targets longer than their bases). Holes
        // materialize as zeros that resolve to nothing in the chunk index
        // (or to a zero chunk, which the delta gate rejects as literals),
        // so this cannot fabricate a meaningful base.
        self.base_chunk_from_bytes(&bytes)
    }

    /// Build a candidate base from already-materialized bytes (the write
    /// path's RMW read) without re-reading the store.
    pub fn base_chunk_from_bytes(
        &self,
        bytes: &[u8],
    ) -> Result<Option<crate::core::candidate::BaseChunk>, StoreError> {
        let id = crate::core::extent::ChunkId::of(bytes);
        let Some(desc_bytes) = self.chunk_descriptor(&id)? else {
            return Ok(None);
        };
        let limits = self.config.limits;
        let desc = match crate::format::descriptor::decode(&desc_bytes, &limits) {
            Ok(d) => d,
            Err(_) => return Ok(None),
        };
        let depth = crate::optimizer::rebase::chain_depth(self, &desc);
        Ok(Some(crate::core::candidate::BaseChunk {
            id,
            bytes: bytes.to_vec(),
            depth,
        }))
    }

    /// The current descriptor bytes of the extent covering `offset` of
    /// `ino` (None when the region is a hole). Used as the CAS token by
    /// the background optimizer (§25).
    pub fn extent_descriptor(&self, ino: u64, offset: u64) -> Result<Option<Vec<u8>>, StoreError> {
        let inode = self
            .get_inode(ino)?
            .ok_or_else(|| StoreError::Invariant(format!("inode {ino} missing")))?;
        let extent_root = match inode.data {
            InodeData::File { extent_root } => extent_root,
            _ => return Err(StoreError::Invariant("not a regular file".into())),
        };
        if extent_root.is_zero() {
            return Ok(None);
        }
        let entry = crate::store::extent_tree::covering(
            extent_root,
            offset,
            BTREE_ORDER,
            self.config.limits.max_fanout,
            self,
        )?;
        Ok(entry.map(|(_, bytes)| bytes))
    }

    /// Phase-10E convergence: after a background rewrite pass, the derived
    /// chunk index can resolve a content id to a DEEPER descriptor than the
    /// one a previously-committed extent validated against (a later rewrite
    /// of the same content re-encodes it deeper and replaces the index
    /// entry; references are resolved through the index at materialize
    /// time). Any extent whose full reference chain now exceeds
    /// `max_reference_depth` is unreadable (`DepthExceeded`). Rebase those
    /// extents to a depth-0 encoding. Returns the number of extents
    /// rebased; a no-op when the cap is respected (the steady state).
    pub fn rebase_overdepth_extents(
        &self,
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<u64, StoreError> {
        let mut rebased = 0u64;
        let limits = self.config.limits;
        // Relaxed decode budget for recovering the bytes of an over-depth
        // chain: the chain is acyclic and bounded by the write-path gates
        // and the walk caps, so a generous depth cap recovers the content
        // without looping.
        let mut relaxed = limits;
        relaxed.max_reference_depth = 64;
        let inos = self.all_inodes()?;
        for ino in inos {
            let Some(inode) = self.get_inode(ino)? else {
                continue;
            };
            let extent_root = match inode.data {
                InodeData::File { extent_root } => extent_root,
                _ => continue,
            };
            if extent_root.is_zero() {
                continue;
            }
            let entries = crate::store::extent_tree::scan_all(
                extent_root,
                BTREE_ORDER,
                limits.max_fanout,
                self,
            )?;
            for (start, desc_bytes) in entries {
                let desc = match crate::format::descriptor::decode(&desc_bytes, &limits) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                if crate::optimizer::rebase::chain_depth(self, &desc) <= limits.max_reference_depth
                {
                    continue;
                }
                // Recover the logical bytes under the relaxed budget, then
                // re-encode at depth 0 (§32-gated like every other write).
                let bytes = crate::core::materialize::materialize_to_vec(&desc, self, &relaxed)
                    .map_err(|e| StoreError::Descriptor(e.to_string()))?;
                let cid = crate::core::extent::ChunkId::of(&bytes);
                // CAS: skip if the extent changed since the scan.
                let _lock = self.inode_lock(ino);
                let current = self.extent_descriptor(ino, start)?;
                if current.as_deref() != Some(desc_bytes.as_slice()) {
                    continue;
                }
                let flat = Store::encode_chunk(&bytes, start, cid, &limits, &self.config.policy)?;
                self.validate_update(&flat)?;
                self.commit_file_extents(ino, vec![flat], None, hooks)?;
                rebased += 1;
            }
        }
        Ok(rebased)
    }

    // ------------------------------------------------------------------
    // Segments
    // ------------------------------------------------------------------

    fn open_segment(&self, seq: u64) -> Result<(), StoreError> {
        let w = SegmentWriter::open(&self.io, seq)?;
        *self.segment.lock().expect("segment poisoned") = Some(w);
        Ok(())
    }

    /// The storage transport (Phase 10F).
    pub(crate) fn io(&self) -> &std::sync::Arc<dyn crate::store::io::IoBackend> {
        &self.io
    }

    /// Replace the current segment writer (offline GC compaction).
    pub(crate) fn install_segment(&self, w: SegmentWriter) {
        *self.segment.lock().expect("segment poisoned") = Some(w);
    }

    /// ENOSPC guard: refuse a commit before staging anything when the
    /// projected physical usage would exceed the high watermark (§21/§22).
    /// The watermark leaves the GC emergency reserve untouched so recovery
    /// never needs space it cannot have.
    pub(crate) fn ensure_commit_space(
        &self,
        records: &[crate::store::transaction::PendingRecord],
    ) -> Result<(), StoreError> {
        let capacity = self.physical_capacity();
        if capacity == 0 {
            // No statvfs info (unusual); do not refuse writes on that basis
            // alone — the segment append still bounds them.
            return Ok(());
        }
        let mut projected = self.physical_used();
        if let Some(w) = self.segment.lock().expect("segment poisoned").as_ref() {
            projected = projected.saturating_add(w.buffered_len());
        }
        for pending in records {
            let flags = if pending.materialized_len.is_some() {
                FLAG_HAS_MATERIALIZED_LEN
            } else {
                0
            };
            let encoded = encode_record(
                pending.tag,
                flags,
                pending.materialized_len,
                &pending.payload,
            );
            projected = projected.saturating_add(encoded.len() as u64);
        }
        let watermark = (capacity as f64 * self.config.gc_high_watermark) as u64;
        if projected > watermark {
            return Err(StoreError::Full(format!(
                "commit would use {projected} of {capacity} bytes (watermark {watermark}); \
                 delete data or run GC first"
            )));
        }
        Ok(())
    }

    /// Append pending records (raw payloads) to the current segment,
    /// encoding each envelope with its flags; rolls the segment when full.
    /// Serialized by the commit coordinator (`commit_lock`).
    fn append_records(
        &self,
        records: &mut Vec<crate::store::transaction::PendingRecord>,
    ) -> Result<(), StoreError> {
        for pending in records.drain(..) {
            let flags = if pending.materialized_len.is_some() {
                FLAG_HAS_MATERIALIZED_LEN
            } else {
                0
            };
            let encoded = encode_record(
                pending.tag,
                flags,
                pending.materialized_len,
                &pending.payload,
            );
            let offset = {
                let mut seg = self.segment.lock().expect("segment poisoned");
                let w = seg.as_mut().ok_or(StoreError::NotOpen)?;
                let base = w.durable_end() + w.buffered_len();
                if base + encoded.len() as u64 > self.config.segment_size {
                    // Roll: flush + sync current, open the next. The
                    // segment lock is released before re-acquiring it in
                    // `open_segment`.
                    w.flush()?;
                    w.fdatasync()?;
                    let next = w.seq() + 1;
                    drop(seg);
                    self.open_segment(next)?;
                    self.io.sync_segments_dir()?;
                    base_after_roll(self, &encoded)
                } else {
                    base
                }
            };
            let mut seg = self.segment.lock().expect("segment poisoned");
            let w = seg.as_mut().ok_or(StoreError::NotOpen)?;
            w.append(encoded);
            self.object_index.insert(
                ChunkId::of(&pending.payload),
                Location {
                    segment_seq: w.seq(),
                    offset,
                    stored_len: pending.payload.len() as u64,
                    materialized_len: pending.materialized_len,
                    tag: pending.tag,
                },
            );
        }
        Ok(())
    }

    /// fdatasync the current segment.
    pub fn fdatasync_segment(&self) -> Result<(), StoreError> {
        let mut seg = self.segment.lock().expect("segment poisoned");
        if let Some(w) = seg.as_mut() {
            // Flush buffered bytes first (the caller appended them).
            w.flush()?;
            w.fdatasync()?;
        }
        Ok(())
    }

    /// Flush buffered segment bytes.
    pub fn flush_segment(&self) -> Result<(), StoreError> {
        let mut seg = self.segment.lock().expect("segment poisoned");
        if let Some(w) = seg.as_mut() {
            w.flush()?;
        }
        Ok(())
    }

    /// Ensure the segments directory entries are durable (Phase 10F: via
    /// the storage transport).
    pub fn sync_segments_dir(&self) -> Result<(), StoreError> {
        self.io.sync_segments_dir()
    }

    /// Fetch an object payload by content id (Phase-10E/10F: via the
    /// backend's cached segment read handles + offset-based reads, so
    /// concurrent reads never re-open files and never share a seek
    /// position).
    pub fn fetch_object(&self, id: &ChunkId) -> Result<Option<Vec<u8>>, StoreError> {
        match self.object_index.get(id) {
            Some(loc) => Ok(Some(self.io.read_payload(
                loc.segment_seq,
                loc.offset,
                loc.stored_len,
            )?)),
            None => Ok(None),
        }
    }

    /// Fetch a record payload by location (fsck; also cached-fd reads).
    pub fn read_payload_at(&self, loc: &Location) -> Result<Vec<u8>, StoreError> {
        self.io
            .read_payload(loc.segment_seq, loc.offset, loc.stored_len)
    }

    /// Phase-10F `read_many`: fetch many object payloads in ONE backend
    /// call (one submission queue for `UringIo`). The i-th result
    /// corresponds to the i-th id: `Ok(Some(bytes))` when present,
    /// `Ok(None)` when the object index has no record for it (a committed
    /// root can never reference such an id; the overlay paths use this for
    /// pending-chunk resolution), or `Err` for I/O failures.
    pub fn fetch_objects_many(&self, ids: &[ChunkId]) -> Vec<Result<Option<Vec<u8>>, StoreError>> {
        let mut reqs: Vec<Option<crate::store::io::ReadRequest>> = Vec::with_capacity(ids.len());
        let mut any = false;
        for id in ids {
            match self.object_index.get(id) {
                Some(loc) => {
                    reqs.push(Some(crate::store::io::ReadRequest {
                        segment_seq: loc.segment_seq,
                        offset: loc.offset,
                        stored_len: loc.stored_len,
                    }));
                    any = true;
                }
                None => reqs.push(None),
            }
        }
        if !any {
            return ids.iter().map(|_| Ok(None)).collect();
        }
        let need: Vec<usize> = (0..reqs.len()).filter(|&i| reqs[i].is_some()).collect();
        let batch: Vec<crate::store::io::ReadRequest> = need
            .iter()
            .map(|&i| reqs[i].expect("filtered to Some"))
            .collect();
        let results = self.io.read_many(&batch);
        let mut out: Vec<Result<Option<Vec<u8>>, StoreError>> =
            ids.iter().map(|_| Ok(None)).collect();
        for (k, r) in results.into_iter().enumerate() {
            let idx = need[k];
            out[idx] = r.map(Some);
        }
        out
    }

    // ------------------------------------------------------------------
    // Superblock / commit
    // ------------------------------------------------------------------

    /// Write the inactive superblock slot for the new root (page cache;
    /// fsync at the barrier). Runs under the commit coordinator
    /// (`commit_lock`). Phase 10F: through the storage transport.
    pub fn write_superblock(&self, root_id: ChunkId, root: &Root) -> Result<(), StoreError> {
        let mut cs = self.commit.write().expect("commit state poisoned");
        let mut sb = cs.superblock.clone();
        sb.generation = root.generation;
        sb.root_object_id = root_id;
        sb.segment_seq = root.segment_seq;
        sb.incompat = cs.features_in_use;
        let offset = match root.generation & 1 {
            0 => SUPERBLOCK_SLOT_A_OFFSET,
            _ => SUPERBLOCK_SLOT_B_OFFSET,
        };
        self.io.write_superblock_slot(offset, &sb.encode())?;
        cs.superblock = sb;
        Ok(())
    }

    /// fsync the superblock file (commit durable).
    pub fn fsync_superblock(&self) -> Result<(), StoreError> {
        self.io.fsync_superblock()
    }

    /// The durability barrier (ADR-0008, Phase 6): makes the current
    /// in-memory root durable — segment fdatasync, segment-directory sync,
    /// superblock slot write, superblock fsync. Called by `fsync()`; also
    /// the final step of a full `Tx::commit`. A power loss may lose every
    /// deferred commit since the last barrier (POSIX: only fsync'd data is
    /// power-durable), but recovery can never wedge: it validates the
    /// chosen slot's root and falls back to the newest valid root record
    /// in the segments.
    pub fn durability_barrier(
        &self,
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<(), StoreError> {
        // Phase-11B: the fsync path gets its own envelope (pass-through
        // inside an outer request). The checkpoint's cp_* rows and the
        // barrier rows below partition it.
        let _req = self.perf.request("durability_barrier");
        // Phase-10D: the barrier also makes the epoch's acknowledged
        // mutations power-durable — checkpoint the epoch first (its own
        // commit is then covered by this barrier; a no-op when empty). The
        // cp_* exclusive rows inside `epoch_checkpoint` attach here.
        self.epoch_checkpoint(hooks)?;
        // Serialize with in-flight commits: the barrier holds the commit
        // lock across [fdatasync -> superblock fsync] because a commit
        // that completed mid-barrier would ack after the fsync started but
        // before its cut — the fsync would then report durable writes the
        // barrier never covered, breaking write->fsync durability
        // linearizability (the crash courts pin this). The hold is why
        // concurrent fsyncs queue: the "fsync convoy" the 11B/11C
        // reconciliation measured as `commit_lock_wait` (34.7% of
        // 16-thread request time pre-11C, 10.8-16.4% after). It is
        // contract-inherent; a future "group durability" could amortize
        // the physical barrier across concurrent fsyncs (each waiter
        // completes only after a cut that includes its writes) without
        // removing the linearizability requirement.
        let _guard = self.perf.time_request("barrier_commit_lock_wait", || {
            self.commit_lock.lock().expect("commit lock poisoned")
        });
        // Records have been appended (by the deferred commit(s)); the
        // segment has not been fdatasync'd yet.
        hooks.hit(crate::store::transaction::CrashPoint::AfterRecordAppend)?;
        // 1. fdatasync the affected segment.
        self.perf
            .time_request("barrier_fdatasync", || self.fdatasync_segment())?;
        hooks.hit(crate::store::transaction::CrashPoint::AfterSegmentFdatasync)?;
        // 2. new segment directory entries durable.
        self.perf
            .time_request("barrier_dir_sync", || self.sync_segments_dir())?;
        hooks.hit(crate::store::transaction::CrashPoint::AfterSegmentDirFsync)?;
        // 3. write the inactive superblock slot (idempotent: the deferred
        //    commit already wrote it to the page cache) and fsync it.
        let root = self.current_root();
        let root_id = root.id();
        self.perf
            .time_request("barrier_sb_write", || self.write_superblock(root_id, &root))?;
        hooks.hit(crate::store::transaction::CrashPoint::AfterSuperblockWrite)?;
        self.perf
            .time_request("barrier_sb_fsync", || self.fsync_superblock())?;
        hooks.hit(crate::store::transaction::CrashPoint::AfterSuperblockFsync)?;
        Ok(())
    }

    /// Publish a committed root to the in-memory state (under the commit
    /// coordinator).
    pub fn publish_commit(&self, root: &Root, _root_id: ChunkId) -> Result<(), StoreError> {
        let mut cs = self.commit.write().expect("commit state poisoned");
        cs.root = root.clone();
        cs.generation = root.generation;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Begin transaction
    // ------------------------------------------------------------------

    /// Begin a write transaction. Takes the commit coordinator lock (held
    /// until commit): the transaction application and root publication are
    /// serialized, while candidate encoding (the expensive part of a
    /// write) happens before `begin_tx` and runs concurrently.
    pub fn begin_tx(&self) -> Result<crate::store::transaction::Tx<'_>, StoreError> {
        let guard = self.commit_lock.lock().expect("commit lock poisoned");
        // Ensure the segment writer is present.
        if self.segment.lock().expect("segment poisoned").is_none() {
            self.open_segment(self.current_root().segment_seq)?;
        }
        Ok(crate::store::transaction::Tx::begin(self, guard))
    }

    /// The per-inode mutation lock (file-data writes and truncates).
    pub fn inode_lock(&self, ino: u64) -> std::sync::MutexGuard<'_, ()> {
        self.inode_locks.lock(ino)
    }

    // ------------------------------------------------------------------
    // Inode index
    // ------------------------------------------------------------------

    /// Look up an inode by number.
    pub fn get_inode(&self, ino: u64) -> Result<Option<Inode>, StoreError> {
        let key = ino.to_be_bytes();
        match index::get(
            self.current_root().inode_index_root,
            &key,
            BTREE_ORDER,
            self.config.limits.max_fanout,
            self,
        )? {
            Some(id_bytes) => {
                let inode_id =
                    ChunkId::new(id_bytes.as_slice().try_into().map_err(|_| {
                        StoreError::Invariant("inode index value not an id".into())
                    })?);
                let payload = self.fetch_object(&inode_id)?.ok_or_else(|| {
                    StoreError::Invariant(format!("inode object {inode_id} missing"))
                })?;
                Inode::decode(&payload)
                    .map(Some)
                    .map_err(|e| StoreError::Descriptor(e.to_string()))
            }
            None => Ok(None),
        }
    }

    /// Insert/update an inode in the index (within a transaction).
    pub fn put_inode_in_tx(
        tx: &mut crate::store::transaction::Tx<'_>,
        ino: u64,
        inode: &Inode,
    ) -> Result<(), StoreError> {
        let inode_id = crate::store::transaction::put_inode(tx, inode);
        let key = ino.to_be_bytes();
        tx.root_mut().inode_index_root = index::insert(
            tx.root_mut().inode_index_root,
            &key,
            inode_id.as_bytes(),
            BTREE_ORDER,
            tx.store.config.limits.max_fanout,
            tx,
        )?;
        Ok(())
    }

    /// Remove an inode from the index (unlink of the last link).
    pub fn remove_inode_in_tx(
        tx: &mut crate::store::transaction::Tx<'_>,
        ino: u64,
    ) -> Result<(), StoreError> {
        let key = ino.to_be_bytes();
        tx.root_mut().inode_index_root = index::remove(
            tx.root_mut().inode_index_root,
            &key,
            BTREE_ORDER,
            tx.store.config.limits.max_fanout,
            tx,
        )?;
        Ok(())
    }

    /// All inode numbers (for fsck/GC; bounded by the index size).
    pub fn all_inodes(&self) -> Result<Vec<u64>, StoreError> {
        let entries = index::scan_all(
            self.current_root().inode_index_root,
            BTREE_ORDER,
            self.config.limits.max_fanout,
            self,
        )?;
        Ok(entries
            .into_iter()
            .map(|(k, _)| u64::from_be_bytes(k.as_slice().try_into().expect("8-byte ino")))
            .collect())
    }

    // ------------------------------------------------------------------
    // Chunk index (content id → descriptor bytes)
    // ------------------------------------------------------------------

    /// Look up a chunk descriptor by content id.
    pub fn chunk_descriptor(&self, cid: &ChunkId) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(index::get(
            self.current_root().chunk_index_root,
            cid.as_bytes(),
            BTREE_ORDER,
            self.config.limits.max_fanout,
            self,
        )?)
    }

    /// Insert a chunk descriptor (within a transaction).
    pub fn put_chunk_in_tx(
        tx: &mut crate::store::transaction::Tx<'_>,
        cid: &ChunkId,
        descriptor: &Representation,
    ) -> Result<(), StoreError> {
        if descriptor.validate(&tx.store.config.limits).is_err() {
            return Err(StoreError::Descriptor("invalid descriptor".into()));
        }
        // The chunk index must never resolve a content id to a descriptor
        // that references the same content id: `EXACT_REF{target: cid}`
        // inserted for `cid` loops forever at decode (the self-aliasing
        // extent stays valid — it resolves through the retained terminal
        // entry).
        if let Representation::ExactRef { target, .. } = descriptor {
            if *target == *cid {
                return Ok(());
            }
        }
        // Track incompat features.
        let mut features = {
            tx.store
                .commit
                .read()
                .expect("commit state poisoned")
                .features_in_use
        };
        match descriptor {
            Representation::EntropyRef { .. } => {
                features |= crate::format::features::Feature::EntropyRef.mask();
            }
            Representation::Palette { .. } => {
                features |= crate::format::features::Feature::Palette.mask();
            }
            Representation::Permutation { .. } => {
                features |= crate::format::features::Feature::Permutation.mask();
            }
            Representation::SequenceRans { .. } => {
                features |= crate::format::features::Feature::SequenceRans.mask();
            }
            Representation::SparseBlock64 { .. } => {
                features |= crate::format::features::Feature::SparseBlock64.mask();
            }
            Representation::SequenceDict { .. } => {
                features |= crate::format::features::Feature::SequenceDict.mask();
            }
            Representation::SequenceSharedDict { .. } => {
                features |= crate::format::features::Feature::SequenceSharedDict.mask();
            }
            Representation::SequenceDeep { .. } => {
                features |= crate::format::features::Feature::SequenceDeep.mask();
            }
            _ => {}
        }
        let bytes = crate::format::descriptor::encode(descriptor)?;
        tx.root_mut().chunk_index_root = index::insert(
            tx.root_mut().chunk_index_root,
            cid.as_bytes(),
            &bytes,
            BTREE_ORDER,
            tx.store.config.limits.max_fanout,
            tx,
        )?;
        // Feature bits are recorded on the commit state (the tx runs under
        // the commit coordinator).
        tx.store
            .commit
            .write()
            .expect("commit state poisoned")
            .features_in_use |= features;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Extent tree
    // ------------------------------------------------------------------

    /// Insert an extent (within a transaction) for the given inode.
    pub fn put_extent_in_tx(
        tx: &mut crate::store::transaction::Tx<'_>,
        ino: u64,
        offset: u64,
        descriptor: &Representation,
    ) -> Result<(), StoreError> {
        let bytes = crate::format::descriptor::encode(descriptor)?;
        let inode = Store::inode_for_tx(tx, ino)?;
        let (new_root, mut inode) = match inode.data {
            InodeData::File { extent_root } => {
                let new_root = crate::store::extent_tree::insert(
                    extent_root,
                    offset,
                    &bytes,
                    BTREE_ORDER,
                    tx.store.config.limits.max_fanout,
                    tx,
                )?;
                (new_root, inode)
            }
            _ => return Err(StoreError::Invariant("not a regular file".into())),
        };
        inode.data = InodeData::File {
            extent_root: new_root,
        };
        inode.ctime = crate::store::inode::Timespec::now();
        Store::put_inode_in_tx(tx, ino, &inode)?;
        Ok(())
    }

    /// Remove an extent (within a transaction).
    pub fn remove_extent_in_tx(
        tx: &mut crate::store::transaction::Tx<'_>,
        ino: u64,
        offset: u64,
    ) -> Result<(), StoreError> {
        let inode = Store::inode_for_tx(tx, ino)?;
        let (new_root, _) = match inode.data {
            InodeData::File { extent_root } => crate::store::extent_tree::remove(
                extent_root,
                offset,
                BTREE_ORDER,
                tx.store.config.limits.max_fanout,
                tx,
            )?,
            _ => return Err(StoreError::Invariant("not a regular file".into())),
        };
        let mut inode = inode;
        inode.data = InodeData::File {
            extent_root: new_root,
        };
        Store::put_inode_in_tx(tx, ino, &inode)?;
        Ok(())
    }

    fn inode_for_tx(tx: &crate::store::transaction::Tx<'_>, ino: u64) -> Result<Inode, StoreError> {
        let key = ino.to_be_bytes();
        match index::get(
            tx.root().inode_index_root,
            &key,
            BTREE_ORDER,
            tx.store.config.limits.max_fanout,
            tx,
        )? {
            Some(id_bytes) => {
                let inode_id =
                    ChunkId::new(id_bytes.as_slice().try_into().map_err(|_| {
                        StoreError::Invariant("inode index value not an id".into())
                    })?);
                let payload = tx.fetch_pending_or_store(&inode_id)?.ok_or_else(|| {
                    StoreError::Invariant(format!("inode object {inode_id} missing"))
                })?;
                Inode::decode(&payload).map_err(|e| StoreError::Descriptor(e.to_string()))
            }
            None => Err(StoreError::Invariant(format!("inode {ino} missing"))),
        }
    }

    // ------------------------------------------------------------------
    // Read path
    // ------------------------------------------------------------------

    /// Materialized byte range of a file.
    pub fn read_file(&self, ino: u64, offset: u64, len: u64) -> Result<Vec<u8>, StoreError> {
        let inode = self
            .get_inode(ino)?
            .ok_or_else(|| StoreError::Invariant(format!("inode {ino} missing")))?;
        if !inode.is_file() {
            return Err(StoreError::Invariant("not a regular file".into()));
        }
        // Reads are clipped to the file size; holes (gaps between extents
        // and everything past the last extent) materialize as ZERO and stay
        // in the output buffer — never truncated away.
        let avail = inode.size.saturating_sub(offset).min(len);
        let end = offset.saturating_add(avail);
        let extent_root = match &inode.data {
            InodeData::File { extent_root } => *extent_root,
            _ => unreachable!(),
        };
        // Phase-10E: one RANGE TRAVERSAL per read: collect the covering
        // extents in a single B-tree walk instead of a per-chunk descent.
        // The extent COVERING `offset` may start before it; begin the scan
        // at its start (a predecessor lookup) so it is included.
        let scan_start = match crate::store::extent_tree::covering(
            extent_root,
            offset,
            BTREE_ORDER,
            self.config.limits.max_fanout,
            self,
        )? {
            Some((start, _)) => start,
            None => offset,
        };
        // Phase-10F: LEVEL-ORDER batched scan (one read_many per tree
        // level), then ONE prefetch submission for the materialization
        // dependencies, then parallel decode.
        let extents = self.perf.time_request("read_scan", || {
            self.scan_extents_batched(extent_root, scan_start, end)
        })?;
        self.materialize_range_batched(None, &extents, offset, end, avail as usize)
    }

    /// Phase-10F batched extent scan: LEVEL-ORDER descent with ONE
    /// `read_many` per tree level (sibling node fetches batch into a
    /// single submission for `UringIo`), replacing the per-node sequential
    /// fetch of the DFS `scan_range` in the read path. Returns the extents
    /// whose start lies in `[start_offset, end_offset)`, in offset order.
    /// (The covering extent for `start_offset` — which may start before
    /// it — is found by the caller's `covering` descent, unchanged.)
    fn scan_extents_batched(
        &self,
        extent_root: ChunkId,
        start_offset: u64,
        end_offset: u64,
    ) -> Result<Vec<(u64, Vec<u8>)>, StoreError> {
        if extent_root.is_zero() || end_offset <= start_offset {
            return Ok(Vec::new());
        }
        let order = BTREE_ORDER;
        let fanout = self.config.limits.max_fanout;
        let start_key = crate::store::extent_tree::extent_key(start_offset);
        let end_key = crate::store::extent_tree::extent_key(end_offset);
        let mut out: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut frontier: Vec<ChunkId> = vec![extent_root];
        while !frontier.is_empty() {
            // Fetch every frontier node in ONE read_many.
            let results = self.fetch_objects_many(&frontier);
            let mut next: Vec<ChunkId> = Vec::new();
            for (i, node_id) in frontier.iter().enumerate() {
                let bytes = match &results[i] {
                    Ok(Some(b)) => b.clone(),
                    Ok(None) => {
                        return Err(StoreError::Invariant(format!(
                            "extent tree node {node_id} missing (batched scan)"
                        )));
                    }
                    Err(e) => return Err(e.clone()),
                };
                match crate::store::index::Node::decode(&bytes, order, fanout) {
                    Ok(crate::store::index::Node::Leaf { entries }) => {
                        for e in entries {
                            if e.key.as_slice() < start_key.as_slice() {
                                continue;
                            }
                            if e.key.as_slice() >= end_key.as_slice() {
                                break; // sorted: past the range
                            }
                            let start = u64::from_be_bytes(
                                e.key.as_slice().try_into().expect("8-byte extent key"),
                            );
                            out.push((start, e.value));
                        }
                    }
                    Ok(crate::store::index::Node::Internal {
                        first_child,
                        entries,
                    }) => {
                        // Child ranges: first_child covers [.., k0); child_i
                        // covers [k_i, k_{i+1}) (last: [k_last, ..)).
                        // Collect the children whose range intersects
                        // [start_key, end_key).
                        if entries
                            .first()
                            .is_none_or(|e| start_key.as_slice() < e.key.as_slice())
                        {
                            next.push(first_child);
                        }
                        for (i, e) in entries.iter().enumerate() {
                            if e.key.as_slice() >= end_key.as_slice() {
                                break;
                            }
                            let child_below_start = entries
                                .get(i + 1)
                                .is_some_and(|n| n.key.as_slice() <= start_key.as_slice());
                            if !child_below_start {
                                next.push(child_id_value(&e.value));
                            }
                        }
                    }
                    Err(e) => {
                        return Err(StoreError::Index(format!(
                            "batched extent scan: node {node_id}: {e}"
                        )));
                    }
                }
            }
            frontier = next;
        }
        Ok(out)
    }

    /// Phase-10F read_many: enumerate every object a set of extent
    /// descriptors needs to materialize — the direct object dependencies
    /// of each descriptor (model + encoded stream for the entropy
    /// families, the raw object, the base for BASE_RESIDUAL), plus the
    /// objects of nested base/target/dictionary descriptors resolved
    /// through the chunk index, depth-capped. The read path then fetches
    /// them in ONE backend call (one submission queue for `UringIo`) and
    /// decodes from the prefetched map.
    fn collect_read_deps(
        &self,
        ep: Option<&crate::store::epoch::Epoch>,
        desc: &Representation,
        depth: u8,
        deps: &mut Vec<ChunkId>,
        seen_objects: &mut std::collections::HashSet<ChunkId>,
        seen_nested: &mut std::collections::HashSet<ChunkId>,
        // Phase-11C: every nested descriptor resolved here (pending
        // overlay or committed) is captured so the decode half can run
        // without the epoch guard.
        nested_descriptors: &mut std::collections::HashMap<ChunkId, Vec<u8>>,
    ) -> Result<(), StoreError> {
        let limits = self.config.limits;
        for oid in crate::store::transaction::descriptor_objects(desc, &limits) {
            if seen_objects.insert(oid) {
                deps.push(oid);
            }
        }
        if depth >= limits.max_reference_depth {
            return Ok(());
        }
        // Nested chunk references (EXACT_REF targets, BASE_RESIDUAL bases,
        // and the dictionary/shared-dictionary chunks of the SEQUENCE_DICT
        // families): resolve the nested descriptor (epoch overlay first)
        // and recurse so its objects are prefetched too. ZERO ids mean
        // "absent" (no file dictionary).
        let mut nested: Vec<ChunkId> = Vec::new();
        match desc {
            Representation::ExactRef { target, .. } => nested.push(*target),
            Representation::BaseResidual { base, .. } => nested.push(*base),
            Representation::SequenceDict { dictionary, .. } => {
                if !dictionary.is_zero() {
                    nested.push(*dictionary);
                }
            }
            Representation::SequenceSharedDict {
                dictionary, shared, ..
            } => {
                if !dictionary.is_zero() {
                    nested.push(*dictionary);
                }
                nested.push(*shared);
            }
            _ => {}
        }
        for id in nested {
            if !seen_nested.insert(id) {
                continue;
            }
            let bytes = match ep.and_then(|e| e.overlay_chunk(&id)) {
                Some(b) => Some(b),
                None => self.chunk_descriptor(&id)?,
            };
            if let Some(b) = bytes {
                nested_descriptors.insert(id, b.clone());
                if let Ok(d) = crate::format::descriptor::decode(&b, &limits) {
                    self.collect_read_deps(
                        ep,
                        &d,
                        depth + 1,
                        deps,
                        seen_objects,
                        seen_nested,
                        nested_descriptors,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Phase-10F batched materialization: prefetch every dependency of the
    /// merged extents in ONE `read_many`, then decode the extents in
    /// parallel (scoped threads; single-extent reads inline). The batch is
    /// an optimization — every object missing from the prefetch falls back
    /// to the store at decode time, so the bytes are always the exact
    /// committed/overlay state.
    ///
    /// Phase-11C: split into [`Store::materialize_prepare`] (the
    /// guard-dependent half: descriptor decode, dependency enumeration, and
    /// the batched object fetch with the epoch's staged-payload fallback)
    /// and [`Store::materialize_decode`] (pure CPU). The epoch write path
    /// prepares the chunk prefill under the guard, RELEASES it, and decodes
    /// outside — the measured block-A guard hold (`epoch_lock_wait` at
    /// 8–16 threads) was the prefill materialization.
    fn materialize_range_batched(
        &self,
        ep: Option<&crate::store::epoch::Epoch>,
        extents: &[(u64, Vec<u8>)],
        offset: u64,
        end: u64,
        avail: usize,
    ) -> Result<Vec<u8>, StoreError> {
        let prepared = self.materialize_prepare(ep, extents, offset, end, avail)?;
        self.materialize_decode(prepared)
    }

    /// The guard-dependent half of a batched materialization: decode every
    /// descriptor, enumerate its object dependencies, and fetch them in
    /// ONE backend call (the `read_many` win; one submission queue for
    /// `UringIo`). Objects staged by in-flight epoch ops are not in the
    /// object index yet — they resolve from the epoch's staged payloads
    /// (the overlay read must never see a pending descriptor whose objects
    /// are unfetchable). The caller must hold the epoch guard when `ep` is
    /// `Some`; the returned [`PreparedRead`] owns everything the decode
    /// half needs, so it can be decoded WITHOUT the guard.
    fn materialize_prepare(
        &self,
        ep: Option<&crate::store::epoch::Epoch>,
        extents: &[(u64, Vec<u8>)],
        offset: u64,
        end: u64,
        avail: usize,
    ) -> Result<PreparedRead, StoreError> {
        let limits = self.config.limits;
        // 1. Decode every descriptor and enumerate its dependencies.
        let mut starts: Vec<u64> = Vec::with_capacity(extents.len());
        let mut descs: Vec<Representation> = Vec::with_capacity(extents.len());
        let mut deps: Vec<ChunkId> = Vec::new();
        let mut seen_objects: std::collections::HashSet<ChunkId> = std::collections::HashSet::new();
        let mut seen_nested: std::collections::HashSet<ChunkId> = std::collections::HashSet::new();
        // The reference closure's nested descriptors (Phase-11C): the
        // decode half resolves them from this map, without the guard.
        let mut descriptors: std::collections::HashMap<ChunkId, Vec<u8>> =
            std::collections::HashMap::new();
        self.perf.time_request("read_deps", || {
            for (start, bytes) in extents {
                let desc = crate::format::descriptor::decode(bytes, &limits)?;
                self.collect_read_deps(
                    ep,
                    &desc,
                    0,
                    &mut deps,
                    &mut seen_objects,
                    &mut seen_nested,
                    &mut descriptors,
                )?;
                starts.push(*start);
                descs.push(desc);
            }
            Ok::<(), StoreError>(())
        })?;
        // 2. ONE batched fetch.
        let objects: std::collections::HashMap<ChunkId, Vec<u8>> = {
            let results = self
                .perf
                .time_request("read_prefetch", || self.fetch_objects_many(&deps));
            let mut map = std::collections::HashMap::with_capacity(deps.len());
            for (id, r) in deps.iter().zip(results) {
                match r {
                    Ok(Some(b)) => {
                        map.insert(*id, b);
                    }
                    Ok(None) => {
                        if let Some(b) = ep.and_then(|e| e.staged_payloads.get(id)) {
                            map.insert(*id, b.clone());
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
            map
        };
        Ok(PreparedRead {
            starts,
            descs,
            objects,
            descriptors,
            offset,
            end,
            avail,
        })
    }

    /// The pure-CPU decode half of a batched materialization: parallel
    /// decode of the prepared descriptors (scoped threads; single-extent
    /// reads inline; the prefetched map makes decode pure CPU). Touches NO
    /// epoch state — safe to run without the epoch guard. `pub(crate)` for
    /// the FUSE two-phase read.
    pub(crate) fn materialize_decode(&self, prepared: PreparedRead) -> Result<Vec<u8>, StoreError> {
        let PreparedRead {
            starts,
            descs,
            objects,
            descriptors,
            offset,
            end,
            avail,
        } = prepared;
        let limits = self.config.limits;
        // The decode context has NO epoch reference: the prepared maps
        // carry every object and every nested descriptor the closure
        // needs, so decode runs without the guard. Built per-branch: the
        // Phase-11E pool path moves the maps into Arc and the workers
        // build their own contexts.
        self.perf.time_request("read_decode", || {
            let mut out = vec![0u8; avail];
            if descs.is_empty() {
                // No extents in the window: a pure hole (zeros). Must not
                // reach the worker semaphore — an empty decode has nothing
                // to parallelize and would block for no work (the same
                // guard applies to the pool: no tasks, no submission).
                return Ok(out);
            }
            if descs.len() == 1 {
                let ctx = crate::store::epoch::PrefetchContext::new(
                    self,
                    &objects,
                    Some(&descriptors),
                    None,
                );
                for (i, desc) in descs.iter().enumerate() {
                    materialize_into_window(&ctx, desc, starts[i], offset, end, &limits, &mut out)?;
                }
                return Ok(out);
            }
            if self.worker_pool.load(std::sync::atomic::Ordering::Relaxed) {
                // Phase-11E PROBE: the persistent fair pool (decode path).
                // Each extent becomes a typed DecodeExtent task carrying the
                // prefetched maps; the caller reassembles strictly by
                // ordinal (the pool's determinism contract). Only the
                // scheduler changes — same descriptors, same maps, same
                // materializer.
                let store_arc = crate::store::workers::POOL
                    .store_arc()
                    .expect("11E pool bound to a store");
                let objects = std::sync::Arc::new(objects);
                let descriptors = std::sync::Arc::new(descriptors);
                let request_id = crate::store::workers::POOL.alloc_request_id();
                let tasks: Vec<crate::store::workers::WorkerTask> = descs
                    .iter()
                    .enumerate()
                    .map(
                        |(i, desc)| crate::store::workers::WorkerTask::DecodeExtent {
                            request_id,
                            ordinal: i,
                            store: std::sync::Arc::clone(&store_arc),
                            start: starts[i],
                            desc: desc.clone(),
                            objects: std::sync::Arc::clone(&objects),
                            descriptors: std::sync::Arc::clone(&descriptors),
                            limits,
                            offset,
                            end,
                            avail,
                        },
                    )
                    .collect();
                let t_scope = std::time::Instant::now();
                let submit = crate::store::workers::POOL.submit(request_id, tasks);
                let (joined, metrics) = submit.join();
                self.perf.record("worker_queue_wait", metrics.queue_wait_ns);
                self.perf
                    .record("worker_scope_wall", t_scope.elapsed().as_nanos() as u64);
                self.perf.record("worker_useful_cpu", metrics.cpu_ns);
                for wr in joined {
                    match wr.result {
                        Ok(crate::store::workers::WorkerOutcome::Decode((start, chunk))) => {
                            assemble_extent_window(&mut out, start, &chunk, offset, end, avail);
                        }
                        _ => unreachable!("decode request produced a non-decode result"),
                    }
                }
                return Ok(out);
            }
            // Phase-11C: the process-wide worker semaphore — concurrent
            // requests wait for the machine's workers instead of spawning
            // T×N threads (or burning CPU in a serial fallback).
            let ctx =
                crate::store::epoch::PrefetchContext::new(self, &objects, Some(&descriptors), None);
            let want = descs.len().min(
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4),
            );
            // Phase-11D oracle: the decode batch's queue wait (Gate A).
            let t_q = std::time::Instant::now();
            let grant = crate::store::workers::grant(want);
            self.perf
                .record("worker_queue_wait", t_q.elapsed().as_nanos() as u64);
            let workers = grant.n();
            if workers <= 1 {
                for (i, desc) in descs.iter().enumerate() {
                    materialize_into_window(&ctx, desc, starts[i], offset, end, &limits, &mut out)?;
                }
                return Ok(out);
            }
            let n = descs.len();
            let mut runs: Vec<Result<Vec<(u64, Vec<u8>)>, StoreError>> = Vec::new();
            // Phase-11D oracle: the decode scope wall (Gate B) and the
            // workers' true thread-CPU time (Gate C).
            let t_s = std::time::Instant::now();
            let useful = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let perf = &self.perf;
            std::thread::scope(|s| {
                let mut handles = Vec::with_capacity(workers);
                for w in 0..workers {
                    let lo = w * n / workers;
                    let hi = ((w + 1) * n / workers).max(lo + 1).min(n);
                    if lo >= hi {
                        continue;
                    }
                    let descs = &descs;
                    let starts = &starts;
                    let ctx = &ctx;
                    let useful = std::sync::Arc::clone(&useful);
                    let perf = perf;
                    handles.push(s.spawn(move || {
                        let t0 = crate::store::workers::WorkerClock::start();
                        let mut mine = Vec::with_capacity(hi - lo);
                        for i in lo..hi {
                            let desc = &descs[i];
                            let mut chunk = vec![0u8; desc.len() as usize];
                            let mut budget = limits.max_decode_work;
                            crate::core::materialize::materialize(
                                desc,
                                ctx,
                                &limits,
                                0,
                                &mut budget,
                                &mut chunk,
                            )
                            .map_err(|e| StoreError::Descriptor(e.to_string()))?;
                            mine.push((starts[i], chunk));
                            perf.record("worker_tasks", 0);
                        }
                        useful.fetch_add(t0.elapsed_ns(), std::sync::atomic::Ordering::Relaxed);
                        Ok(mine)
                    }));
                }
                for h in handles {
                    runs.push(match h.join() {
                        Ok(r) => r,
                        Err(_) => Err(StoreError::Invariant("read decode thread panicked".into())),
                    });
                }
            });
            self.perf
                .record("worker_scope_wall", t_s.elapsed().as_nanos() as u64);
            self.perf.record(
                "worker_useful_cpu",
                useful.load(std::sync::atomic::Ordering::Relaxed),
            );
            // Assemble (extent ranges are disjoint and ordered).
            for run in runs {
                for (start, chunk) in run? {
                    assemble_extent_window(&mut out, start, &chunk, offset, end, avail);
                }
            }
            Ok(out)
        })
    }

    /// Materialized chunk at an aligned offset (zeros for holes).
    pub fn read_chunk(
        &self,
        ino: u64,
        offset: u64,
        chunk_class: u64,
    ) -> Result<Vec<u8>, StoreError> {
        self.read_file(ino, offset, chunk_class)
    }

    /// Physical capacity of the backing store (statfs basis; capped by
    /// `capacity_override` when set — never above the real device, §22).
    pub fn physical_capacity(&self) -> u64 {
        use rustix::fs::statvfs;
        let physical = match statvfs(&self.dir) {
            Ok(s) => s.f_blocks.saturating_mul(s.f_frsize),
            Err(_) => 0,
        };
        match self.config.capacity_override {
            Some(o) => o.min(physical),
            None => physical,
        }
    }

    /// Physical bytes used (sum of segment file sizes).
    pub fn physical_used(&self) -> u64 {
        let mut total = 0u64;
        if let Ok(segments) = segment::list_segments(&self.dir) {
            for seq in segments {
                if let Ok(md) = std::fs::metadata(segment::segment_path(&self.dir, seq)) {
                    total += md.len();
                }
            }
        }
        total
    }

    /// Encode one logical chunk through the cheap foreground candidate
    /// pipeline (ZERO/FILL/SPARSE/PALETTE/PERIODIC/RANS/RAW; no bases, no
    /// dedup — those arrive with context from the write/optimizer layers).
    /// The winner is the cheapest valid candidate; RAW always exists.
    pub fn encode_chunk(
        chunk: &[u8],
        offset: u64,
        content_id: crate::core::extent::ChunkId,
        limits: &crate::core::limits::Limits,
        policy: &crate::core::cost::Policy,
    ) -> Result<ExtentUpdate, StoreError> {
        let ctx = crate::core::candidate::CandidateContext {
            limits,
            policy,
            content_id,
            bases: &[],
            dedup: None,
        };
        let mut cands = Vec::new();
        if let Some(z) = crate::core::candidate::zero_candidate(chunk, content_id, limits) {
            cands.push(z);
        }
        if let Some(f) = crate::core::candidate::fill_candidate(chunk, content_id) {
            cands.push(f);
        }
        for enc in [
            Box::new(crate::entropy::sparse::SparseEncoder)
                as Box<dyn crate::core::candidate::Encoder>,
            Box::new(crate::entropy::palette::PaletteEncoder),
            Box::new(crate::entropy::periodic::PeriodicEncoder),
            Box::new(crate::entropy::sparse64::SparseBlock64Encoder),
            Box::new(crate::rans::residual::RansEncoder),
            Box::new(crate::rans::sequence::SequenceEncoder),
        ] {
            cands.extend(enc.encode(chunk, &ctx));
        }
        if let Some(r) = crate::core::candidate::raw_candidate(chunk, content_id, limits) {
            cands.push(r);
        }
        let best = crate::core::candidate::pick_cheapest(&cands, policy)
            .ok_or_else(|| StoreError::Invariant("no candidate for chunk".into()))?;
        Ok(ExtentUpdate {
            offset,
            descriptor: best.representation.clone(),
            content_id,
            objects: best.objects.clone(),
        })
    }

    /// §32 gate for unguided updates: materialize the update's descriptor
    /// through a resolver that sees both the committed store and the
    /// update's own new objects, and require the result to hash to the
    /// update's content id. The guided write path validates inside the
    /// search; this closes the bypass for `encode_chunk` call sites
    /// (flatten-on-write, truncate re-encoding).
    fn validate_update(&self, u: &ExtentUpdate) -> Result<(), StoreError> {
        self.validate_update_pending(u, None)
    }

    /// §32 gate for unguided updates: materialize the update's descriptor
    /// through a resolver that sees the committed store, the update's own
    /// new objects, and (Phase-8C) the batch's pending descriptors and
    /// staged objects, and require the result to hash to the update's
    /// content id. The pending view is required for the canonical-reuse
    /// path: the reused descriptor's objects are staged in the same batch,
    /// not yet committed.
    fn validate_update_pending(
        &self,
        u: &ExtentUpdate,
        pending: Option<&crate::optimizer::search::PendingBatch>,
    ) -> Result<(), StoreError> {
        let resolver = crate::optimizer::search::CandidateResolver::new(
            self,
            u.objects
                .iter()
                .map(|o| (o.id, o.payload.clone()))
                .collect(),
            pending,
        );
        let bytes = crate::core::materialize::materialize_to_vec(
            &u.descriptor,
            &resolver,
            &self.config.limits,
        )
        .map_err(|e| StoreError::Descriptor(e.to_string()))?;
        if crate::core::extent::ChunkId::of(&bytes) != u.content_id {
            return Err(StoreError::Invariant(
                "update does not materialize to its content id".into(),
            ));
        }
        Ok(())
    }

    /// Commit a set of extent updates for a file region (the FUSE write
    /// path entry point after candidate selection).
    pub fn commit_file_extents(
        &self,
        ino: u64,
        updates: Vec<ExtentUpdate>,
        new_size: Option<u64>,
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<(), StoreError> {
        let mut tx = self.begin_tx()?;
        for u in updates {
            // Append new objects.
            for obj in u.objects {
                let tag = match obj.kind {
                    crate::core::candidate::ObjectKind::Data => RecordTag::Data,
                    crate::core::candidate::ObjectKind::Model => RecordTag::Model,
                };
                let ml = if tag == RecordTag::Data {
                    Some(u.descriptor.len())
                } else {
                    None
                };
                crate::store::transaction::put_object(&mut tx, tag, obj.payload, ml);
            }
            // Chunk index entry.
            Store::put_chunk_in_tx(&mut tx, &u.content_id, &u.descriptor)?;
            // Extent entry.
            Store::put_extent_in_tx(&mut tx, ino, u.offset, &u.descriptor)?;
        }
        if let Some(size) = new_size {
            let inode = Store::inode_for_tx(&tx, ino)?;
            let mut inode = inode;
            // A smaller write must not leave extents past the new EOF
            // (fsck invariant: extent end <= file size). Drop extents
            // starting at or beyond the new size; the write's own updates
            // already replaced any touched trailing chunk at its clipped
            // logical length.
            if let InodeData::File { extent_root } = &inode.data {
                if !extent_root.is_zero() {
                    let limits = tx.store.config.limits;
                    let all = crate::store::extent_tree::scan_all(
                        *extent_root,
                        BTREE_ORDER,
                        limits.max_fanout,
                        &tx,
                    )?;
                    let mut keep_root = *extent_root;
                    for (start, _) in all {
                        if start >= size {
                            let (nr, _) = crate::store::extent_tree::remove(
                                keep_root,
                                start,
                                BTREE_ORDER,
                                limits.max_fanout,
                                &mut tx,
                            )?;
                            keep_root = nr;
                        }
                    }
                    if keep_root != *extent_root {
                        inode.data = InodeData::File {
                            extent_root: keep_root,
                        };
                    }
                }
            }
            inode.size = size;
            inode.mtime = crate::store::inode::Timespec::now();
            Store::put_inode_in_tx(&mut tx, ino, &inode)?;
        }
        tx.commit(hooks)?;
        Ok(())
    }

    /// Commit a set of extent updates with deferred durability (the FUSE
    /// write path; durability is provided by `fsync` →
    /// [`Store::durability_barrier`]). Process-crash safe; power-durable
    /// only after the next barrier.
    pub fn commit_file_extents_deferred(
        &self,
        ino: u64,
        updates: Vec<ExtentUpdate>,
        new_size: Option<u64>,
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<(), StoreError> {
        let mut tx = self
            .perf
            .time_request("begin_tx_wait", || self.begin_tx())?;
        self.perf
            .time_request("btree_mutation", || -> Result<(), StoreError> {
                for u in &updates {
                    for obj in &u.objects {
                        let tag = match obj.kind {
                            crate::core::candidate::ObjectKind::Data => RecordTag::Data,
                            crate::core::candidate::ObjectKind::Model => RecordTag::Model,
                        };
                        let ml = if tag == RecordTag::Data {
                            Some(u.descriptor.len())
                        } else {
                            None
                        };
                        crate::store::transaction::put_object(
                            &mut tx,
                            tag,
                            obj.payload.clone(),
                            ml,
                        );
                    }
                    Store::put_chunk_in_tx(&mut tx, &u.content_id, &u.descriptor)?;
                    Store::put_extent_in_tx(&mut tx, ino, u.offset, &u.descriptor)?;
                }
                Ok(())
            })?;
        if let Some(size) = new_size {
            let inode = Store::inode_for_tx(&tx, ino)?;
            let mut inode = inode;
            if let InodeData::File { extent_root } = &inode.data {
                if !extent_root.is_zero() {
                    let limits = tx.store.config.limits;
                    let all = crate::store::extent_tree::scan_all(
                        *extent_root,
                        BTREE_ORDER,
                        limits.max_fanout,
                        &tx,
                    )?;
                    let mut keep_root = *extent_root;
                    for (start, _) in all {
                        if start >= size {
                            let (nr, _) = crate::store::extent_tree::remove(
                                keep_root,
                                start,
                                BTREE_ORDER,
                                limits.max_fanout,
                                &mut tx,
                            )?;
                            keep_root = nr;
                        }
                    }
                    if keep_root != *extent_root {
                        inode.data = InodeData::File {
                            extent_root: keep_root,
                        };
                    }
                }
            }
            inode.size = size;
            inode.mtime = crate::store::inode::Timespec::now();
            Store::put_inode_in_tx(&mut tx, ino, &inode)?;
        }
        let _ = tx.commit_deferred(hooks)?;
        Ok(())
    }

    /// Truncate a file: drop extents starting at or beyond the new size
    /// and re-encode the trailing partial extent so no extent extends past
    /// `new_size` (fsck invariant: extent end <= file size).
    /// Truncate a file: drop extents starting at or beyond the new size
    /// and re-encode the trailing partial extent so no extent extends past
    /// `new_size` (fsck invariant: extent end <= file size). Takes the
    /// per-inode mutation lock.
    pub fn truncate_file(&self, ino: u64, new_size: u64) -> Result<(), StoreError> {
        let _lock = self.inode_lock(ino);
        self.truncate_file_locked(ino, new_size)
    }

    /// The truncate body (the caller holds the per-inode mutation lock).
    pub(crate) fn truncate_file_locked(&self, ino: u64, new_size: u64) -> Result<(), StoreError> {
        let inode = self
            .get_inode(ino)?
            .ok_or_else(|| StoreError::Invariant(format!("inode {ino} missing")))?;
        if !inode.is_file() {
            return Err(StoreError::Invariant("not a regular file".into()));
        }
        let limits = self.config.limits;
        let extent_root = match &inode.data {
            InodeData::File { extent_root } => *extent_root,
            _ => unreachable!(),
        };
        // Pre-compute the trailing trim against the committed store (the
        // store itself is the DecoderContext; no transaction needed to read).
        let trim: Option<(u64, crate::core::extent::ChunkId, ExtentUpdate)> = if new_size == 0 {
            None
        } else {
            match crate::store::extent_tree::covering(
                extent_root,
                new_size - 1,
                BTREE_ORDER,
                limits.max_fanout,
                self,
            )? {
                Some((start, desc_bytes)) => {
                    let desc = crate::format::descriptor::decode(&desc_bytes, &limits)?;
                    let extent_end = start.saturating_add(desc.len());
                    if extent_end > new_size {
                        let mut chunk = vec![0u8; desc.len() as usize];
                        let mut budget = limits.max_decode_work;
                        materialize(&desc, self, &limits, 0, &mut budget, &mut chunk)
                            .map_err(|e| StoreError::Descriptor(e.to_string()))?;
                        let prefix_len = (new_size - start) as usize;
                        let prefix = &chunk[..prefix_len];
                        let cid = crate::core::extent::ChunkId::of(prefix);
                        let update =
                            Store::encode_chunk(prefix, start, cid, &limits, &self.config.policy)?;
                        Some((start, cid, update))
                    } else {
                        None
                    }
                }
                None => None,
            }
        };
        let mut tx = self.begin_tx()?;
        // Keep all extents starting below the new size; drop the rest.
        let mut keep = extent_root;
        if new_size > 0 {
            let all = crate::store::extent_tree::scan_all(
                extent_root,
                BTREE_ORDER,
                limits.max_fanout,
                &tx,
            )?;
            for (start, _) in all {
                if start >= new_size {
                    let (nr, _) = crate::store::extent_tree::remove(
                        keep,
                        start,
                        BTREE_ORDER,
                        limits.max_fanout,
                        &mut tx,
                    )?;
                    keep = nr;
                }
            }
        } else {
            keep = crate::core::extent::ChunkId::ZERO;
        }
        // Stage the trimmed trailing extent (if any).
        if let Some((start, cid, update)) = trim {
            // §32 gate: the re-encoded prefix must materialize to its
            // content id before it may be persisted.
            self.validate_update(&update)?;
            for obj in &update.objects {
                let tag = match obj.kind {
                    crate::core::candidate::ObjectKind::Data => RecordTag::Data,
                    crate::core::candidate::ObjectKind::Model => RecordTag::Model,
                };
                let ml = if tag == RecordTag::Data {
                    Some(update.descriptor.len())
                } else {
                    None
                };
                crate::store::transaction::put_object(&mut tx, tag, obj.payload.clone(), ml);
            }
            Store::put_chunk_in_tx(&mut tx, &cid, &update.descriptor)?;
            let bytes = crate::format::descriptor::encode(&update.descriptor)?;
            keep = crate::store::extent_tree::insert(
                keep,
                start,
                &bytes,
                BTREE_ORDER,
                limits.max_fanout,
                &mut tx,
            )?;
        }
        let mut inode = inode;
        inode.data = InodeData::File { extent_root: keep };
        inode.size = new_size;
        inode.mtime = crate::store::inode::Timespec::now();
        inode.ctime = inode.mtime;
        Store::put_inode_in_tx(&mut tx, ino, &inode)?;
        tx.commit(&crate::store::transaction::CrashHooks::none())?;
        Ok(())
    }

    /// Directory operations (thin wrappers over the dir tree).
    pub fn dir_lookup(&self, dir_ino: u64, name: &[u8]) -> Result<Option<DirEntry>, StoreError> {
        let inode = self
            .get_inode(dir_ino)?
            .ok_or_else(|| StoreError::Invariant(format!("inode {dir_ino} missing")))?;
        match inode.data {
            InodeData::Directory { dir_root } => Ok(crate::store::directory::lookup(
                dir_root,
                name,
                BTREE_ORDER,
                self.config.limits.max_fanout,
                self,
            )?),
            _ => Err(StoreError::Invariant("not a directory".into())),
        }
    }

    /// Insert a directory entry.
    pub fn dir_insert(
        &self,
        dir_ino: u64,
        name: &[u8],
        entry: DirEntry,
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<(), StoreError> {
        let fanout = self.config.limits.max_fanout;
        let mut tx = self.begin_tx()?;
        let inode = Store::inode_for_tx(&tx, dir_ino)?;
        let dir_root = match inode.data {
            InodeData::Directory { dir_root } => dir_root,
            _ => return Err(StoreError::Invariant("not a directory".into())),
        };
        let new_root =
            crate::store::directory::insert(dir_root, name, entry, BTREE_ORDER, fanout, &mut tx)?;
        let mut inode = inode;
        inode.data = InodeData::Directory { dir_root: new_root };
        inode.mtime = crate::store::inode::Timespec::now();
        Store::put_inode_in_tx(&mut tx, dir_ino, &inode)?;
        tx.commit(hooks)?;
        Ok(())
    }

    /// Remove a directory entry.
    pub fn dir_remove(
        &self,
        dir_ino: u64,
        name: &[u8],
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<(), StoreError> {
        let fanout = self.config.limits.max_fanout;
        let mut tx = self.begin_tx()?;
        let inode = Store::inode_for_tx(&tx, dir_ino)?;
        let dir_root = match inode.data {
            InodeData::Directory { dir_root } => dir_root,
            _ => return Err(StoreError::Invariant("not a directory".into())),
        };
        let (new_root, _) =
            crate::store::directory::remove(dir_root, name, BTREE_ORDER, fanout, &mut tx)?;
        let mut inode = inode;
        inode.data = InodeData::Directory { dir_root: new_root };
        inode.mtime = crate::store::inode::Timespec::now();
        Store::put_inode_in_tx(&mut tx, dir_ino, &inode)?;
        tx.commit(hooks)?;
        Ok(())
    }

    /// Scan a directory (readdir).
    pub fn dir_scan(
        &self,
        dir_ino: u64,
        start_after: Option<&[u8]>,
        limit: usize,
    ) -> Result<crate::store::directory::DirScan, StoreError> {
        let inode = self
            .get_inode(dir_ino)?
            .ok_or_else(|| StoreError::Invariant(format!("inode {dir_ino} missing")))?;
        match inode.data {
            InodeData::Directory { dir_root } => Ok(crate::store::directory::scan(
                dir_root,
                start_after,
                limit,
                BTREE_ORDER,
                self.config.limits.max_fanout,
                self,
            )?),
            _ => Err(StoreError::Invariant("not a directory".into())),
        }
    }

    // ------------------------------------------------------------------
    // Namespace transactions (used by the FUSE adapter and CLI)
    // ------------------------------------------------------------------

    /// Validate a directory entry name (raw bytes; never assumed UTF-8).
    pub fn validate_name(name: &[u8]) -> bool {
        !name.is_empty()
            && name != b"."
            && name != b".."
            && name.len() <= 255
            && !name.contains(&0u8)
            && !name.contains(&b'/')
    }

    /// Create a new entry (file/dir/symlink/device) under `parent` and
    /// return its inode number. One transaction for inode + entry.
    pub fn create_entry(
        &self,
        parent: u64,
        name: &[u8],
        entry: NewEntry,
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<u64, StoreError> {
        let kind = &entry.kind;
        let mode_perms = entry.mode;
        let uid = entry.uid;
        let gid = entry.gid;
        if !Self::validate_name(name) {
            return Err(StoreError::Config("invalid entry name".into()));
        }
        let fanout = self.config.limits.max_fanout;
        let ino = self.alloc_ino()?;
        let mut tx = self.begin_tx()?;
        // The parent must exist, be a directory, and not already contain
        // the name.
        let parent_inode = Store::inode_for_tx(&tx, parent)?;
        let dir_root = match parent_inode.data {
            InodeData::Directory { dir_root } => dir_root,
            _ => return Err(StoreError::Invariant("parent not a directory".into())),
        };
        if crate::store::directory::lookup(dir_root, name, BTREE_ORDER, fanout, &tx)?.is_some() {
            return Err(StoreError::Invariant("entry already exists".into()));
        }
        let inode = match kind {
            EntryKind::File => Inode::new_file(uid, gid, mode_perms),
            EntryKind::Directory => Inode::new_dir(uid, gid, mode_perms),
            EntryKind::Symlink(target) => Inode::new_symlink(target.clone(), uid, gid),
            EntryKind::Device(is_char, rdev) => {
                let mut i = Inode::new_file(uid, gid, mode_perms);
                i.data_kind = crate::store::inode::DATA_DEVICE;
                i.data = InodeData::Device;
                i.rdev = *rdev;
                i.mode = (if *is_char {
                    crate::store::inode::mode::S_IFCHR
                } else {
                    crate::store::inode::mode::S_IFBLK
                }) | (mode_perms & crate::store::inode::mode::S_IPERM);
                i
            }
        };
        Store::put_inode_in_tx(&mut tx, ino, &inode)?;
        let entry = DirEntry {
            ino,
            d_type: match &kind {
                EntryKind::File => directory::dt::DT_REG,
                EntryKind::Directory => directory::dt::DT_DIR,
                EntryKind::Symlink(_) => directory::dt::DT_LNK,
                EntryKind::Device(_, _) => directory::dt::DT_UNKNOWN,
            },
        };
        let new_dir_root =
            crate::store::directory::insert(dir_root, name, entry, BTREE_ORDER, fanout, &mut tx)?;
        let mut p = parent_inode;
        p.data = InodeData::Directory {
            dir_root: new_dir_root,
        };
        p.mtime = crate::store::inode::Timespec::now();
        if matches!(kind, EntryKind::Directory) {
            p.nlink = p.nlink.saturating_add(1);
        }
        Store::put_inode_in_tx(&mut tx, parent, &p)?;
        tx.commit(hooks)?;
        Ok(ino)
    }

    /// Remove an entry; drops the inode when its nlink reaches zero
    /// (GC reclaims the objects). `is_dir` selects rmdir semantics
    /// (directory must be empty). Returns the removed entry's inode
    /// number (needed by the FUSE layer for kernel cache invalidation).
    pub fn unlink(
        &self,
        parent: u64,
        name: &[u8],
        is_dir: bool,
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<u64, StoreError> {
        if !Self::validate_name(name) {
            return Err(StoreError::Config("invalid entry name".into()));
        }
        let fanout = self.config.limits.max_fanout;
        let mut tx = self.begin_tx()?;
        let parent_inode = Store::inode_for_tx(&tx, parent)?;
        let dir_root = match parent_inode.data {
            InodeData::Directory { dir_root } => dir_root,
            _ => return Err(StoreError::Invariant("parent not a directory".into())),
        };
        let entry = match crate::store::directory::lookup(dir_root, name, BTREE_ORDER, fanout, &tx)?
        {
            Some(e) => e,
            None => return Err(StoreError::Invariant("no such entry".into())),
        };
        let target = Store::inode_for_tx(&tx, entry.ino)?;
        if is_dir {
            if !target.is_dir() {
                return Err(StoreError::Invariant("not a directory".into()));
            }
            // A directory is empty when its tree has no entries.
            if let InodeData::Directory { dir_root: dr } = &target.data {
                if !dr.is_zero()
                    && !crate::store::directory::scan(*dr, None, 1, BTREE_ORDER, fanout, &tx)?
                        .0
                        .is_empty()
                {
                    return Err(StoreError::Invariant("directory not empty".into()));
                }
            }
        } else if target.is_dir() {
            return Err(StoreError::Invariant("is a directory".into()));
        }
        let new_dir_root =
            crate::store::directory::remove(dir_root, name, BTREE_ORDER, fanout, &mut tx)?.0;
        let mut p = parent_inode;
        p.data = InodeData::Directory {
            dir_root: new_dir_root,
        };
        p.mtime = crate::store::inode::Timespec::now();
        if target.is_dir() {
            p.nlink = p.nlink.saturating_sub(1);
        }
        Store::put_inode_in_tx(&mut tx, parent, &p)?;
        // Drop the inode when the last link goes away. An rmdir'd
        // directory dies outright: POSIX removes the directory entry and
        // the directory inode together (there is no nlink-1 state for a
        // removed directory).
        let mut target = target;
        if is_dir {
            Store::remove_inode_in_tx(&mut tx, entry.ino)?;
        } else {
            target.nlink = target.nlink.saturating_sub(1);
            if target.nlink == 0 {
                Store::remove_inode_in_tx(&mut tx, entry.ino)?;
            } else {
                Store::put_inode_in_tx(&mut tx, entry.ino, &target)?;
            }
        }
        tx.commit(hooks)?;
        Ok(entry.ino)
    }

    /// Rename `src_name` under `src_parent` to `dst_name` under
    /// `dst_parent` (v1: no RENAME_EXCHANGE / RENAME_NOREPLACE flags;
    /// an existing destination is replaced).
    ///
    /// Same-parent renames operate on a single tree root: the destination
    /// removal, destination insertion, and source removal are chained on
    /// one root so no entry is lost or duplicated. Cross-parent renames
    /// mutate both roots independently. POSIX type rules apply: a
    /// directory cannot replace a non-directory (and vice versa), and a
    /// directory can only replace an empty directory.
    pub fn rename(
        &self,
        src_parent: u64,
        src_name: &[u8],
        dst_parent: u64,
        dst_name: &[u8],
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<RenameOutcome, StoreError> {
        if !Self::validate_name(src_name) || !Self::validate_name(dst_name) {
            return Err(StoreError::Config("invalid entry name".into()));
        }
        // Renaming a name onto itself is a POSIX no-op.
        if src_parent == dst_parent && src_name == dst_name {
            return Ok(RenameOutcome {
                src_ino: self
                    .dir_lookup(src_parent, src_name)?
                    .ok_or_else(|| StoreError::Invariant("no such entry".into()))?
                    .ino,
                replaced_dst_ino: None,
            });
        }
        let fanout = self.config.limits.max_fanout;
        let mut tx = self.begin_tx()?;
        let sp = Store::inode_for_tx(&tx, src_parent)?;
        let src_root = match sp.data {
            InodeData::Directory { dir_root } => dir_root,
            _ => return Err(StoreError::Invariant("src parent not a directory".into())),
        };
        let entry =
            match crate::store::directory::lookup(src_root, src_name, BTREE_ORDER, fanout, &tx)? {
                Some(e) => e,
                None => return Err(StoreError::Invariant("no such entry".into())),
            };
        let src_inode = Store::inode_for_tx(&tx, entry.ino)?;
        let src_is_dir = src_inode.is_dir();
        let dp = Store::inode_for_tx(&tx, dst_parent)?;
        let dst_root = match dp.data {
            InodeData::Directory { dir_root } => dir_root,
            _ => return Err(StoreError::Invariant("dst parent not a directory".into())),
        };
        let mut replaced_dst_ino = None;
        if let Some(dst_entry) =
            crate::store::directory::lookup(dst_root, dst_name, BTREE_ORDER, fanout, &tx)?
        {
            if dst_entry.ino != entry.ino {
                // POSIX type rules.
                let dst_inode = Store::inode_for_tx(&tx, dst_entry.ino)?;
                let dst_is_dir = dst_inode.is_dir();
                if src_is_dir && !dst_is_dir {
                    return Err(StoreError::Invariant("cannot rename dir over file".into()));
                }
                if !src_is_dir && dst_is_dir {
                    return Err(StoreError::Invariant("cannot rename file over dir".into()));
                }
                if src_is_dir && dst_is_dir {
                    if let InodeData::Directory { dir_root: dr } = &dst_inode.data {
                        if !dr.is_zero()
                            && !crate::store::directory::scan(
                                *dr,
                                None,
                                1,
                                BTREE_ORDER,
                                fanout,
                                &tx,
                            )?
                            .0
                            .is_empty()
                        {
                            return Err(StoreError::Invariant("directory not empty".into()));
                        }
                    }
                }
                replaced_dst_ino = Some(dst_entry.ino);
                // Drop the destination's inode reference. A replaced
                // directory dies outright (directories cannot be hard
                // linked); a replaced file drops one link.
                if dst_is_dir {
                    Store::remove_inode_in_tx(&mut tx, dst_entry.ino)?;
                } else {
                    let mut target = dst_inode;
                    target.nlink = target.nlink.saturating_sub(1);
                    if target.nlink == 0 {
                        Store::remove_inode_in_tx(&mut tx, dst_entry.ino)?;
                    } else {
                        Store::put_inode_in_tx(&mut tx, dst_entry.ino, &target)?;
                    }
                }
            }
        }
        // Same-parent renames chain all tree mutations on one root;
        // cross-parent renames mutate both roots.
        let mut sp = sp;
        let mut dp = dp;
        if src_parent == dst_parent {
            let mut root = dst_root;
            if replaced_dst_ino.is_some() {
                root =
                    crate::store::directory::remove(root, dst_name, BTREE_ORDER, fanout, &mut tx)?
                        .0;
            }
            root = crate::store::directory::insert(
                root,
                dst_name,
                entry,
                BTREE_ORDER,
                fanout,
                &mut tx,
            )?;
            if src_name != dst_name {
                root =
                    crate::store::directory::remove(root, src_name, BTREE_ORDER, fanout, &mut tx)?
                        .0;
            }
            sp.data = InodeData::Directory { dir_root: root };
            sp.mtime = crate::store::inode::Timespec::now();
            Store::put_inode_in_tx(&mut tx, src_parent, &sp)?;
        } else {
            let mut dst_root = dst_root;
            if replaced_dst_ino.is_some() {
                dst_root = crate::store::directory::remove(
                    dst_root,
                    dst_name,
                    BTREE_ORDER,
                    fanout,
                    &mut tx,
                )?
                .0;
            }
            dst_root = crate::store::directory::insert(
                dst_root,
                dst_name,
                entry,
                BTREE_ORDER,
                fanout,
                &mut tx,
            )?;
            let src_root =
                crate::store::directory::remove(src_root, src_name, BTREE_ORDER, fanout, &mut tx)?
                    .0;
            sp.data = InodeData::Directory { dir_root: src_root };
            sp.mtime = crate::store::inode::Timespec::now();
            dp.data = InodeData::Directory { dir_root: dst_root };
            dp.mtime = crate::store::inode::Timespec::now();
            // Moving a directory changes both parents' subdirectory count.
            if src_is_dir {
                sp.nlink = sp.nlink.saturating_sub(1);
                dp.nlink = dp.nlink.saturating_add(1);
            }
            Store::put_inode_in_tx(&mut tx, src_parent, &sp)?;
            Store::put_inode_in_tx(&mut tx, dst_parent, &dp)?;
        }
        tx.commit(hooks)?;
        Ok(RenameOutcome {
            src_ino: entry.ino,
            replaced_dst_ino,
        })
    }

    /// Create a hard link: another directory entry for `ino` (nlink++).
    /// Flushes the active epoch first (the link target must be committed;
    /// the epoch's pending inodes are invisible to the transactional
    /// path).
    pub fn link(
        &self,
        parent: u64,
        name: &[u8],
        ino: u64,
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<(), StoreError> {
        self.ensure_epoch_flushed(hooks)?;
        if !Self::validate_name(name) {
            return Err(StoreError::Config("invalid entry name".into()));
        }
        let fanout = self.config.limits.max_fanout;
        let mut tx = self.begin_tx()?;
        let parent_inode = Store::inode_for_tx(&tx, parent)?;
        let dir_root = match parent_inode.data {
            InodeData::Directory { dir_root } => dir_root,
            _ => return Err(StoreError::Invariant("parent not a directory".into())),
        };
        if crate::store::directory::lookup(dir_root, name, BTREE_ORDER, fanout, &tx)?.is_some() {
            return Err(StoreError::Invariant("entry already exists".into()));
        }
        let target = Store::inode_for_tx(&tx, ino)?;
        if target.is_dir() {
            return Err(StoreError::Invariant("cannot hard link a directory".into()));
        }
        let entry = DirEntry {
            ino,
            d_type: match &target.data {
                InodeData::File { .. } => directory::dt::DT_REG,
                InodeData::Symlink { .. } => directory::dt::DT_LNK,
                _ => directory::dt::DT_UNKNOWN,
            },
        };
        let new_root =
            crate::store::directory::insert(dir_root, name, entry, BTREE_ORDER, fanout, &mut tx)?;
        let mut p = parent_inode;
        p.data = InodeData::Directory { dir_root: new_root };
        p.mtime = crate::store::inode::Timespec::now();
        Store::put_inode_in_tx(&mut tx, parent, &p)?;
        let mut target = target;
        target.nlink = target.nlink.saturating_add(1);
        Store::put_inode_in_tx(&mut tx, ino, &target)?;
        tx.commit(hooks)?;
        Ok(())
    }

    /// Replace an inode's mode/uid/gid/size/time fields (setattr). Returns
    /// the updated inode. Takes the per-inode mutation lock (a size change
    /// truncates, which must serialize with concurrent writes).
    pub fn setattr_inode(
        &self,
        ino: u64,
        update: &AttrUpdate,
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<Inode, StoreError> {
        let _lock = self.inode_lock(ino);
        self.setattr_inode_locked(ino, update, hooks)
    }

    /// The setattr body (the caller holds the per-inode mutation lock).
    fn setattr_inode_locked(
        &self,
        ino: u64,
        update: &AttrUpdate,
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<Inode, StoreError> {
        let mode = update.mode;
        let uid = update.uid;
        let gid = update.gid;
        let size = update.size;
        let atime = update.atime;
        let mtime = update.mtime;
        let fanout = self.config.limits.max_fanout;
        let mut tx = self.begin_tx()?;
        let inode = Store::inode_for_tx(&tx, ino)?;
        let mut inode = inode;
        if let Some(m) = mode {
            // Preserve the type bits; replace the permission bits.
            inode.mode = (inode.mode & crate::store::inode::mode::S_IFMT) | (m & 0o7777);
        }
        if let Some(u) = uid {
            inode.uid = u;
        }
        if let Some(g) = gid {
            inode.gid = g;
        }
        if let Some(s) = size {
            if s != inode.size {
                // Truncate or extend via the store truncate logic. The
                // truncate path re-encodes trailing partial extents.
                let _ = fanout;
                drop(tx);
                self.truncate_file_locked(ino, s)?;
                let mut tx = self.begin_tx()?;
                let mut inode = Store::inode_for_tx(&tx, ino)?;
                inode.size = s;
                if let Some(a) = atime {
                    inode.atime = a;
                }
                if let Some(m) = mtime {
                    inode.mtime = m;
                }
                inode.ctime = crate::store::inode::Timespec::now();
                Store::put_inode_in_tx(&mut tx, ino, &inode)?;
                tx.commit(hooks)?;
                return Ok(inode);
            }
        }
        if let Some(a) = atime {
            inode.atime = a;
        }
        if let Some(m) = mtime {
            inode.mtime = m;
        }
        inode.ctime = crate::store::inode::Timespec::now();
        Store::put_inode_in_tx(&mut tx, ino, &inode)?;
        tx.commit(hooks)?;
        Ok(inode)
    }
}

/// Entry kinds for `create_entry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Symbolic link with target bytes.
    Symlink(Vec<u8>),
    /// Device node (char, rdev).
    Device(bool, u32),
}

/// Parameters for creating a new entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEntry {
    /// Entry kind.
    pub kind: EntryKind,
    /// Permission bits (0o7777; type bits are implied by the kind).
    pub mode: u32,
    /// Owner uid.
    pub uid: u32,
    /// Owner gid.
    pub gid: u32,
}

/// Outcome of a rename, for kernel cache invalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenameOutcome {
    /// Inode that was moved.
    pub src_ino: u64,
    /// Inode of a replaced destination entry, if any.
    pub replaced_dst_ino: Option<u64>,
}

impl NewEntry {
    /// A regular file with the given permission bits.
    pub fn file(mode: u32, uid: u32, gid: u32) -> Self {
        Self {
            kind: EntryKind::File,
            mode,
            uid,
            gid,
        }
    }

    /// A directory with the given permission bits.
    pub fn dir(mode: u32, uid: u32, gid: u32) -> Self {
        Self {
            kind: EntryKind::Directory,
            mode,
            uid,
            gid,
        }
    }

    /// A symlink with the given target.
    pub fn symlink(target: Vec<u8>, uid: u32, gid: u32) -> Self {
        Self {
            kind: EntryKind::Symlink(target),
            mode: 0o777,
            uid,
            gid,
        }
    }

    /// A device node.
    pub fn device(is_char: bool, rdev: u32, mode: u32, uid: u32, gid: u32) -> Self {
        Self {
            kind: EntryKind::Device(is_char, rdev),
            mode,
            uid,
            gid,
        }
    }
}

/// Attribute updates for `setattr_inode` (all optional).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AttrUpdate {
    /// Replace permission bits (type bits preserved).
    pub mode: Option<u32>,
    /// Replace uid.
    pub uid: Option<u32>,
    /// Replace gid.
    pub gid: Option<u32>,
    /// Replace size (truncate or extend).
    pub size: Option<u64>,
    /// Replace atime.
    pub atime: Option<crate::store::inode::Timespec>,
    /// Replace mtime.
    pub mtime: Option<crate::store::inode::Timespec>,
}

/// One composed chunk ready for the concurrent search (Phase-10C).
///
/// Phase-11E: `Clone` — the persistent-pool probe moves an owned copy of
/// each chunk into its `EncodeChunk` task (the pool's workers outlive the
/// submit frame, so the task must own its inputs; the semaphore path keeps
/// borrowing from this struct). The per-chunk clone is ~64 KiB of bytes
/// plus the optional dictionary/synthetic state — caller-side cost, not
/// worker CPU. `pub(crate)`: the pool's `WorkerTask::EncodeChunk` carries
/// it (the task must own its inputs across the submit frame).
#[derive(Clone)]
pub(crate) struct Composed {
    chunk_off: u64,
    bytes: Vec<u8>,
    cid: crate::core::extent::ChunkId,
    prev_version: Option<crate::core::candidate::BaseChunk>,
    dictionary: Option<crate::core::candidate::BaseChunk>,
    /// Synthetic batch view resolving the in-batch dictionary chunk to
    /// its composed bytes: the parallel search validates SequenceDict
    /// candidates against this view without waiting for the previous
    /// chunk's encode. The REAL descriptor and chain depth are applied by
    /// the serial assembly phase. `None` when the dictionary is a
    /// committed chunk (the committed store resolves it).
    synthetic: Option<crate::optimizer::search::PendingBatch>,
}

/// One chunk's phase-2 outcome: the rebase-flatten updates, the validated
/// search outcome, and the prev_version actually used for the search
/// (post-flatten) so the serial depth/validation fallback can rebuild the
/// identical context.
type ChunkResult = Result<
    (
        Vec<ExtentUpdate>,
        crate::optimizer::search::SearchOutcome,
        Option<crate::core::candidate::BaseChunk>,
    ),
    StoreError,
>;

/// Encode one composed chunk (Phase-10C phase 2): the rebase-on-write
/// flatten plus the guided candidate search. Each chunk's context — prev
/// version from the RMW read, in-batch dictionary from the composed bytes
/// — is independent, so this runs concurrently for multi-chunk writes and
/// inline for the single-chunk FUSE request.
///
/// The in-batch dictionary is used with an ASSUMED depth 0: the exact
/// depth of a chained in-batch dictionary is only known after its own
/// encode, and resolving that serially would defeat the parallelism. The
/// search validates SequenceDict candidates against the chunk's synthetic
/// view; the serial assembly phase re-validates against the REAL batch
/// state and re-encodes (without the dictionary family) any outcome whose
/// real reference chain would exceed the decode cap — exactly what the
/// serial search did when it refused a too-deep dictionary. A candidate
/// whose real chain is admissible persists bytes identical to the serial
/// path: the encoder's streams depend only on input + dict bytes (both in
/// hand), never on the assumed depth.
fn encode_prepared_chunk(
    store: &Store,
    c: &Composed,
    ino: u64,
    limits: crate::core::limits::Limits,
    options: crate::optimizer::policy::OptimizeOptions,
    fg: crate::optimizer::foreground::ForegroundPolicy,
) -> ChunkResult {
    // Rebase-on-write (§11): drift workloads edit the same chunk
    // repeatedly, and each edit would otherwise nest another
    // BaseResidual until the depth cap collapses the strategy to RAW.
    // When the previous version is itself a deep chain, re-encode it at
    // depth 0 in the same transaction (the flat extent update lands
    // first; the edit's update replaces it).
    let mut flatten_updates: Vec<ExtentUpdate> = Vec::new();
    let mut prev_version = c.prev_version.clone();
    if let Some(p) = &prev_version {
        if p.depth >= crate::optimizer::rebase::REBASE_DEPTH_THRESHOLD {
            let policy = store.config.policy;
            let flat = Store::encode_chunk(&p.bytes, c.chunk_off, p.id, &limits, &policy)?;
            // §32 gate: the unguided cheap path bypasses the guided
            // search's validation; every persisted representation must
            // materialize to its content id.
            store.validate_update(&flat)?;
            flatten_updates.push(flat);
            prev_version = Some(crate::core::candidate::BaseChunk {
                id: p.id,
                bytes: p.bytes.clone(),
                depth: 0,
            });
        }
    }
    let ctx = crate::optimizer::search::GuidedContext {
        ino,
        offset: c.chunk_off,
        target: &c.bytes,
        prev_version: prev_version.clone(),
        dictionary: c.dictionary.clone(),
        // Phase-9C: the write path has no shared dictionary in hand; the
        // background shared-dict pass supplies it.
        shared: None,
        // The search validates against the chunk's own synthetic view
        // (the in-batch dictionary's composed bytes); the real batch
        // pending state is applied by the serial assembly phase.
        pending: c.synthetic.as_ref(),
        mode: crate::optimizer::search::SearchMode::Foreground,
    };
    let outcome = store.perf().time("search", || {
        crate::optimizer::search::encode_guided(store, &ctx, options, fg)
    })?;
    Ok((flatten_updates, outcome, prev_version))
}

impl Store {
    /// Write `data` at `offset` of file `ino` (chunk-aligned
    /// read-modify-write; one transaction; extends the file size). Takes
    /// the per-inode mutation lock for the whole prepare+commit sequence.
    pub fn write_region(&self, ino: u64, offset: u64, data: &[u8]) -> Result<(), StoreError> {
        self.write_region_with_fg(
            ino,
            offset,
            data,
            crate::optimizer::policy::OptimizeOptions::default(),
            self.foreground,
        )
    }

    /// Write with explicit optimization options (ablation benchmarks, §43).
    /// Takes the per-inode mutation lock. Ablation semantics: the full
    /// foreground policy (the policy gates CPU, not families — ablations
    /// measure the families).
    pub fn write_region_with(
        &self,
        ino: u64,
        offset: u64,
        data: &[u8],
        options: crate::optimizer::policy::OptimizeOptions,
    ) -> Result<(), StoreError> {
        self.write_region_with_fg(
            ino,
            offset,
            data,
            options,
            crate::optimizer::foreground::ForegroundPolicy::full(),
        )
    }

    /// Write with explicit options AND a foreground policy (Phase-10B).
    pub fn write_region_with_fg(
        &self,
        ino: u64,
        offset: u64,
        data: &[u8],
        options: crate::optimizer::policy::OptimizeOptions,
        fg: crate::optimizer::foreground::ForegroundPolicy,
    ) -> Result<(), StoreError> {
        let _lock = self.inode_lock(ino);
        self.write_region_with_locked_fg(ino, offset, data, options, fg)
    }

    /// The write body: the caller holds the per-inode mutation lock.
    /// Candidate encoding (hashing, rANS, dedup lookups, base search)
    /// runs concurrently with reads and with other inodes' prepares; only
    /// the final `commit_file_extents_deferred` serializes on the commit
    /// coordinator.
    pub(crate) fn write_region_with_locked(
        &self,
        ino: u64,
        offset: u64,
        data: &[u8],
        options: crate::optimizer::policy::OptimizeOptions,
    ) -> Result<(), StoreError> {
        self.write_region_with_locked_fg(
            ino,
            offset,
            data,
            options,
            crate::optimizer::foreground::ForegroundPolicy::full(),
        )
    }

    /// The write body with an explicit foreground policy.
    pub(crate) fn write_region_with_locked_fg(
        &self,
        ino: u64,
        offset: u64,
        data: &[u8],
        options: crate::optimizer::policy::OptimizeOptions,
        fg: crate::optimizer::foreground::ForegroundPolicy,
    ) -> Result<(), StoreError> {
        if data.is_empty() {
            return Ok(());
        }
        // Phase-11B: direct (non-epoch) write envelope; the commit
        // coordinator rows attach inside `commit_file_extents_deferred`
        // and `Tx::commit_deferred`.
        let _req = self.perf.request("write_region");
        let (updates, new_size) = self.perf.time_request("prepare", || {
            self.prepare_write(ino, offset, data, None, None, options, fg, None)
        })?;
        self.commit_file_extents_deferred(ino, updates, Some(new_size), &CrashHooks::none())?;
        Ok(())
    }

    /// Prepare a file write: chunk-aligned read-modify-write + candidate
    /// encoding into extent updates, WITHOUT committing. The caller holds
    /// the per-inode mutation lock and must commit the returned updates
    /// (possibly batched with other regions of the same inode).
    ///
    /// `overlay` (chunk offset → bytes) carries uncommitted in-batch chunk
    /// state so a later partial write in the same batch sees earlier batch
    /// writes instead of stale committed bytes. `epoch_size` (Phase-10D)
    /// overrides the committed inode size with the ACTIVE EPOCH's size:
    /// the epoch's writes/truncates are uncommitted, and clipping chunks
    /// to the committed size would corrupt a file the epoch has already
    /// grown. `None` for the transactional paths. Returns the updates plus
    /// the file size after this write.
    fn prepare_write(
        &self,
        ino: u64,
        offset: u64,
        data: &[u8],
        mut overlay: Option<&mut std::collections::BTreeMap<u64, Vec<u8>>>,
        mut pending: Option<&mut crate::optimizer::search::PendingBatch>,
        options: crate::optimizer::policy::OptimizeOptions,
        fg: crate::optimizer::foreground::ForegroundPolicy,
        epoch_size: Option<u64>,
    ) -> Result<(Vec<ExtentUpdate>, u64), StoreError> {
        if data.is_empty() {
            let committed = self.get_inode(ino)?.map(|i| i.size).unwrap_or(0);
            return Ok((Vec::new(), epoch_size.unwrap_or(committed)));
        }
        let limits = self.config.limits;
        let chunk_class = limits.chunk_class;
        let end = offset.saturating_add(data.len() as u64);
        let first_chunk = offset / chunk_class;
        let last_chunk = end.div_ceil(chunk_class);
        // The committed inode is only the size source for the
        // transactional paths; the epoch write passes its own (possibly
        // uncommitted) size and the caller already validated existence
        // against the overlay, so a committed miss is not an error there.
        let old_size = match epoch_size {
            Some(s) => s,
            None => {
                let inode = self
                    .get_inode(ino)?
                    .ok_or_else(|| StoreError::Invariant(format!("inode {ino} missing")))?;
                inode.size
            }
        };
        let new_size = old_size.max(end);

        // Phase-10C: parallel chunk preparation in three phases.
        //
        // 1. Compose every chunk's FINAL bytes serially (the batch overlay
        //    semantics are inherently ordered: a later write sees earlier
        //    writes to the same chunk). This phase is memory-bound and
        //    cheap.
        // 2. Encode all chunks CONCURRENTLY (the expensive candidate
        //    search; each chunk's context — prev version from the RMW
        //    read, in-batch dictionary from the composed bytes — is
        //    independent). The in-batch dictionary is used with an
        //    ASSUMED depth 0: the exact depth of a chained in-batch
        //    dictionary is only known after its own encode, and resolving
        //    that serially would defeat the parallelism. §32 byte-exact
        //    validation is the backstop for any depth-cap mismatch (a
        //    candidate whose real reference chain exceeds the decode cap
        //    fails materialization and loses; a valid candidate's
        //    persisted bytes are identical regardless of the assumed
        //    depth).
        // 3. Apply the batch semantics serially in offset order — the
        //    in-batch dedup canonicalization (a chunk whose content was
        //    already encoded earlier in the batch reuses the canonical
        //    descriptor or EXACT_REF alias, marginally cheapest) and the
        //    pending registration — then assemble the updates for ONE
        //    commit.
        let mut composed: Vec<Composed> = Vec::new();
        let mut chunk = first_chunk;
        while chunk < last_chunk {
            let chunk_off = chunk * chunk_class;
            let in_start = offset.max(chunk_off);
            let in_end = end.min(chunk_off + chunk_class);
            let write_start = (in_start - chunk_off) as usize;
            let write_end = (in_end - chunk_off) as usize;
            let payload = &data[(in_start - offset) as usize..(in_end - offset) as usize];

            // Read the current chunk bytes: the batch overlay (if this
            // chunk was already touched in this batch), else the committed
            // store (zeros for holes / beyond EOF) — unless the write
            // covers the entire chunk. The whole chunk is read (clipped to
            // the file size) so untouched bytes survive.
            let full_chunk = write_start == 0 && write_end == chunk_class as usize;
            let mut chunk_bytes = vec![0u8; chunk_class as usize];
            let mut partial: Vec<u8> = Vec::new();
            let mut from_overlay = false;
            if !full_chunk {
                let overlay_hit = overlay.as_ref().and_then(|o| o.get(&chunk_off).cloned());
                match overlay_hit {
                    Some(bytes) => {
                        let n = bytes.len().min(chunk_class as usize);
                        chunk_bytes[..n].copy_from_slice(&bytes[..n]);
                        partial = bytes;
                        from_overlay = true;
                    }
                    None => {
                        let read_end = (chunk_off + chunk_class).min(old_size);
                        if read_end > chunk_off {
                            // Phase-11B: the RMW read is PREPARATION work
                            // (inside the `prepare` partition row), so its
                            // read-leaf rows must not attach to the request
                            // as top-level reads (that would double-count
                            // them against `prepare`).
                            partial = self.perf.detach(|| {
                                self.perf.time("rmw_read", || {
                                    self.read_file(ino, chunk_off, read_end - chunk_off)
                                })
                            })?;
                            let n = partial.len().min(chunk_class as usize);
                            chunk_bytes[..n].copy_from_slice(&partial[..n]);
                        }
                    }
                }
            }
            chunk_bytes[write_start..write_end].copy_from_slice(payload);

            // A trailing partial chunk must be encoded at its logical
            // length, not padded to the full chunk class: extents must
            // never extend past the file size (fsck invariant, and the
            // SEEK_DATA/SEEK_HOLE contract).
            let chunk_end = chunk_off.saturating_add(chunk_class).min(new_size);
            let chunk_len = (chunk_end - chunk_off) as usize;
            let chunk_bytes = &chunk_bytes[..chunk_len];
            if let Some(o) = overlay.as_mut() {
                o.insert(chunk_off, chunk_bytes.to_vec());
            }

            let cid = self
                .perf
                .time("hash", || crate::core::extent::ChunkId::of(chunk_bytes));

            // P0: the previous version of this chunk (the natural edit
            // base for versioned data, H2); usable when the old extent
            // resolves in the chunk index. When the RMW already
            // materialized the full pre-write chunk, reuse those bytes
            // instead of re-reading the store (Phase 6 hot path). The
            // batch overlay bytes are *uncommitted*, so they are never a
            // base (the store cannot resolve them).
            // Phase-11B: the prev-version materialization re-reads the
            // store (`base_chunk_at` -> `read_file`); it is PREPARATION
            // work inside the `prepare` row, so its read-leaf rows must
            // not attach to the request.
            let prev_version = self.perf.detach(|| {
                let v: Option<crate::core::candidate::BaseChunk> =
                    if !from_overlay && old_size > chunk_off {
                        if !full_chunk && old_size >= chunk_off + chunk_len as u64 {
                            self.base_chunk_from_bytes(&partial[..chunk_len])?
                        } else if full_chunk {
                            self.base_chunk_at(ino, chunk_off, chunk_len)?
                        } else {
                            None // old extent shorter than the target chunk
                        }
                    } else {
                        None
                    };
                Ok::<Option<crate::core::candidate::BaseChunk>, StoreError>(v)
            })?;
            // Phase-9B: the SequenceDict dictionary is the previous
            // same-file chunk. Sequential writes make its bytes nearly
            // free: the batch overlay holds the uncommitted previous chunk
            // (its descriptor commits in this same transaction, so a
            // reference resolves at decode); otherwise the committed
            // store. Phase-10C: the in-batch dictionary uses the composed
            // bytes with an ASSUMED depth 0 (see the phase comment; §32
            // validates any depth-cap mismatch) — the overlay bytes always
            // match the bytes the reference materializes, whether the
            // previous chunk's descriptor is a fresh in-batch encode or a
            // canonical reuse of a committed chunk.
            let mut dictionary: Option<crate::core::candidate::BaseChunk> = None;
            let mut synthetic: Option<crate::optimizer::search::PendingBatch> = None;
            if chunk_off >= chunk_class {
                let prev_off = chunk_off - chunk_class;
                let overlay_hit = overlay.as_ref().and_then(|o| o.get(&prev_off).cloned());
                match overlay_hit {
                    Some(prev_bytes) => {
                        let pcid = crate::core::extent::ChunkId::of(&prev_bytes);
                        dictionary = Some(crate::core::candidate::BaseChunk {
                            id: pcid,
                            bytes: prev_bytes.clone(),
                            depth: 0, // assumed; phase 3 applies the real chain
                        });
                        // Synthetic view: pcid -> RAW descriptor over the
                        // composed bytes (a RAW object's id IS the payload
                        // hash, so object id == pcid). The synthetic
                        // descriptor is terminal (depth 0); phase 3 walks
                        // the REAL chain and re-encodes without the
                        // dictionary family if it would exceed the decode
                        // cap. A candidate whose real chain is admissible
                        // persists bytes identical to the serial path: the
                        // encoder's streams depend only on input + dict
                        // bytes (both in hand), never on the assumed depth.
                        let rep = crate::core::representation::Representation::Raw {
                            obj: pcid,
                            len: prev_bytes.len() as u64,
                        };
                        let desc_bytes = crate::format::descriptor::encode(&rep)?;
                        let mut syn = crate::optimizer::search::PendingBatch::default();
                        syn.descriptors.insert(pcid, desc_bytes);
                        syn.objects.insert(pcid, prev_bytes);
                        synthetic = Some(syn);
                    }
                    None => {
                        // Previous chunk not touched in this batch: the
                        // committed store is authoritative. The store read
                        // is preparation work (inside `prepare`), so it
                        // must not attach to the request (Phase-11B).
                        dictionary = self
                            .perf
                            .detach(|| self.base_chunk_at(ino, prev_off, chunk_len))?;
                    }
                }
            }
            composed.push(Composed {
                chunk_off,
                bytes: chunk_bytes.to_vec(),
                cid,
                prev_version,
                dictionary,
                synthetic,
            });
            chunk += 1;
        }

        // Phase 2: candidate search — CONCURRENTLY for multi-chunk
        // writes (scoped threads over the composed chunks), inline for the
        // single-chunk FUSE request (a scoped thread would cost ~50 µs of
        // latency for no parallelism). Deterministic: the outcomes are
        // gathered by index and phase 3 applies them in offset order.
        let n = composed.len();
        let mut results: Vec<Option<ChunkResult>> = (0..n).map(|_| None).collect();
        if n == 1 {
            results[0] = Some(encode_prepared_chunk(
                self,
                &composed[0],
                ino,
                limits,
                options,
                fg,
            ));
        } else if self.worker_pool.load(std::sync::atomic::Ordering::Relaxed) {
            // Phase-11E PROBE: the persistent fair worker pool (see
            // `workers.rs` — per-store opt-in; the FUSE daemon and every
            // non-probe store keep the semaphore path). The composed chunks
            // become one request of typed EncodeChunk tasks; the pool
            // serves requests round-robin at task granularity and the
            // results reassemble strictly by ordinal, so persisted semantic
            // order never depends on scheduling order (the pool's
            // determinism contract). Same DSFB, same policy, same corpus —
            // only the scheduler changes (the 11E attribution rule).
            let store_arc = crate::store::workers::POOL
                .store_arc()
                .expect("11E pool bound to a store");
            let request_id = crate::store::workers::POOL.alloc_request_id();
            let tasks: Vec<crate::store::workers::WorkerTask> = composed
                .iter()
                .enumerate()
                .map(
                    |(ordinal, c)| crate::store::workers::WorkerTask::EncodeChunk {
                        request_id,
                        ordinal,
                        store: std::sync::Arc::clone(&store_arc),
                        ino,
                        composed: c.clone(),
                        limits,
                        options,
                        fg,
                    },
                )
                .collect();
            let t_scope = std::time::Instant::now();
            let submit = crate::store::workers::POOL.submit(request_id, tasks);
            let (joined, metrics) = submit.join();
            // Phase-11D oracle rows, pool-path semantics: queue wait =
            // submit -> first service; scope wall = submit -> join; useful
            // CPU = the request's summed task thread-CPU.
            self.perf.record("worker_queue_wait", metrics.queue_wait_ns);
            self.perf
                .record("worker_scope_wall", t_scope.elapsed().as_nanos() as u64);
            self.perf.record("worker_useful_cpu", metrics.cpu_ns);
            for wr in joined {
                match wr.result {
                    Ok(crate::store::workers::WorkerOutcome::Encode(r)) => {
                        results[wr.ordinal] = Some(r);
                    }
                    _ => unreachable!("encode request produced a non-encode result"),
                }
            }
        } else {
            // Phase-11C: the process-wide worker SEMAPHORE. Concurrent
            // requests wait for the machine's workers (T requests × N
            // cores was the oversubscription 11B measured — the 16-thread
            // per-chunk search ran ~11× its single-request cost) instead
            // of spawning T×N threads; the search CPU stays bounded at
            // every thread count and the wall converges to the single-
            // request floor. A non-blocking serial fallback was measured
            // and rejected (the unlucky requests' inline searches thrashed
            // the workers' cores). The grant parks this thread; it holds
            // no other store lock here (11B releases the epoch guard
            // before prepare), so no lock-order cycle can form.
            let want = n.min(
                std::thread::available_parallelism()
                    .map(|p| p.get())
                    .unwrap_or(4),
            );
            // Phase-11D oracle: the grant acquisition IS the semaphore
            // queue wait (Gate A) — measured as its own phase so `prepare`
            // decomposes into useful CPU + queue wait + spawn/join.
            let t_q = std::time::Instant::now();
            let grant = crate::store::workers::grant(want);
            self.perf
                .record("worker_queue_wait", t_q.elapsed().as_nanos() as u64);
            let workers = grant.n();
            if workers <= 1 {
                for (j, slot) in results.iter_mut().enumerate() {
                    *slot = Some(encode_prepared_chunk(
                        self,
                        &composed[j],
                        ino,
                        limits,
                        options,
                        fg,
                    ));
                }
            } else {
                let per = n.div_ceil(workers);
                // Phase-11D oracle: the scope wall (Gate B: spawn/join) and
                // the workers' TRUE thread-CPU time (Gate C: useful work).
                let t_s = std::time::Instant::now();
                let useful = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                std::thread::scope(|s| {
                    let mut handles = Vec::with_capacity(workers);
                    for (w, slice) in results.chunks_mut(per).enumerate() {
                        let store = &*self;
                        let composed = &composed[..];
                        let useful = std::sync::Arc::clone(&useful);
                        handles.push(s.spawn(move || {
                            // The oracle's worker clock: true thread-CPU
                            // time — the useful-search-CPU contribution.
                            let t0 = crate::store::workers::WorkerClock::start();
                            for (j, slot) in slice.iter_mut().enumerate() {
                                let c = &composed[w * per + j];
                                let r = encode_prepared_chunk(store, c, ino, limits, options, fg);
                                *slot = Some(r);
                                store.perf().record("worker_tasks", 0);
                            }
                            useful.fetch_add(t0.elapsed_ns(), std::sync::atomic::Ordering::Relaxed);
                        }));
                    }
                    for h in handles {
                        let _ = h.join();
                    }
                });
                self.perf
                    .record("worker_scope_wall", t_s.elapsed().as_nanos() as u64);
                self.perf.record(
                    "worker_useful_cpu",
                    useful.load(std::sync::atomic::Ordering::Relaxed),
                );
            }
        }

        // Phase 3: batch semantics in offset order — the in-batch dedup
        // canonicalization, the real chain-depth enforcement, and the
        // pending registration — then the update assembly, exactly as the
        // serial path produced them.
        //
        // `depths` mirrors `pending.depths`: the REAL reference depth of
        // each in-batch descriptor, resolved as the batch proceeds so the
        // depth fallback and later dictionary references see the true
        // chain.
        let mut depths: std::collections::HashMap<crate::core::extent::ChunkId, u8> =
            std::collections::HashMap::new();
        let mut updates = Vec::new();
        for (i, c) in composed.iter().enumerate() {
            // Phase-8C batch canonicalization: if this exact content was
            // already encoded earlier in the batch, reuse the canonical
            // descriptor (or alias via EXACT_REF) instead of the fresh
            // encode — encode each unique final content once (§12, the
            // marginally cheapest exact representation wins). The
            // canonical was validated when its first occurrence won §32;
            // the reuse is re-validated here against the batch pending
            // state (the canonical's objects are staged, not committed).
            let canonical: Option<Vec<u8>> = pending
                .as_ref()
                .and_then(|p| p.descriptors.get(&c.cid))
                .cloned();
            if let Some(canon_bytes) = canonical {
                let canon = crate::format::descriptor::decode(&canon_bytes, &limits)?;
                let reuse_cost = canon_bytes.len() as u64;
                let alias = if options.allow_exact_ref {
                    crate::core::candidate::exact_ref_candidate(
                        c.cid,
                        c.cid,
                        c.bytes.len() as u64,
                        c.bytes.len() as u64,
                        &limits,
                    )
                } else {
                    None
                };
                let alias_cost = alias
                    .as_ref()
                    .map(|a| a.representation.encoded_size())
                    .unwrap_or(u64::MAX);
                let update = ExtentUpdate {
                    offset: c.chunk_off,
                    descriptor: if alias_cost < reuse_cost {
                        alias.expect("alias present").representation
                    } else {
                        canon
                    },
                    content_id: c.cid,
                    objects: Vec::new(),
                };
                // §32 gate for the reuse path (pending-aware resolver).
                self.validate_update_pending(&update, pending.as_deref())?;
                updates.push(update);
                continue;
            }
            let (flatten_updates, outcome, prev_version) =
                results[i].take().expect("phase 2 produced a result")?;
            let mut outcome = outcome;
            // Phase-10C backstop: the parallel search validated its winner
            // against the chunk's own SYNTHETIC view (the in-batch
            // dictionary's composed bytes, assumed depth 0) rather than the
            // real batch state. Anything the synthetic view can get wrong
            // is caught here, against the REAL pending state:
            //
            // - a dedup reuse whose object exists only in the synthetic
            //   view (consecutive identical content: the synthetic RAW
            //   descriptor ties the EXACT_REF alias on marginal bytes and
            //   would otherwise win while referencing an object that is
            //   never persisted);
            // - a dictionary chain whose REAL depth exceeds the decode cap
            //   (materialization walks the real chain and fails);
            // - any other resolution the synthetic view shadowed.
            //
            // On failure the chunk is re-encoded with the REAL pending
            // state and the REAL dictionary depth — exactly the serial
            // search's input, so the outcome is byte-identical to it (the
            // encoder's streams depend only on input + dict bytes; the
            // depth gates only admissibility).
            let mut real_depth = crate::optimizer::rebase::chain_depth_uncapped(
                self,
                &outcome.update.descriptor,
                &depths,
            );
            if (c.synthetic.is_some() || real_depth > 0)
                && self
                    .validate_update_pending(&outcome.update, pending.as_deref())
                    .is_err()
            {
                let dictionary = match &c.dictionary {
                    Some(d) if c.synthetic.is_some() => Some(crate::core::candidate::BaseChunk {
                        id: d.id,
                        bytes: d.bytes.clone(),
                        depth: depths.get(&d.id).copied().unwrap_or(0),
                    }),
                    other => other.clone(),
                };
                let ctx = crate::optimizer::search::GuidedContext {
                    ino,
                    offset: c.chunk_off,
                    target: &c.bytes,
                    prev_version,
                    dictionary,
                    shared: None,
                    pending: pending.as_deref(),
                    mode: crate::optimizer::search::SearchMode::Foreground,
                };
                let redo = self.perf.time("search", || {
                    crate::optimizer::search::encode_guided(self, &ctx, options, fg)
                })?;
                // The re-encode validated internally against the real
                // pending; confirm here so no fallback path can commit an
                // unvalidated update.
                self.validate_update_pending(&redo.update, pending.as_deref())
                    .map_err(|e| {
                        StoreError::Invariant(format!("fallback re-encode failed validation: {e}"))
                    })?;
                outcome = redo;
                real_depth = crate::optimizer::rebase::chain_depth_uncapped(
                    self,
                    &outcome.update.descriptor,
                    &depths,
                );
                if real_depth > limits.max_reference_depth {
                    return Err(StoreError::Invariant(format!(
                        "fallback re-encode still exceeds the decode cap ({} > {})",
                        real_depth, limits.max_reference_depth
                    )));
                }
            }
            // Phase-8C: register this chunk's descriptor + objects in the
            // batch pending state so later chunks in the same transaction
            // can dedup against it. First occurrence wins (the persisted
            // chunk-index entry is exactly the first occurrence's
            // descriptor); EXACT_REF descriptors are skipped — an alias
            // resolves through the committed index, and a self-
            // referencing pending entry would loop at validation.
            if let Some(p) = pending.as_mut() {
                use crate::core::representation::Representation as Rep;
                if !matches!(outcome.update.descriptor, Rep::ExactRef { .. }) {
                    let desc_bytes = crate::format::descriptor::encode(&outcome.update.descriptor)?;
                    p.descriptors
                        .entry(outcome.update.content_id)
                        .or_insert(desc_bytes);
                    // Phase-9B: register the descriptor's REAL reference
                    // depth so a later chunk in this batch can use it as a
                    // SequenceDict dictionary without exceeding the decode
                    // cap (first occurrence wins, like the descriptor).
                    p.depths
                        .entry(outcome.update.content_id)
                        .or_insert(real_depth);
                    depths
                        .entry(outcome.update.content_id)
                        .or_insert(real_depth);
                }
                for obj in &outcome.update.objects {
                    p.objects.entry(obj.id).or_insert(obj.payload.clone());
                }
            }
            updates.extend(flatten_updates);
            updates.push(outcome.update);
        }
        Ok((updates, new_size))
    }

    /// Group commit: write many (offset, data) regions of one file in a
    /// single transaction (§16, Phase-8 write aggregation). Regions are
    /// applied in offset order with an in-batch overlay, so overlapping or
    /// adjacent partial chunks compose correctly. Takes the per-inode
    /// mutation lock.
    pub fn write_region_batch(
        &self,
        ino: u64,
        writes: &[(u64, Vec<u8>)],
        options: crate::optimizer::policy::OptimizeOptions,
    ) -> Result<(), StoreError> {
        if writes.is_empty() {
            return Ok(());
        }
        let _lock = self.inode_lock(ino);
        let mut sorted: Vec<(u64, Vec<u8>)> = writes.to_vec();
        sorted.sort_by_key(|(off, _)| *off);
        let mut overlay: std::collections::BTreeMap<u64, Vec<u8>> =
            std::collections::BTreeMap::new();
        let mut pending: crate::optimizer::search::PendingBatch =
            crate::optimizer::search::PendingBatch::default();
        let mut updates: Vec<ExtentUpdate> = Vec::new();
        let mut new_size = 0u64;
        let fg = crate::optimizer::foreground::ForegroundPolicy::full();
        for (offset, data) in &sorted {
            let (u, sz) = self.prepare_write(
                ino,
                *offset,
                data,
                Some(&mut overlay),
                Some(&mut pending),
                options,
                fg,
                None,
            )?;
            updates.extend(u);
            new_size = new_size.max(sz);
        }
        self.commit_file_extents_deferred(ino, updates, Some(new_size), &CrashHooks::none())?;
        Ok(())
    }

    /// Punch a hole: the byte range reads as ZERO (and is stored as ZERO
    /// descriptors, so space is freed). When the punch reaches EOF and
    /// `keep_size` is clear, the file is truncated instead. Takes the
    /// per-inode mutation lock.
    pub fn punch_hole(
        &self,
        ino: u64,
        start: u64,
        end: u64,
        keep_size: bool,
    ) -> Result<(), StoreError> {
        // Flush the epoch: the hole punch operates on the committed
        // extents, which the epoch's pending writes would shadow.
        self.ensure_epoch_flushed(&crate::store::transaction::CrashHooks::none())?;
        let _lock = self.inode_lock(ino);
        self.punch_hole_locked(ino, start, end, keep_size)
    }

    /// The punch body (the caller holds the per-inode lock).
    fn punch_hole_locked(
        &self,
        ino: u64,
        start: u64,
        end: u64,
        keep_size: bool,
    ) -> Result<(), StoreError> {
        let inode = self
            .get_inode(ino)?
            .ok_or_else(|| StoreError::Invariant(format!("inode {ino} missing")))?;
        let size = inode.size;
        if !keep_size && end >= size {
            return self.truncate_file_locked(ino, start);
        }
        let punch_end = end.min(size);
        if punch_end > start {
            let zeros = vec![0u8; (punch_end - start) as usize];
            self.write_region_with_locked(
                ino,
                start,
                &zeros,
                crate::optimizer::policy::OptimizeOptions::default(),
            )?;
        }
        Ok(())
    }

    /// `copy_file_range`: copy `len` bytes between files (v1 reads through
    /// the materialization path and writes through the RMW path — correct,
    /// not zero-copy). Returns the number of bytes copied. Serializes with
    /// other writers of the destination inode.
    pub fn copy_range(
        &self,
        ino_in: u64,
        offset_in: u64,
        ino_out: u64,
        offset_out: u64,
        len: u64,
    ) -> Result<u64, StoreError> {
        // Flush the epoch: both sides must be committed for the
        // transactional copy.
        self.ensure_epoch_flushed(&crate::store::transaction::CrashHooks::none())?;
        let data = self.read_file(ino_in, offset_in, len)?;
        let copied = data.len() as u64;
        if copied > 0 {
            let _lock = self.inode_lock(ino_out);
            self.write_region_with_locked(
                ino_out,
                offset_out,
                &data,
                crate::optimizer::policy::OptimizeOptions::default(),
            )?;
        }
        Ok(copied)
    }

    /// Sum of materialized logical bytes across all file extents.
    pub fn logical_bytes(&self) -> Result<u64, StoreError> {
        let limits = self.config.limits;
        let mut total = 0u64;
        for ino in self.all_inodes()? {
            let inode = match self.get_inode(ino)? {
                Some(i) => i,
                None => continue,
            };
            if let InodeData::File { extent_root } = &inode.data {
                if extent_root.is_zero() {
                    continue;
                }
                let entries = crate::store::extent_tree::scan_all(
                    *extent_root,
                    BTREE_ORDER,
                    limits.max_fanout,
                    self,
                )?;
                for (_, bytes) in entries {
                    if let Ok(d) = crate::format::descriptor::decode(&bytes, &limits) {
                        total = total.saturating_add(d.len());
                    }
                }
            }
        }
        Ok(total)
    }

    /// Find the directory containing `ino` (for readdir `..`).
    ///
    /// v1: a reverse scan over directory entry lists (correct, but O(dirs)
    /// per call — the FUSE layer caches parents per inode to keep readdir
    /// cheap; a parent pointer is a future format refinement). The root
    /// directory is its own parent.
    pub fn parent_of(&self, ino: u64) -> Result<u64, StoreError> {
        let root_dir = self.current_root().root_dir_ino;
        if ino == root_dir {
            return Ok(root_dir);
        }
        let fanout = self.config.limits.max_fanout;
        for dir_ino in self.all_inodes()? {
            let inode = match self.get_inode(dir_ino)? {
                Some(i) => i,
                None => continue,
            };
            let dir_root = match inode.data {
                InodeData::Directory { dir_root } => dir_root,
                _ => continue,
            };
            if dir_root.is_zero() {
                continue;
            }
            let entries = index::scan_all(dir_root, BTREE_ORDER, fanout, self)?;
            for (_, v) in entries {
                if let Ok(e) = directory::DirEntry::decode(&v) {
                    if e.ino == ino {
                        return Ok(dir_ino);
                    }
                }
            }
        }
        Ok(root_dir)
    }

    /// Overlay-aware parent lookup (Phase-10D): the epoch's pending
    /// entries first (an epoch-created inode's parent is only visible
    /// there), then the committed scan.
    pub fn parent_of_epoch(
        &self,
        ep: &crate::store::epoch::Epoch,
        ino: u64,
    ) -> Result<u64, StoreError> {
        let root_dir = self.current_root().root_dir_ino;
        if ino == root_dir {
            return Ok(root_dir);
        }
        for ((parent, name), e) in ep.pending_entries.iter() {
            let _ = name;
            if e.ino == ino {
                return Ok(*parent);
            }
        }
        self.parent_of(ino)
    }

    /// Allocate a fresh inode number (monotonic; simple for v1 — the max
    /// ino + 1, found by scanning; the fuse layer caches the counter).
    pub fn alloc_ino(&self) -> Result<u64, StoreError> {
        let inodes = self.all_inodes()?;
        Ok(inodes.iter().copied().max().unwrap_or(1) + 1)
    }

    /// Resolve an absolute POSIX path (raw bytes) to an inode number.
    /// The path may use `/` separators, `.` and `..` components. Returns
    /// `None` for a missing component. v1: no symlink following in the
    /// middle of the path (a final symlink is returned as-is).
    pub fn resolve_path(&self, path: &[u8]) -> Result<Option<u64>, StoreError> {
        let mut ino = self.current_root().root_dir_ino;
        let mut components = Vec::new();
        for part in path.split(|&b| b == b'/') {
            if part.is_empty() || part == b"." {
                continue;
            }
            components.push(part);
        }
        for comp in components {
            if comp == b".." {
                // Track parents: walk from the root tracking the parent of
                // each directory (v1: directories store no parent pointer,
                // so resolve by scanning the root dir and each dir's
                // parent chain is unavailable — support `..` only at the
                // top level by returning an error otherwise).
                let _ = comp;
                return Err(StoreError::Invariant(
                    "'..' resolution not supported in v1 resolve_path".into(),
                ));
            }
            match self.dir_lookup(ino, comp)? {
                Some(entry) => ino = entry.ino,
                None => return Ok(None),
            }
        }
        Ok(Some(ino))
    }

    // ------------------------------------------------------------------
    // Snapshots
    // ------------------------------------------------------------------

    /// Create a snapshot of the current root under `name`.
    pub fn create_snapshot(
        &self,
        name: &[u8],
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<crate::store::snapshot::SnapshotEntry, StoreError> {
        if name.is_empty() || name.len() > 255 || name.contains(&b'/') || name.contains(&0u8) {
            return Err(StoreError::Config(format!(
                "invalid snapshot name {:?}",
                String::from_utf8_lossy(name)
            )));
        }
        let fanout = self.config.limits.max_fanout;
        let mut tx = self.begin_tx()?;
        let root = tx.root().clone();
        let root_id = root.id();
        let entry = crate::store::snapshot::SnapshotEntry {
            root_id,
            created_unix_ns: crate::store::inode::Timespec::now().sec * 1_000_000_000
                + crate::store::inode::Timespec::now().nsec as u64,
        };
        tx.root_mut().snapshot_tree_root = crate::store::snapshot::insert(
            tx.root_mut().snapshot_tree_root,
            name,
            entry,
            BTREE_ORDER,
            fanout,
            &mut tx,
        )?;
        tx.commit(hooks)?;
        Ok(entry)
    }

    /// List snapshots in name order.
    pub fn list_snapshots(
        &self,
    ) -> Result<Vec<(Vec<u8>, crate::store::snapshot::SnapshotEntry)>, StoreError> {
        Ok(crate::store::snapshot::list(
            self.current_root().snapshot_tree_root,
            BTREE_ORDER,
            self.config.limits.max_fanout,
            self,
        )?)
    }

    /// Look up a snapshot by name.
    pub fn snapshot_lookup(
        &self,
        name: &[u8],
    ) -> Result<Option<crate::store::snapshot::SnapshotEntry>, StoreError> {
        Ok(crate::store::snapshot::lookup(
            self.current_root().snapshot_tree_root,
            name,
            BTREE_ORDER,
            self.config.limits.max_fanout,
            self,
        )?)
    }

    /// Delete a snapshot by name. Returns whether it existed.
    pub fn delete_snapshot(
        &self,
        name: &[u8],
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<bool, StoreError> {
        let fanout = self.config.limits.max_fanout;
        let mut tx = self.begin_tx()?;
        let (new_root, present) = crate::store::snapshot::remove(
            tx.root_mut().snapshot_tree_root,
            name,
            BTREE_ORDER,
            fanout,
            &mut tx,
        )?;
        tx.root_mut().snapshot_tree_root = new_root;
        tx.commit(hooks)?;
        Ok(present)
    }

    /// Restore (roll back to) a snapshot's root. The generation is bumped
    /// so the superblock flip stays monotonic, and the restored-from
    /// snapshot entry is re-inserted so the snapshot itself survives the
    /// rollback (ZFS/btrfs-style semantics, §17).
    pub fn restore_snapshot(
        &self,
        name: &[u8],
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<(), StoreError> {
        let entry = self
            .snapshot_lookup(name)?
            .ok_or_else(|| StoreError::Invariant("no such snapshot".into()))?;
        let snap_bytes = self
            .fetch_object(&entry.root_id)?
            .ok_or_else(|| StoreError::Invariant("snapshot root object missing".into()))?;
        let snap_root = crate::store::root::Root::decode(&snap_bytes)
            .map_err(|e| StoreError::Superblock(format!("snapshot root decode: {e:?}")))?;
        let fanout = self.config.limits.max_fanout;
        let mut tx = self.begin_tx()?;
        *tx.root_mut() = snap_root;
        // Keep the restored-from snapshot (and older snapshots already in
        // its tree) alive after the rollback.
        tx.root_mut().snapshot_tree_root = crate::store::snapshot::insert(
            tx.root_mut().snapshot_tree_root,
            name,
            entry,
            BTREE_ORDER,
            fanout,
            &mut tx,
        )?;
        tx.commit(hooks)?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // xattrs (per-inode B-tree at `inode.xattr_root`)
    // ------------------------------------------------------------------

    /// Maximum xattr name length (linux XATTR_NAME_MAX).
    pub const XATTR_NAME_MAX: usize = 255;
    /// Maximum xattr value size (linux XATTR_SIZE_MAX).
    pub const XATTR_SIZE_MAX: u64 = 64 * 1024;

    /// Validate an xattr name (raw bytes; no NUL, no '/').
    pub fn validate_xattr_name(name: &[u8]) -> bool {
        !name.is_empty()
            && name.len() <= Self::XATTR_NAME_MAX
            && !name.contains(&0u8)
            && !name.contains(&b'/')
    }

    /// Get an xattr value (raw bytes; `None` when absent). Flushes the
    /// active epoch first (xattrs live in committed inode trees).
    pub fn get_xattr(&self, ino: u64, name: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.ensure_epoch_flushed(&crate::store::transaction::CrashHooks::none())?;
        let inode = self
            .get_inode(ino)?
            .ok_or_else(|| StoreError::Invariant(format!("inode {ino} missing")))?;
        if inode.xattr_root.is_zero() {
            return Ok(None);
        }
        Ok(index::get(
            inode.xattr_root,
            name,
            BTREE_ORDER,
            self.config.limits.max_fanout,
            self,
        )?)
    }

    /// Set an xattr (insert or replace). Flushes the active epoch first.
    pub fn set_xattr(
        &self,
        ino: u64,
        name: &[u8],
        value: &[u8],
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<(), StoreError> {
        self.ensure_epoch_flushed(hooks)?;
        if !Self::validate_xattr_name(name) {
            return Err(StoreError::Config("invalid xattr name".into()));
        }
        if value.len() as u64 > Self::XATTR_SIZE_MAX {
            return Err(StoreError::Limit(format!(
                "xattr value {} exceeds {}",
                value.len(),
                Self::XATTR_SIZE_MAX
            )));
        }
        let fanout = self.config.limits.max_fanout;
        let mut tx = self.begin_tx()?;
        let inode = Store::inode_for_tx(&tx, ino)?;
        let new_root = index::insert(inode.xattr_root, name, value, BTREE_ORDER, fanout, &mut tx)?;
        let mut inode = inode;
        inode.xattr_root = new_root;
        inode.ctime = crate::store::inode::Timespec::now();
        Store::put_inode_in_tx(&mut tx, ino, &inode)?;
        tx.commit(hooks)?;
        Ok(())
    }

    /// Remove an xattr; returns whether it existed. Flushes the active
    /// epoch first.
    pub fn remove_xattr(
        &self,
        ino: u64,
        name: &[u8],
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<bool, StoreError> {
        self.ensure_epoch_flushed(hooks)?;
        let fanout = self.config.limits.max_fanout;
        let mut tx = self.begin_tx()?;
        let inode = Store::inode_for_tx(&tx, ino)?;
        if inode.xattr_root.is_zero() {
            return Ok(false);
        }
        let present = index::get(inode.xattr_root, name, BTREE_ORDER, fanout, &tx)?.is_some();
        if !present {
            return Ok(false);
        }
        let new_root = index::remove(inode.xattr_root, name, BTREE_ORDER, fanout, &mut tx)?;
        let mut inode = inode;
        inode.xattr_root = new_root;
        inode.ctime = crate::store::inode::Timespec::now();
        Store::put_inode_in_tx(&mut tx, ino, &inode)?;
        tx.commit(hooks)?;
        Ok(true)
    }

    /// List xattr names. Flushes the active epoch first.
    pub fn list_xattr(&self, ino: u64) -> Result<Vec<Vec<u8>>, StoreError> {
        self.ensure_epoch_flushed(&crate::store::transaction::CrashHooks::none())?;
        let inode = self
            .get_inode(ino)?
            .ok_or_else(|| StoreError::Invariant(format!("inode {ino} missing")))?;
        if inode.xattr_root.is_zero() {
            return Ok(Vec::new());
        }
        let entries = index::scan_all(
            inode.xattr_root,
            BTREE_ORDER,
            self.config.limits.max_fanout,
            self,
        )?;
        Ok(entries.into_iter().map(|(k, _)| k).collect())
    }
}

/// Helper for segment rollover offset computation.
fn base_after_roll(store: &Store, encoded: &[u8]) -> u64 {
    let seg = store.segment.lock().expect("segment poisoned");
    let w = seg.as_ref().expect("segment open");
    let base = w.durable_end();
    debug_assert!(base + encoded.len() as u64 <= store.config.segment_size);
    base
}

/// The 32-byte child-id value of an internal B-tree entry.
fn child_id_value(value: &[u8]) -> ChunkId {
    ChunkId::new(value.try_into().expect("32-byte child id"))
}

/// Materialize one extent and copy its window into `out` (the shared
/// assembly step of the Phase-10F batched read path). Extent ranges are
/// disjoint, so this is safe to call per-extent in any order.
fn materialize_into_window(
    ctx: &dyn DecoderContext,
    desc: &Representation,
    start: u64,
    offset: u64,
    end: u64,
    limits: &crate::core::limits::Limits,
    out: &mut [u8],
) -> Result<(), StoreError> {
    let extent_end = start.saturating_add(desc.len()).min(end);
    let copy_start = start.max(offset);
    if copy_start >= extent_end {
        return Ok(());
    }
    let mut chunk = vec![0u8; desc.len() as usize];
    let mut budget = limits.max_decode_work;
    crate::core::materialize::materialize(desc, ctx, limits, 0, &mut budget, &mut chunk)
        .map_err(|e| StoreError::Descriptor(e.to_string()))?;
    let s = (copy_start - start) as usize;
    let c = (extent_end - copy_start) as usize;
    let o = (copy_start - offset) as usize;
    let c = c.min(out.len() - o);
    out[o..o + c].copy_from_slice(&chunk[s..s + c]);
    Ok(())
}

/// Copy one ALREADY-materialized extent's window into `out` — the shared
/// assembly step of the batched decode paths (the Phase-11C scoped-worker
/// path and the Phase-11E pool path, which materialize per-extent first
/// and assemble after). Extent ranges are disjoint and ordered, so
/// assembly is safe in any order — scheduling order can never change the
/// output bytes (the pool's determinism contract).
fn assemble_extent_window(
    out: &mut [u8],
    start: u64,
    chunk: &[u8],
    offset: u64,
    end: u64,
    avail: usize,
) {
    let extent_end = start.saturating_add(chunk.len() as u64).min(end);
    let copy_start = start.max(offset);
    if copy_start >= extent_end {
        return;
    }
    let s = (copy_start - start) as usize;
    let c = (extent_end - copy_start) as usize;
    let o = (copy_start - offset) as usize;
    let c = c.min(avail - o);
    out[o..o + c].copy_from_slice(&chunk[s..s + c]);
}

impl ObjectProvider for Store {
    fn get(&self, id: &ChunkId) -> Result<Option<Vec<u8>>, BTreeError> {
        self.fetch_object(id)
            .map_err(|e| BTreeError::Provider(e.to_string()))
    }

    fn put(&mut self, _id: ChunkId, _bytes: Vec<u8>) {
        // The store itself never creates nodes outside a transaction; this
        // is a marker path for read-only use.
        unreachable!("Store::put must not be called directly; use Tx")
    }
}

impl DecoderContext for Store {
    fn fetch_object(&self, id: &ChunkId) -> Result<Vec<u8>, MaterializeError> {
        self.fetch_object_impl(id)
    }

    fn fetch_descriptor(&self, id: &ChunkId) -> Result<Representation, MaterializeError> {
        match self
            .chunk_descriptor(id)
            .map_err(|e| MaterializeError::Universe(e.to_string()))?
        {
            Some(bytes) => crate::format::descriptor::decode(&bytes, &self.config.limits)
                .map_err(|e| MaterializeError::InvalidDescriptor(e.to_string())),
            None => Err(MaterializeError::MissingChunk(*id)),
        }
    }

    fn decode_rans(
        &self,
        model: &[u8],
        encoded: &[u8],
        scale_bits: u8,
        codec: RansCodec,
        out_len: u64,
    ) -> Result<Vec<u8>, MaterializeError> {
        // The model cache memoizes decoded models (pure memo of immutable
        // content-addressed bytes; performance only).
        let model_id = ChunkId::of(model);
        if let Some(cached) = self
            .model_cache
            .lock()
            .ok()
            .and_then(|mut c| c.get(&model_id))
        {
            if cached.scale_bits == scale_bits && cached.codec == codec {
                return crate::rans::residual::decode_stream(&cached, encoded, out_len)
                    .map_err(|e| MaterializeError::RansDecode(e.to_string()));
            }
        }
        let parsed = crate::rans::metadata::decode_model(model, self.config.limits.max_model_bytes)
            .map_err(|e| MaterializeError::RansDecode(e.to_string()))?;
        if parsed.scale_bits != scale_bits || parsed.codec != codec {
            return Err(MaterializeError::RansDecode("model tag mismatch".into()));
        }
        if let Ok(mut c) = self.model_cache.lock() {
            c.insert(model_id, parsed.clone());
        }
        crate::rans::residual::decode_stream(&parsed, encoded, out_len)
            .map_err(|e| MaterializeError::RansDecode(e.to_string()))
    }

    fn universe_bytes(
        &self,
        universe: UniverseId,
        seed: [u8; 16],
        coordinate: u64,
        range: Range<u64>,
    ) -> Result<Vec<u8>, MaterializeError> {
        match universe {
            UniverseId::UniformXofV1 => Ok(
                crate::entropy::universe::UniformXofV1::materialize_range(seed, coordinate, range),
            ),
        }
    }
}

impl Store {
    /// Internal fetch helper for the DecoderContext impl (pub(crate) for
    /// the optimizer's validation resolver).
    pub(crate) fn fetch_object_impl(&self, id: &ChunkId) -> Result<Vec<u8>, MaterializeError> {
        self.fetch_object(id)
            .map_err(|e| MaterializeError::Universe(e.to_string()))?
            .ok_or(MaterializeError::MissingObject(*id))
    }
}

/// Open the advisory lock file (flock exclusive).
fn open_lock(dir: &Path) -> Result<File, StoreError> {
    use rustix::fs::{FlockOperation, flock};
    let path = dir.join("lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    flock(&file, FlockOperation::LockExclusive)
        .map_err(|e| StoreError::Io(format!("store lock failed: {e}")))?;
    Ok(file)
}

/// Load and validate the root object a superblock references (None when
/// missing/undecodable — the recovery fallback path).
fn load_root_for(
    sb: &Superblock,
    dir: &Path,
    object_index: &ObjectIndex,
) -> Result<Option<Root>, StoreError> {
    let Some(loc) = object_index.get(&sb.root_object_id) else {
        return Ok(None);
    };
    let bytes = segment::read_payload(dir, loc.segment_seq, loc.offset, loc.stored_len)?;
    match Root::decode(&bytes) {
        Ok(root) => Ok(Some(root)),
        Err(_) => Ok(None),
    }
}

/// Ensure the store directory exists (create helper).
pub fn ensure_store_dir(dir: &Path) -> Result<(), StoreError> {
    std::fs::create_dir_all(dir)?;
    Ok(())
}

/// Current effective uid (safe wrapper; /proc/self/status fallback).
pub fn current_uid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

/// Current effective gid (safe wrapper; /proc/self/status fallback).
pub fn current_gid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Gid:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

/// Write a scratch file atomically (used by evidence/tools).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Phase-10D: metadata writeback epoch
// ---------------------------------------------------------------------------
//
// The foreground write path accumulates acknowledged namespace/writeback
// mutations in an ACTIVE EPOCH (`store/epoch.rs`) instead of committing one
// immutable transaction per operation. Each op appends its staged objects
// plus a `MutationLog` ENVELOPE (the recoverable dirty state) to the
// append-only store and flushes to the page cache BEFORE the ack — the
// same process-crash guarantee as the deferred-commit path. The committed
// trees still describe the last CHECKPOINT; on checkpoint the frozen
// overlay is merged into the trees once (bulk-load for the small
// per-directory trees, `apply_sorted_batch` for the global indexes) and
// ONE root publication carries the merged state plus the consumed log
// sequence. Recovery replays envelopes with `seq > root.log_seq`.

impl Store {
    /// The active epoch (serialized by its mutex).
    pub fn epoch(&self) -> std::sync::MutexGuard<'_, crate::store::epoch::Epoch> {
        self.epoch.lock().expect("epoch poisoned")
    }

    /// Append one epoch op's staged records + envelope (the per-op ack
    /// path): append + flush to the page cache under the commit
    /// coordinator; persist the MutationLog incompat bit once. The root
    /// and superblock generation are untouched — the committed trees still
    /// describe the last checkpoint.
    pub(crate) fn epoch_append(
        &self,
        records: Vec<crate::store::transaction::PendingRecord>,
        hooks: &CrashHooks,
    ) -> Result<(), StoreError> {
        // Phase-11B: the commit-coordinator wait is the shared write-side
        // serialization resource the 4->16-thread plateau points at — it
        // gets its own exclusive partition row.
        let _guard = self.perf.time_request("commit_lock_wait", || {
            self.commit_lock.lock().expect("commit lock poisoned")
        });
        let needs_bit = {
            self.commit
                .read()
                .expect("commit state poisoned")
                .features_in_use
                & crate::format::features::Feature::MutationLog.mask()
                == 0
        };
        let mut recs = records;
        self.perf
            .time_request("append", || self.append_records(&mut recs))?;
        // Process-crash durable (page cache); the durability barrier makes
        // it power-durable, exactly like every other commit.
        self.perf.time_request("flush", || self.flush_segment())?;
        if needs_bit {
            // Persist the incompat bit so an implementation that cannot
            // replay the log refuses the store.
            self.commit
                .write()
                .expect("commit state poisoned")
                .features_in_use |= crate::format::features::Feature::MutationLog.mask();
            let root = self.current_root();
            let root_id = root.id();
            self.write_superblock(root_id, &root)?;
        }
        hooks.hit(CrashPoint::AfterSegmentFdatasync)?;
        Ok(())
    }

    // -- overlay-aware reads (committed trees + the active epoch) ------

    /// Overlay-aware inode read.
    pub fn get_inode_epoch(
        &self,
        ep: &crate::store::epoch::Epoch,
        ino: u64,
    ) -> Result<Option<Inode>, StoreError> {
        let committed = self.get_inode(ino)?;
        let out = ep.overlay_inode(ino, committed);
        Ok(out)
    }

    /// Overlay-aware directory lookup.
    pub fn dir_lookup_epoch(
        &self,
        ep: &crate::store::epoch::Epoch,
        dir_ino: u64,
        name: &[u8],
    ) -> Result<Option<directory::DirEntry>, StoreError> {
        if let Some(e) = ep.overlay_entry(dir_ino, name) {
            return Ok(Some(e));
        }
        if ep.removed_entries.contains(&(dir_ino, name.to_vec())) {
            return Ok(None);
        }
        // Fall back to the committed tree through the CURRENT committed
        // parent inode — NOT the overlay inode's dir_root, which is the
        // committed root captured when the epoch's first pending op read
        // it and goes STALE once a checkpoint publishes a newer directory
        // tree (Phase-10G: the parallel namespace court hit "no such
        // entry" on an entry the epoch-merged committed tree already
        // contains). An epoch-created directory has no committed inode;
        // its dir_root is ZERO until the checkpoint.
        let dir_root = match self.get_inode(dir_ino)? {
            Some(i) => match i.data {
                InodeData::Directory { dir_root } => dir_root,
                _ => return Err(StoreError::Invariant("not a directory".into())),
            },
            None => crate::core::extent::ChunkId::ZERO,
        };
        if dir_root.is_zero() {
            return Ok(None);
        }
        Ok(directory::lookup(
            dir_root,
            name,
            BTREE_ORDER,
            self.config.limits.max_fanout,
            self,
        )?)
    }

    /// Overlay-aware chunk descriptor.
    pub fn chunk_descriptor_epoch(
        &self,
        ep: &crate::store::epoch::Epoch,
        cid: &crate::core::extent::ChunkId,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        if let Some(b) = ep.overlay_chunk(cid) {
            return Ok(Some(b));
        }
        self.chunk_descriptor(cid)
    }

    /// Overlay-aware directory scan (name order).
    pub fn read_dir_epoch(
        &self,
        ep: &crate::store::epoch::Epoch,
        dir_ino: u64,
    ) -> Result<Vec<(Vec<u8>, directory::DirEntry)>, StoreError> {
        // The CURRENT committed inode's dir_root (not the overlay inode's,
        // which goes stale once a checkpoint publishes a newer directory
        // tree — see `dir_lookup_epoch`). An epoch-created directory has
        // no committed inode yet (empty base).
        let dir_root = match self.get_inode(dir_ino)? {
            Some(i) => match i.data {
                InodeData::Directory { dir_root } => dir_root,
                _ => return Err(StoreError::Invariant("not a directory".into())),
            },
            None => crate::core::extent::ChunkId::ZERO,
        };
        let mut merged: std::collections::BTreeMap<Vec<u8>, directory::DirEntry> =
            std::collections::BTreeMap::new();
        if !dir_root.is_zero() {
            let committed = directory::scan(
                dir_root,
                None,
                usize::MAX,
                BTREE_ORDER,
                self.config.limits.max_fanout,
                self,
            )?
            .0;
            for (name, e) in committed {
                merged.insert(name, e);
            }
        }
        for ((p, name), e) in ep.pending_entries.iter() {
            if *p == dir_ino {
                merged.insert(name.clone(), *e);
            }
        }
        for (p, name) in ep.removed_entries.iter() {
            if *p == dir_ino {
                merged.remove(name);
            }
        }
        Ok(merged.into_iter().collect())
    }

    /// Overlay-aware file read: the committed extents shadowed by the
    /// epoch's pending writes, clamped to the epoch's file size.
    pub fn read_file_epoch(
        &self,
        ep: &crate::store::epoch::Epoch,
        ino: u64,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, StoreError> {
        let Some(prepared) = self.read_file_epoch_prepare(ep, ino, offset, len)? else {
            return Ok(Vec::new());
        };
        // Phase-11C: the decode half touches no epoch state, so the caller
        // may release the epoch guard before calling it.
        self.materialize_decode(prepared)
    }

    /// The guard-dependent half of an overlay-aware file read: the inode
    /// view, the range-limited extent collection (committed scan overlaid
    /// with the epoch's pending extents), the dependency enumeration, and
    /// the batched object fetch. The caller must hold the epoch guard; the
    /// returned [`PreparedRead`] decodes WITHOUT it (`None` for an empty
    /// window). `pub(crate)` so the FUSE read can release the guard before
    /// the decode half.
    pub(crate) fn read_file_epoch_prepare(
        &self,
        ep: &crate::store::epoch::Epoch,
        ino: u64,
        offset: u64,
        len: u64,
    ) -> Result<Option<PreparedRead>, StoreError> {
        let inode = self
            .get_inode_epoch(ep, ino)?
            .ok_or_else(|| StoreError::Invariant(format!("inode {ino} missing")))?;
        if !inode.is_file() {
            return Err(StoreError::Invariant("not a regular file".into()));
        }
        let limits = self.config.limits;
        // The extent_root inside the epoch's PENDING inode is the
        // committed root captured when the first pending write read it —
        // STALE once a checkpoint publishes a newer root (and the newer
        // committed extents were removed from the overlay). The committed
        // trees are immutable, so the read must walk the CURRENT
        // committed inode's extent_root (overlaid with the pending
        // extents) for a complete view. A file created in this epoch has
        // no committed inode yet — every extent is pending (empty base).
        // (Phase-10G: the mounted parallel read-back hit holes at chunk
        // boundaries when a checkpoint merged mid-epoch writes.)
        let extent_root = match self.get_inode(ino)? {
            Some(i) => match i.data {
                InodeData::File { extent_root } => extent_root,
                _ => crate::core::extent::ChunkId::ZERO,
            },
            None => crate::core::extent::ChunkId::ZERO,
        };
        // Clamp to the final file size (the epoch's pending size).
        let size = inode.size;
        let end = offset.saturating_add(len).min(size);
        if end <= offset {
            return Ok(None);
        }
        // Phase-10E: RANGE-LIMITED collection — the committed extents in
        // [covering(offset), end) via one traversal, overlaid with the
        // epoch's pending extents in the same window (a full-tree scan
        // would walk every leaf for a small read).
        let mut scan_start = if extent_root.is_zero() {
            offset
        } else {
            match crate::store::extent_tree::covering(
                extent_root,
                offset,
                BTREE_ORDER,
                limits.max_fanout,
                self,
            )? {
                Some((start, _)) => start,
                None => offset,
            }
        };
        // A PENDING extent may cover `offset` while starting below the
        // committed predecessor — when the writes are still in the epoch,
        // the committed tree lacks them, so the covering above falls back
        // to `offset` and the pending range below would miss the covering
        // extent (Phase-10G: the mounted page-granular reads hit holes at
        // chunk boundaries exactly this way). Extend the window to the
        // pending predecessor ONLY when it actually COVERS `offset` (a
        // mid-chunk read of an uncommitted chunk): a chunk-aligned read's
        // predecessor is the previous chunk, which does not cover the
        // offset — pulling it in collects an out-of-window extent and
        // forces a multi-extent decode for every prefill read (Phase-11C:
        // measured as the read_decode explosion at 2+ threads).
        if let Some(((_, poff), bytes)) = ep.pending_extents.range(..=(ino, offset)).next_back() {
            let covers = crate::format::descriptor::decode(bytes, &limits)
                .map(|d| *poff + d.len() > offset)
                .unwrap_or(false);
            if covers {
                scan_start = scan_start.min(*poff);
            }
        }
        let mut extents: std::collections::BTreeMap<u64, Vec<u8>> =
            std::collections::BTreeMap::new();
        if !extent_root.is_zero() {
            let scanned = self.perf.time_request("read_scan", || {
                self.scan_extents_batched(extent_root, scan_start, end)
            })?;
            for (off, bytes) in scanned {
                extents.insert(off, bytes);
            }
        }
        // The pending window is half-open: `(ino, scan_start)..(ino, end)`
        // with `end` EXCLUSIVE (one past the read's last byte, clamped to
        // the file size). The exclusive upper bound is load-bearing:
        // Phase-11C found the window with an INCLUSIVE bound — a
        // chunk-aligned read then pulled in the NEXT chunk's pending
        // extent, forcing a multi-extent decode + worker-semaphore wait on
        // every write-path prefill read (measured as the 11C read_decode
        // explosion at 2+ threads). The regression test
        // `epoch_read_window_excludes_adjacent_pending_extent` pins both
        // this bound and the conditional predecessor extension above.
        for ((fino, off), bytes) in ep.pending_extents.range((ino, scan_start)..(ino, end)) {
            let _ = fino;
            extents.insert(*off, bytes.clone());
        }
        // A pending extent may COVER `offset` while starting below
        // `scan_start`'s predecessor logic missed it (pending extents are
        // always chunk-aligned, so the covering pending extent starts at
        // scan_start or later; nothing to add here).
        let avail = (end - offset) as usize;
        let merged: Vec<(u64, Vec<u8>)> = extents.into_iter().collect();
        // Phase-10F/11C: ONE prefetch submission for every extent's
        // materialization dependencies (overlay-aware); the decode half
        // runs without the guard.
        let prepared = self.materialize_prepare(Some(ep), &merged, offset, end, avail)?;
        Ok(Some(prepared))
    }

    /// Flush the active epoch to a checkpoint (merge + one root
    /// publication). A no-op when the epoch is empty. GC and the
    /// background optimizer call this first: the epoch's staged objects
    /// are only referenced by the log, which GC's reachability walk does
    /// not see as roots.
    pub fn epoch_checkpoint(&self, hooks: &CrashHooks) -> Result<(), StoreError> {
        // Fast path: nothing pending (also avoids taking the commit lock
        // for the common empty-epoch case).
        if self.epoch().is_empty() {
            return Ok(());
        }
        let limits = self.config.limits;
        let fanout = limits.max_fanout;
        let mut tx = self.perf.time_request("cp_lock_wait", || self.begin_tx())?;
        // SNAPSHOT the pending overlay; the live epoch KEEPS its state.
        // The merge runs on the snapshot, and the snapshot's entries are
        // compare-and-removed only after the commit SUCCEEDS. Two
        // properties follow:
        //
        // 1. The snapshot is taken UNDER the commit lock, so a checkpoint
        //    that waited behind another checkpoint merges the CURRENT
        //    overlay — never a snapshot taken before the earlier
        //    checkpoint's commit + remove. An un-serialized snapshot could
        //    revert a newer committed tree to a stale inode: the stale
        //    snapshot's merge base is the NEWER root (whose extents stay),
        //    while its inode (older size) overwrites the newer one — the
        //    Phase-10G corruption where a file's committed size regressed
        //    while its tail extents remained, tripping fsck ("extent ends
        //    beyond file size") and short reads on the mount.
        //
        // 2. Closing the visibility gap of the old take-first design: a
        //    concurrent epoch op between the freeze (mem::take) and the
        //    root publication saw an EMPTY overlay AND a STALE committed
        //    root and reported spurious "inode missing" invariants
        //    (epoch ops read through the overlay + committed trees without
        //    the commit lock, so they must never observe the checkpoint
        //    mid-flight). A failed commit also leaves the overlay intact
        //    for the next attempt instead of silently discarding
        //    acknowledged state.
        //
        // The envelope sequence counter is GLOBALLY MONOTONIC: it is only
        // ever bumped (never reset), so a post-checkpoint op can never
        // reuse a sequence the checkpoint already consumed into `log_seq`.
        // Without monotonicity, an op staged after a checkpoint could
        // receive a small seq <= an earlier log_seq and be silently
        // dropped at recovery (its overlay was never checkpointed), and
        // two epochs could emit envelopes sharing a sequence (the
        // recovery duplicate invariant). The inode high-water mark is
        // monotonic the same way: a reset to 0 would make the next create
        // re-scan the committed index — STALE while this checkpoint's
        // root is mid-commit — and hand out an inode number another
        // (already-merged) create allocated, corrupting the namespace
        // (two files, one ino).
        let frozen: crate::store::epoch::Epoch = {
            let ep = self.epoch();
            if ep.is_empty() {
                // Another checkpoint merged everything while we waited
                // for the commit lock; nothing to do.
                return Ok(());
            }
            crate::store::epoch::Epoch {
                seq: ep.seq,
                pending_inodes: ep.pending_inodes.clone(),
                removed_inodes: ep.removed_inodes.clone(),
                pending_entries: ep.pending_entries.clone(),
                removed_entries: ep.removed_entries.clone(),
                pending_extents: ep.pending_extents.clone(),
                pending_chunks: ep.pending_chunks.clone(),
                staged_objects: ep.staged_objects.clone(),
                staged_payloads: std::collections::HashMap::new(),
                feature_persisted: ep.feature_persisted,
                max_ino: ep.max_ino,
            }
        };
        let committed_root = tx.root().clone();

        // 1. Final inode map: the epoch's pending inodes (the ops updated
        //    mtime/nlink/size) plus the directory and file trees the
        //    checkpoint rebuilds (dir_root / extent_root become the new
        //    tree roots).
        let mut final_inodes: std::collections::BTreeMap<u64, Inode> =
            frozen.pending_inodes.clone();

        // 2. Rebuild every affected directory tree ONCE (bulk-load: the
        //    merged entry set bottom-up, each node staged exactly once).
        let mut affected_dirs: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        for (parent, _) in frozen.pending_entries.keys() {
            affected_dirs.insert(*parent);
        }
        for (parent, _) in frozen.removed_entries.iter() {
            affected_dirs.insert(*parent);
        }
        self.perf.time_request("cp_dir_build", || {
            for parent in &affected_dirs {
                // The base directory tree is the COMMITTED parent's tree (the
                // epoch never rebuilt it); an epoch-created directory has no
                // committed tree (empty base).
                let committed_parent = Store::inode_for_tx(&tx, *parent).ok();
                let dir_root = match committed_parent.as_ref().map(|i| &i.data) {
                    Some(InodeData::Directory { dir_root }) => *dir_root,
                    _ => crate::core::extent::ChunkId::ZERO,
                };
                let mut merged: std::collections::BTreeMap<Vec<u8>, directory::DirEntry> =
                    std::collections::BTreeMap::new();
                if !dir_root.is_zero() {
                    for (name, e) in
                        directory::scan(dir_root, None, usize::MAX, BTREE_ORDER, fanout, &tx)?.0
                    {
                        merged.insert(name, e);
                    }
                }
                for ((p, name), e) in frozen.pending_entries.iter() {
                    if *p == *parent {
                        merged.insert(name.clone(), *e);
                    }
                }
                for (p, name) in frozen.removed_entries.iter() {
                    if *p == *parent {
                        merged.remove(name);
                    }
                }
                let entries: Vec<(Vec<u8>, Vec<u8>)> =
                    merged.into_iter().map(|(n, e)| (n, e.encode())).collect();
                let new_dir_root =
                    crate::store::index::bulk_load(&entries, BTREE_ORDER, fanout, &mut tx)?;
                let pin = final_inodes.entry(*parent).or_insert_with(|| {
                    committed_parent
                        .clone()
                        .expect("affected parent inode must exist (committed or pending)")
                });
                pin.data = InodeData::Directory {
                    dir_root: new_dir_root,
                };
            }
            Ok::<(), StoreError>(())
        })?;

        // 3. Rebuild every affected extent tree ONCE (bulk COW patch).
        let mut affected_files: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        for (ino, _) in frozen.pending_extents.keys() {
            affected_files.insert(*ino);
        }
        self.perf.time_request("cp_extent_build", || {
            for ino in &affected_files {
                // The base extent tree is the COMMITTED file's tree; an
                // epoch-created file has no committed tree (empty base).
                let committed_file = Store::inode_for_tx(&tx, *ino).ok();
                let extent_root = match committed_file.as_ref().map(|i| &i.data) {
                    Some(InodeData::File { extent_root }) => *extent_root,
                    _ => crate::core::extent::ChunkId::ZERO,
                };
                let mut batch: Vec<(Vec<u8>, Option<Vec<u8>>)> = Vec::new();
                for ((fino, off), bytes) in frozen.pending_extents.iter() {
                    if *fino == *ino {
                        batch.push((off.to_be_bytes().to_vec(), Some(bytes.clone())));
                    }
                }
                let new_extent_root = crate::store::index::apply_sorted_batch(
                    extent_root,
                    &batch,
                    BTREE_ORDER,
                    fanout,
                    &mut tx,
                )?;
                let fin = final_inodes.entry(*ino).or_insert_with(|| {
                    committed_file
                        .clone()
                        .expect("affected file inode must exist (committed or pending)")
                });
                fin.data = InodeData::File {
                    extent_root: new_extent_root,
                };
            }
            Ok::<(), StoreError>(())
        })?;

        // 4. Stage every final inode object (dedup against the log-staged
        //    records and the committed CAS) and build the inode-index
        //    batch (one sorted, duplicate-free pass). Removed inodes drop
        //    their entries.
        let mut inode_batch_map: std::collections::BTreeMap<Vec<u8>, Option<Vec<u8>>> =
            std::collections::BTreeMap::new();
        self.perf.time_request("cp_stage_inodes", || {
            for ino in frozen.removed_inodes.iter() {
                inode_batch_map.insert(ino.to_be_bytes().to_vec(), None);
            }
            for (ino, inode) in &final_inodes {
                if frozen.removed_inodes.contains(ino) {
                    continue; // removed in this epoch: drop, do not re-add
                }
                let id = crate::store::transaction::put_object(
                    &mut tx,
                    RecordTag::Inode,
                    inode.encode(),
                    None,
                );
                inode_batch_map.insert(ino.to_be_bytes().to_vec(), Some(id.as_bytes().to_vec()));
            }
            Ok::<(), StoreError>(())
        })?;
        let inode_batch: Vec<(Vec<u8>, Option<Vec<u8>>)> = inode_batch_map.into_iter().collect();

        // 5. Chunk index: the pending descriptors (bulk COW patch).
        let mut chunk_batch: Vec<(Vec<u8>, Option<Vec<u8>>)> = Vec::new();
        for (cid, desc) in frozen.pending_chunks.iter() {
            chunk_batch.push((cid.as_bytes().to_vec(), Some(desc.clone())));
        }
        tx.root_mut().chunk_index_root = self.perf.time_request("cp_chunk_apply", || {
            crate::store::index::apply_sorted_batch(
                committed_root.chunk_index_root,
                &chunk_batch,
                BTREE_ORDER,
                fanout,
                &mut tx,
            )
        })?;

        // 6. Apply the inode index batch once.
        tx.root_mut().inode_index_root = self.perf.time_request("cp_inode_apply", || {
            crate::store::index::apply_sorted_batch(
                committed_root.inode_index_root,
                &inode_batch,
                BTREE_ORDER,
                fanout,
                &mut tx,
            )
        })?;

        // 7. The checkpoint root consumes the frozen log sequence. `max`
        //    with the pre-commit root keeps log_seq globally monotonic
        //    even when two checkpoints overlap (a later-committing
        //    checkpoint may have snapshotted an EARLIER counter value than
        //    the one the previous checkpoint already published).
        tx.root_mut().log_seq = frozen.seq.max(tx.root().log_seq);
        let new_log_seq = tx.root().log_seq;
        tx.commit_deferred(hooks)?;
        // 8. The commit succeeded: drop exactly the snapshot's overlay
        //    entries. Compare-and-remove — an op that staged a NEWER value
        //    for the same key while the merge ran keeps its entry (it is
        //    the next checkpoint's work, and its envelope has seq >
        //    log_seq). For the removal SETS, the snapshot's merge already
        //    made the removal effective in the trees, so a re-added key is
        //    redundant and safe to drop.
        self.perf.time_request("cp_overlay_remove", || {
            {
                let mut ep = self.epoch();
                // Phase-11C: the lock-free pending-op mirror — the ops the
                // merge consumed are gone; ops staged while the merge ran
                // keep their count (their envelopes have seq > log_seq).
                self.epoch_pending.store(
                    ep.seq.saturating_sub(new_log_seq),
                    std::sync::atomic::Ordering::Relaxed,
                );
                for (ino, inode) in &frozen.pending_inodes {
                    if ep.pending_inodes.get(ino) == Some(inode) {
                        ep.pending_inodes.remove(ino);
                    }
                }
                for ino in &frozen.removed_inodes {
                    ep.removed_inodes.remove(ino);
                }
                for ((parent, name), entry) in &frozen.pending_entries {
                    if ep.pending_entries.get(&(*parent, name.clone())) == Some(entry) {
                        ep.pending_entries.remove(&(*parent, name.clone()));
                    }
                }
                for key in &frozen.removed_entries {
                    ep.removed_entries.remove(key);
                }
                for ((ino, off), bytes) in &frozen.pending_extents {
                    if ep.pending_extents.get(&(*ino, *off)) == Some(bytes) {
                        ep.pending_extents.remove(&(*ino, *off));
                    }
                }
                for (cid, bytes) in &frozen.pending_chunks {
                    if ep.pending_chunks.get(cid) == Some(bytes) {
                        ep.pending_chunks.remove(cid);
                    }
                }
                // The staged-object dedup set restarts empty: every object it
                // named is now committed (this merge) or was already appended
                // and indexed, so `epoch_stage` still dedups through the
                // object index. The staged PAYLOADS (for in-flight overlay
                // reads) are dropped with it.
                ep.staged_objects.clear();
                ep.staged_payloads.clear();
            }
            Ok::<(), StoreError>(())
        })?;
        Ok(())
    }

    /// Stage an object record for the epoch (dedup against the epoch's
    /// staged set AND the committed object index: an already-committed
    /// object must not get a duplicate physical record).
    fn epoch_stage(
        ep: &mut crate::store::epoch::Epoch,
        store: &Store,
        records: &mut Vec<crate::store::transaction::PendingRecord>,
        tag: RecordTag,
        payload: Vec<u8>,
        materialized_len: Option<u64>,
    ) -> crate::core::extent::ChunkId {
        let id = crate::core::extent::ChunkId::of(&payload);
        if ep.is_staged(&id) || store.object_index().contains(&id) {
            return id;
        }
        ep.mark_staged(id);
        // Retain the payload for overlay reads that resolve this object
        // before the op's append lands (see `Epoch::staged_payloads`).
        ep.staged_payloads.insert(id, payload.clone());
        records.push(crate::store::transaction::PendingRecord {
            tag,
            payload,
            materialized_len,
        });
        id
    }

    /// Phase-10D epoch create: validate against the overlay, stage the
    /// inode objects, append the MutationLog envelope, ack. The directory
    /// entry and index trees are built at the CHECKPOINT, not here.
    pub fn epoch_create(
        &self,
        parent: u64,
        name: &[u8],
        entry: NewEntry,
        hooks: &CrashHooks,
    ) -> Result<u64, StoreError> {
        if !Self::validate_name(name) {
            return Err(StoreError::Config("invalid entry name".into()));
        }
        let mut ep = self.epoch();
        let parent_inode = self
            .get_inode_epoch(&ep, parent)?
            .ok_or_else(|| StoreError::Invariant(format!("parent {parent} missing")))?;
        if !matches!(parent_inode.data, InodeData::Directory { .. }) {
            return Err(StoreError::Invariant("parent not a directory".into()));
        }
        if self.dir_lookup_epoch(&ep, parent, name)?.is_some() {
            return Err(StoreError::Invariant("entry already exists".into()));
        }
        let kind = &entry.kind;
        let ino = if ep.max_ino == 0 {
            // First allocation this epoch: the committed high-water mark
            // (inos are never reused, so the committed max is the max
            // ever allocated).
            let committed = self.all_inodes()?.iter().copied().max().unwrap_or(1);
            ep.max_ino = committed.saturating_add(1);
            ep.max_ino
        } else {
            ep.max_ino = ep.max_ino.saturating_add(1);
            ep.max_ino
        };
        let inode = match kind {
            EntryKind::File => Inode::new_file(entry.uid, entry.gid, entry.mode),
            EntryKind::Directory => Inode::new_dir(entry.uid, entry.gid, entry.mode),
            EntryKind::Symlink(target) => Inode::new_symlink(target.clone(), entry.uid, entry.gid),
            EntryKind::Device(is_char, rdev) => {
                let mut i = Inode::new_file(entry.uid, entry.gid, entry.mode);
                i.data_kind = crate::store::inode::DATA_DEVICE;
                i.data = InodeData::Device;
                i.rdev = *rdev;
                i.mode = (if *is_char {
                    crate::store::inode::mode::S_IFCHR
                } else {
                    crate::store::inode::mode::S_IFBLK
                }) | (entry.mode & crate::store::inode::mode::S_IPERM);
                i
            }
        };
        let d_type = match kind {
            EntryKind::File => directory::dt::DT_REG,
            EntryKind::Directory => directory::dt::DT_DIR,
            EntryKind::Symlink(_) => directory::dt::DT_LNK,
            EntryKind::Device(_, _) => directory::dt::DT_UNKNOWN,
        };
        let mut records: Vec<crate::store::transaction::PendingRecord> = Vec::new();
        let inode_id = Self::epoch_stage(
            &mut ep,
            self,
            &mut records,
            RecordTag::Inode,
            inode.encode(),
            None,
        );
        let mut pin = parent_inode;
        pin.mtime = crate::store::inode::Timespec::now();
        if matches!(kind, EntryKind::Directory) {
            pin.nlink = pin.nlink.saturating_add(1);
        }
        let parent_inode_id = Self::epoch_stage(
            &mut ep,
            self,
            &mut records,
            RecordTag::Inode,
            pin.encode(),
            None,
        );
        // Overlay.
        ep.pending_inodes.insert(ino, inode);
        ep.pending_inodes.insert(parent, pin);
        ep.pending_entries
            .insert((parent, name.to_vec()), directory::DirEntry { ino, d_type });
        let env = ep.envelope(&crate::store::epoch::MutationOp::Create {
            parent,
            name: name.to_vec(),
            ino,
            d_type,
            inode_id,
            parent_inode_id,
        });
        // Phase-11C: the lock-free pending-op mirror (this op is now
        // staged; the checkpoint-threshold check reads it lock-free).
        self.epoch_pending
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        records.push(crate::store::transaction::PendingRecord {
            tag: RecordTag::MutationLog,
            payload: env,
            materialized_len: None,
        });
        drop(ep);
        self.epoch_append(records, hooks)?;
        self.maybe_checkpoint_epoch()?;
        Ok(ino)
    }

    /// Phase-10D epoch setattr for NON-SIZE updates (mode/uid/gid/times).
    /// A size change flushes the epoch and runs the transactional
    /// truncate path (truncates are rare; the batching win is the common
    /// times/mode update).
    pub fn epoch_setattr(
        &self,
        ino: u64,
        update: &AttrUpdate,
        hooks: &CrashHooks,
    ) -> Result<Inode, StoreError> {
        if update.size.is_some() {
            // Flush the epoch first so the truncate sees a clean,
            // committed file state.
            self.epoch_checkpoint(hooks)?;
            return self.setattr_inode(ino, update, hooks);
        }
        let mut ep = self.epoch();
        let mut inode = self
            .get_inode_epoch(&ep, ino)?
            .ok_or_else(|| StoreError::Invariant(format!("inode {ino} missing")))?;
        if let Some(m) = update.mode {
            inode.mode = (inode.mode & crate::store::inode::mode::S_IFMT) | (m & 0o7777);
        }
        if let Some(u) = update.uid {
            inode.uid = u;
        }
        if let Some(g) = update.gid {
            inode.gid = g;
        }
        if let Some(a) = update.atime {
            inode.atime = a;
        }
        if let Some(m) = update.mtime {
            inode.mtime = m;
        }
        inode.ctime = crate::store::inode::Timespec::now();
        let mut records: Vec<crate::store::transaction::PendingRecord> = Vec::new();
        let inode_id = Self::epoch_stage(
            &mut ep,
            self,
            &mut records,
            RecordTag::Inode,
            inode.encode(),
            None,
        );
        let env = ep.envelope(&crate::store::epoch::MutationOp::Setattr { ino, inode_id });
        // Phase-11C: the lock-free pending-op mirror (the op is now
        // staged; a checkpoint may merge it later).
        self.epoch_pending
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ep.pending_inodes.insert(ino, inode.clone());
        records.push(crate::store::transaction::PendingRecord {
            tag: RecordTag::MutationLog,
            payload: env,
            materialized_len: None,
        });
        drop(ep);
        self.epoch_append(records, hooks)?;
        self.maybe_checkpoint_epoch()?;
        Ok(inode)
    }

    /// Phase-10D epoch unlink/rmdir.
    pub fn epoch_unlink(
        &self,
        parent: u64,
        name: &[u8],
        is_dir: bool,
        hooks: &CrashHooks,
    ) -> Result<u64, StoreError> {
        if !Self::validate_name(name) {
            return Err(StoreError::Config("invalid entry name".into()));
        }
        let mut ep = self.epoch();
        let entry = self
            .dir_lookup_epoch(&ep, parent, name)?
            .ok_or_else(|| StoreError::Invariant("no such entry".into()))?;
        let target = self
            .get_inode_epoch(&ep, entry.ino)?
            .ok_or_else(|| StoreError::Invariant("target inode missing".into()))?;
        if is_dir {
            if !target.is_dir() {
                return Err(StoreError::Invariant("not a directory".into()));
            }
            // A directory is empty when its OVERLAY view has no entries.
            if !self.read_dir_epoch(&ep, entry.ino)?.is_empty() {
                return Err(StoreError::Invariant("directory not empty".into()));
            }
        } else if target.is_dir() {
            return Err(StoreError::Invariant("is a directory".into()));
        }
        let mut records: Vec<crate::store::transaction::PendingRecord> = Vec::new();
        let mut pin = self
            .get_inode_epoch(&ep, parent)?
            .ok_or_else(|| StoreError::Invariant("parent missing".into()))?;
        pin.mtime = crate::store::inode::Timespec::now();
        if target.is_dir() {
            pin.nlink = pin.nlink.saturating_sub(1);
        }
        let parent_inode_id = Self::epoch_stage(
            &mut ep,
            self,
            &mut records,
            RecordTag::Inode,
            pin.encode(),
            None,
        );
        // The child: drop on rmdir / nlink-0, else stage the updated inode.
        let mut child_inode_id = None;
        if is_dir {
            ep.removed_inodes.insert(entry.ino);
        } else {
            let mut t = target;
            t.nlink = t.nlink.saturating_sub(1);
            if t.nlink == 0 {
                ep.removed_inodes.insert(entry.ino);
            } else {
                let id = Self::epoch_stage(
                    &mut ep,
                    self,
                    &mut records,
                    RecordTag::Inode,
                    t.encode(),
                    None,
                );
                child_inode_id = Some(id);
                ep.pending_inodes.insert(entry.ino, t);
            }
        }
        ep.pending_inodes.insert(parent, pin);
        ep.removed_entries.insert((parent, name.to_vec()));
        let env = ep.envelope(&crate::store::epoch::MutationOp::Unlink {
            parent,
            name: name.to_vec(),
            child: entry.ino,
            is_dir,
            parent_inode_id,
            child_inode_id,
        });
        // Phase-11C: the lock-free pending-op mirror.
        self.epoch_pending
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        records.push(crate::store::transaction::PendingRecord {
            tag: RecordTag::MutationLog,
            payload: env,
            materialized_len: None,
        });
        drop(ep);
        self.epoch_append(records, hooks)?;
        self.maybe_checkpoint_epoch()?;
        Ok(entry.ino)
    }

    /// Phase-10D epoch rename (POSIX type rules; a replaced destination is
    /// dropped). Overlay-only: the directory trees are rebuilt at the
    /// checkpoint.
    pub fn epoch_rename(
        &self,
        src_parent: u64,
        src_name: &[u8],
        dst_parent: u64,
        dst_name: &[u8],
        hooks: &CrashHooks,
    ) -> Result<crate::store::RenameOutcome, StoreError> {
        if !Self::validate_name(src_name) || !Self::validate_name(dst_name) {
            return Err(StoreError::Config("invalid entry name".into()));
        }
        // Renaming a name onto itself is a POSIX no-op.
        if src_parent == dst_parent && src_name == dst_name {
            let entry = self
                .dir_lookup_epoch(&self.epoch(), src_parent, src_name)?
                .ok_or_else(|| StoreError::Invariant("no such entry".into()))?;
            return Ok(crate::store::RenameOutcome {
                src_ino: entry.ino,
                replaced_dst_ino: None,
            });
        }
        let mut ep = self.epoch();
        let src_entry = self
            .dir_lookup_epoch(&ep, src_parent, src_name)?
            .ok_or_else(|| StoreError::Invariant("no such entry".into()))?;
        let src_inode = self
            .get_inode_epoch(&ep, src_entry.ino)?
            .ok_or_else(|| StoreError::Invariant("src inode missing".into()))?;
        let src_is_dir = src_inode.is_dir();
        let sp = self
            .get_inode_epoch(&ep, src_parent)?
            .ok_or_else(|| StoreError::Invariant("src parent missing".into()))?;
        let dp = self
            .get_inode_epoch(&ep, dst_parent)?
            .ok_or_else(|| StoreError::Invariant("dst parent missing".into()))?;
        if !matches!(dp.data, InodeData::Directory { .. }) {
            return Err(StoreError::Invariant("dst parent not a directory".into()));
        }
        let mut replaced_dst_ino = None;
        let mut replaced_dst_is_dir = false;
        if let Some(dst_entry) = self.dir_lookup_epoch(&ep, dst_parent, dst_name)? {
            if dst_entry.ino != src_entry.ino {
                let dst_inode = self
                    .get_inode_epoch(&ep, dst_entry.ino)?
                    .ok_or_else(|| StoreError::Invariant("dst inode missing".into()))?;
                let dst_is_dir = dst_inode.is_dir();
                replaced_dst_is_dir = dst_is_dir;
                if src_is_dir && !dst_is_dir {
                    return Err(StoreError::Invariant("cannot rename dir over file".into()));
                }
                if !src_is_dir && dst_is_dir {
                    return Err(StoreError::Invariant("cannot rename file over dir".into()));
                }
                if src_is_dir && dst_is_dir && !self.read_dir_epoch(&ep, dst_entry.ino)?.is_empty()
                {
                    return Err(StoreError::Invariant("directory not empty".into()));
                }
                replaced_dst_ino = Some(dst_entry.ino);
                // Drop the destination's inode reference.
                if dst_is_dir {
                    ep.removed_inodes.insert(dst_entry.ino);
                } else {
                    let mut t = dst_inode;
                    t.nlink = t.nlink.saturating_sub(1);
                    if t.nlink == 0 {
                        ep.removed_inodes.insert(dst_entry.ino);
                    } else {
                        ep.pending_inodes.insert(dst_entry.ino, t);
                    }
                }
            }
        }
        let mut records: Vec<crate::store::transaction::PendingRecord> = Vec::new();
        // The moved entry's inode (unchanged for a plain rename).
        let src_child_inode_id = Self::epoch_stage(
            &mut ep,
            self,
            &mut records,
            RecordTag::Inode,
            src_inode.encode(),
            None,
        );
        // Source parent update (mtime; nlink when a directory leaves).
        let mut nsp = sp.clone();
        nsp.mtime = crate::store::inode::Timespec::now();
        let mut ndp = dp.clone();
        ndp.mtime = crate::store::inode::Timespec::now();
        if src_parent == dst_parent {
            // One parent, one entry set change.
        } else if src_is_dir {
            nsp.nlink = nsp.nlink.saturating_sub(1);
            ndp.nlink = ndp.nlink.saturating_add(1);
        }
        // A replaced directory decrements the destination parent's nlink.
        if replaced_dst_is_dir {
            ndp.nlink = ndp.nlink.saturating_sub(1);
        }
        let sp_inode_id = Self::epoch_stage(
            &mut ep,
            self,
            &mut records,
            RecordTag::Inode,
            nsp.encode(),
            None,
        );
        let dp_inode_id = Self::epoch_stage(
            &mut ep,
            self,
            &mut records,
            RecordTag::Inode,
            ndp.encode(),
            None,
        );
        // Overlay entry moves.
        if src_parent != dst_parent {
            ep.removed_entries.insert((src_parent, src_name.to_vec()));
        }
        ep.pending_entries
            .insert((dst_parent, dst_name.to_vec()), src_entry);
        if src_parent != dst_parent {
            ep.pending_inodes.insert(src_parent, nsp);
            ep.pending_inodes.insert(dst_parent, ndp);
        } else {
            ep.pending_inodes.insert(src_parent, nsp);
        }
        // The source entry: a same-parent rename moves dst over src.
        if src_parent == dst_parent {
            // dst was inserted above; drop the source name (unless it IS
            // the destination name — handled by the no-op case).
            ep.removed_entries.insert((src_parent, src_name.to_vec()));
        }
        // The source child is the same inode; its bytes unchanged (a plain
        // rename never rewrites the moved inode; a replaced destination
        // was handled above).
        let env = ep.envelope(&crate::store::epoch::MutationOp::Rename {
            src_parent,
            src_name: src_name.to_vec(),
            dst_parent,
            dst_name: dst_name.to_vec(),
            src_ino: src_entry.ino,
            dst_ino: replaced_dst_ino,
            src_is_dir,
            sp_inode_id,
            dp_inode_id,
            src_child_inode_id: Some(src_child_inode_id),
            dst_child_inode_id: None,
        });
        // Phase-11C: the lock-free pending-op mirror.
        self.epoch_pending
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        records.push(crate::store::transaction::PendingRecord {
            tag: RecordTag::MutationLog,
            payload: env,
            materialized_len: None,
        });
        drop(ep);
        self.epoch_append(records, hooks)?;
        self.maybe_checkpoint_epoch()?;
        Ok(crate::store::RenameOutcome {
            src_ino: src_entry.ino,
            replaced_dst_ino,
        })
    }

    /// Phase-10D epoch write: the 10C parallel chunk preparation against
    /// the epoch's file view, staged as log records + a MutationLog
    /// envelope. The extent/chunk trees are built at the checkpoint.
    ///
    /// Phase-11B: the epoch guard is held ONLY for the overlay reads
    /// (inode + prefill) and the staging — NOT for candidate preparation.
    /// `prepare_write` is pure CPU + committed reads (its inputs are the
    /// pre-filled overlay bytes), so holding the guard across it would
    /// convoy every writer on the single epoch mutex (the measured
    /// 4→16-thread write plateau: 94% of request time waiting on
    /// `epoch_lock_wait`/`epoch_wait`). Same-inode writers are already
    /// serialized by the per-inode mutation lock, and a checkpoint can
    /// only grow this inode's size (it merges this thread's own earlier
    /// pending writes, which the block-A read already includes), so the
    /// size re-read at staging is a monotonicity guard, not a correctness
    /// dependency.
    pub fn epoch_write(
        &self,
        ino: u64,
        offset: u64,
        data: &[u8],
        options: crate::optimizer::policy::OptimizeOptions,
        fg: crate::optimizer::foreground::ForegroundPolicy,
        hooks: &CrashHooks,
    ) -> Result<(), StoreError> {
        if data.is_empty() {
            return Ok(());
        }
        // Phase-11B: the request envelope. Inside a FUSE request this is a
        // pass-through (the handler already opened the envelope); direct
        // callers get their own. The exclusive phases below partition the
        // request so the reconciliation identity (total == sum + residual)
        // can be checked.
        let _req = self.perf.request("epoch_write");
        let _lock = self
            .perf
            .time_request("inode_lock_wait", || self.inode_lock(ino));
        let limits = self.config.limits;
        let chunk_class = limits.chunk_class;
        let end = offset.saturating_add(data.len() as u64);
        let first_chunk = offset / chunk_class;
        let last_chunk = end.div_ceil(chunk_class);
        let prefill_first = first_chunk.saturating_sub(1);

        // Block A (epoch guard held): the overlay view of the file and the
        // PREPARED prefill of the affected chunks — extent collection,
        // dependency enumeration, and the batched object fetch. The decode
        // half runs AFTER the guard drops (Phase-11C): the prepared reads
        // own every object and nested descriptor, so the guard is not held
        // across the materialization (the measured block-A hold at 8–16
        // threads). The read work still attaches to this request through
        // the read-leaf rows (`read_scan`/`read_deps`/`read_prefetch`/
        // `read_decode`) — the prefill IS a read, so wrapping it again
        // would double-count.
        let (old_size, mut overlay, prepared): (
            u64,
            std::collections::BTreeMap<u64, Vec<u8>>,
            Vec<(u64, Option<PreparedRead>)>,
        ) = {
            let ep = self.perf.time_request("epoch_lock_wait", || self.epoch());
            let inode = self
                .get_inode_epoch(&ep, ino)?
                .ok_or_else(|| StoreError::Invariant(format!("inode {ino} missing")))?;
            let old_size = inode.size;
            let mut overlay: std::collections::BTreeMap<u64, Vec<u8>> =
                std::collections::BTreeMap::new();
            let mut prepared: Vec<(u64, Option<PreparedRead>)> = Vec::new();
            // Prefill from the PREVIOUS chunk: prepare_write's in-batch
            // dictionary lookup (the previous same-file chunk) falls back
            // to the committed store on an overlay miss, which would fail
            // for epoch-pending chunks.
            for c in prefill_first..last_chunk {
                let off = c * chunk_class;
                let read_end = (off + chunk_class).min(old_size);
                if read_end > off {
                    prepared.push((
                        off,
                        self.read_file_epoch_prepare(&ep, ino, off, read_end - off)?,
                    ));
                } else {
                    overlay.insert(off, Vec::new());
                }
            }
            (old_size, overlay, prepared)
        }; // the epoch guard drops here — prepare runs WITHOUT it
        // Decode the prefill outside the guard.
        for (off, p) in prepared {
            if let Some(p) = p {
                overlay.insert(off, self.materialize_decode(p)?);
            }
        }
        let new_size = old_size.max(end);

        let mut pending_batch = crate::optimizer::search::PendingBatch::default();
        let (updates, _) = self.perf.time_request("prepare", || {
            self.prepare_write(
                ino,
                offset,
                data,
                Some(&mut overlay),
                Some(&mut pending_batch),
                options,
                fg,
                Some(old_size),
            )
        })?;

        // Block B (guard re-acquired): stage the descriptors + objects +
        // the inode + the mutation-log envelope.
        let mut records: Vec<crate::store::transaction::PendingRecord> = Vec::new();
        let mut chunks: Vec<(u64, crate::core::extent::ChunkId, Vec<u8>)> = Vec::new();
        let mut ep = self.perf.time_request("epoch_lock_wait", || self.epoch());
        // The size is monotone across the guard release: same-inode
        // writers are serialized by `inode_lock`, and a checkpoint only
        // merges this thread's own earlier pending writes (already in the
        // block-A view). Re-read anyway so the invariant is explicit.
        let inode = self
            .get_inode_epoch(&ep, ino)?
            .ok_or_else(|| StoreError::Invariant(format!("inode {ino} missing")))?;
        let new_size = inode.size.max(new_size);
        self.perf.time_request("stage", || {
            for u in &updates {
                let desc_bytes = crate::format::descriptor::encode(&u.descriptor)?;
                for o in &u.objects {
                    let tag = match o.kind {
                        crate::core::candidate::ObjectKind::Data => RecordTag::Data,
                        crate::core::candidate::ObjectKind::Model => RecordTag::Model,
                    };
                    let ml = if tag == RecordTag::Data {
                        Some(u.descriptor.len())
                    } else {
                        None
                    };
                    Self::epoch_stage(&mut ep, self, &mut records, tag, o.payload.clone(), ml);
                }
                chunks.push((u.offset, u.content_id, desc_bytes.clone()));
                ep.pending_extents
                    .insert((ino, u.offset), desc_bytes.clone());
                // The chunk index must never resolve a content id to a
                // descriptor that references the same content id: the dedup
                // path emits EXACT_REF{target: cid} for an already-committed
                // chunk, and registering it in the pending map would let the
                // checkpoint clobber the retained terminal entry with a
                // self-loop (materialize(cid) -> EXACT_REF{cid} forever; the
                // depth cap turns it into DepthExceeded). The self-aliasing
                // extent stays valid — it resolves through the committed
                // terminal. Mirrors `put_chunk_in_tx` and the PendingBatch
                // contract (Phase-10G regression: parallel identical-content
                // writes, e.g. cp -P of duplicated files, hit this).
                let self_aliasing = matches!(
                    u.descriptor,
                    Representation::ExactRef { target, .. } if target == u.content_id
                );
                if !self_aliasing {
                    ep.pending_chunks.entry(u.content_id).or_insert(desc_bytes);
                }
            }
            // The inode + mutation-log envelope are part of the staging
            // work (descriptor/object encoding); keeping them inside this
            // row keeps the reconciliation residual tight.
            let mut fin = inode;
            fin.size = new_size;
            let inode_id = Self::epoch_stage(
                &mut ep,
                self,
                &mut records,
                RecordTag::Inode,
                fin.encode(),
                None,
            );
            ep.pending_inodes.insert(ino, fin);
            let env = ep.envelope(&crate::store::epoch::MutationOp::Write {
                ino,
                size: new_size,
                chunks,
                inode_id,
            });
            // Phase-11C: the lock-free pending-op mirror.
            self.epoch_pending
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            records.push(crate::store::transaction::PendingRecord {
                tag: RecordTag::MutationLog,
                payload: env,
                materialized_len: None,
            });
            Ok::<(), StoreError>(())
        })?;
        drop(ep);
        self.epoch_append(records, hooks)?;
        self.maybe_checkpoint_epoch()?;
        Ok(())
    }

    /// Phase-10D: replay the un-checkpointed mutation log tail at open.
    /// The last checkpoint root is authoritative; envelopes with
    /// `seq > root.log_seq` are the acknowledged-but-unmerged mutations.
    /// Replayed in seq order in ONE transaction (the replayed state is
    /// then committed with the consumed sequence and a durability
    /// barrier, so the mounted state is fully consistent).
    fn epoch_replay(&self) -> Result<(), StoreError> {
        let root = self.current_root();
        let segments = crate::store::segment::list_segments(&self.dir)?;
        let mut log: Vec<(u64, Vec<u8>)> = Vec::new();
        for seq_no in &segments {
            let path = crate::store::segment::segment_path(&self.dir, *seq_no);
            let (records, _) =
                crate::store::segment::scan_segment(&path, self.config.max_records_per_segment)
                    .map_err(|e| StoreError::Io(e.to_string()))?;
            for rec in records {
                if rec.tag == RecordTag::MutationLog {
                    let s = crate::store::epoch::Epoch::envelope_seq(&rec.payload)?;
                    if s > root.log_seq {
                        log.push((s, rec.payload));
                    }
                }
            }
        }
        log.sort_by_key(|(s, _)| *s);
        // Duplicate sequences would imply two envelopes with the same
        // sequence (a store bug); recovery must never silently drop one.
        for w in log.windows(2) {
            if w[0].0 == w[1].0 {
                return Err(StoreError::Invariant(
                    "duplicate mutation log sequence at recovery".into(),
                ));
            }
        }
        if log.is_empty() {
            return Ok(());
        }
        let mut tx = self.begin_tx()?;
        let limits = self.config.limits;
        for (_, env) in &log {
            let (_, op) = crate::store::epoch::Epoch::decode_envelope(env)?;
            self.replay_op(&mut tx, &op, limits)?;
        }
        tx.root_mut().log_seq = log.last().expect("non-empty").0;
        let store = tx.commit_deferred(&CrashHooks::none())?;
        store.durability_barrier(&CrashHooks::none())?;
        Ok(())
    }

    /// Apply one mutation op to a transaction's trees (recovery replay;
    /// also the reference for what a checkpoint's merge produces). The
    /// staged objects resolve through the object index (the log appended
    /// them).
    fn replay_op(
        &self,
        tx: &mut crate::store::transaction::Tx<'_>,
        op: &crate::store::epoch::MutationOp,
        limits: crate::core::limits::Limits,
    ) -> Result<(), StoreError> {
        let fanout = limits.max_fanout;
        let fetch_inode = |tx: &crate::store::transaction::Tx<'_>,
                           id: &crate::core::extent::ChunkId| {
            let bytes = tx.fetch_pending_or_store(id)?.ok_or_else(|| {
                StoreError::Invariant(format!("replay: staged inode object {id} missing"))
            })?;
            Inode::decode(&bytes).map_err(|e| StoreError::Descriptor(e.to_string()))
        };
        match op {
            crate::store::epoch::MutationOp::Create {
                parent,
                name,
                ino,
                d_type,
                inode_id,
                parent_inode_id,
            } => {
                let inode = fetch_inode(tx, inode_id)?;
                Store::put_inode_in_tx(tx, *ino, &inode)?;
                // The parent's FINAL metadata is the log-staged object
                // (mtime/nlink after this op); its dir_root is rebuilt
                // from the tx's current tree + this entry.
                let pmeta = fetch_inode(tx, parent_inode_id)?;
                let pin_cur = Store::inode_for_tx(tx, *parent)?;
                let dir_root = match pin_cur.data {
                    InodeData::Directory { dir_root } => dir_root,
                    _ => return Err(StoreError::Invariant("parent not a directory".into())),
                };
                let new_root = crate::store::directory::insert(
                    dir_root,
                    name,
                    directory::DirEntry {
                        ino: *ino,
                        d_type: *d_type,
                    },
                    BTREE_ORDER,
                    fanout,
                    tx,
                )?;
                let mut pin = pmeta;
                pin.data = InodeData::Directory { dir_root: new_root };
                Store::put_inode_in_tx(tx, *parent, &pin)?;
            }
            crate::store::epoch::MutationOp::Setattr { ino, inode_id } => {
                let inode = fetch_inode(tx, inode_id)?;
                Store::put_inode_in_tx(tx, *ino, &inode)?;
            }
            crate::store::epoch::MutationOp::Unlink {
                parent,
                name,
                child,
                is_dir,
                parent_inode_id,
                child_inode_id,
            } => {
                let pmeta = fetch_inode(tx, parent_inode_id)?;
                let pin_cur = Store::inode_for_tx(tx, *parent)?;
                let dir_root = match pin_cur.data {
                    InodeData::Directory { dir_root } => dir_root,
                    _ => return Err(StoreError::Invariant("parent not a directory".into())),
                };
                let (new_root, _) =
                    crate::store::directory::remove(dir_root, name, BTREE_ORDER, fanout, tx)?;
                let mut pin = pmeta;
                pin.data = InodeData::Directory { dir_root: new_root };
                Store::put_inode_in_tx(tx, *parent, &pin)?;
                match child_inode_id {
                    Some(id) => {
                        let child_inode = fetch_inode(tx, id)?;
                        Store::put_inode_in_tx(tx, *child, &child_inode)?;
                    }
                    None => Store::remove_inode_in_tx(tx, *child)?,
                }
                let _ = is_dir;
            }
            crate::store::epoch::MutationOp::Rename {
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
                dst_child_inode_id: _,
            } => {
                let spmeta = fetch_inode(tx, sp_inode_id)?;
                let dpmeta = fetch_inode(tx, dp_inode_id)?;
                let sp = Store::inode_for_tx(tx, *src_parent)?;
                let src_root = match sp.data {
                    InodeData::Directory { dir_root } => dir_root,
                    _ => return Err(StoreError::Invariant("src parent not a dir".into())),
                };
                let entry =
                    crate::store::directory::lookup(src_root, src_name, BTREE_ORDER, fanout, tx)?
                        .ok_or_else(|| StoreError::Invariant("replay: src entry missing".into()))?;
                if src_parent == dst_parent {
                    let mut root = src_root;
                    if dst_ino.is_some() {
                        root = crate::store::directory::remove(
                            root,
                            dst_name,
                            BTREE_ORDER,
                            fanout,
                            tx,
                        )?
                        .0;
                    }
                    root = crate::store::directory::insert(
                        root,
                        dst_name,
                        entry,
                        BTREE_ORDER,
                        fanout,
                        tx,
                    )?;
                    if src_name != dst_name {
                        root = crate::store::directory::remove(
                            root,
                            src_name,
                            BTREE_ORDER,
                            fanout,
                            tx,
                        )?
                        .0;
                    }
                    let mut pin = spmeta;
                    pin.data = InodeData::Directory { dir_root: root };
                    Store::put_inode_in_tx(tx, *src_parent, &pin)?;
                } else {
                    let dp = Store::inode_for_tx(tx, *dst_parent)?;
                    let mut dst_root = match dp.data {
                        InodeData::Directory { dir_root } => dir_root,
                        _ => return Err(StoreError::Invariant("dst parent not a dir".into())),
                    };
                    if dst_ino.is_some() {
                        dst_root = crate::store::directory::remove(
                            dst_root,
                            dst_name,
                            BTREE_ORDER,
                            fanout,
                            tx,
                        )?
                        .0;
                    }
                    dst_root = crate::store::directory::insert(
                        dst_root,
                        dst_name,
                        entry,
                        BTREE_ORDER,
                        fanout,
                        tx,
                    )?;
                    let src_root = crate::store::directory::remove(
                        src_root,
                        src_name,
                        BTREE_ORDER,
                        fanout,
                        tx,
                    )?
                    .0;
                    let mut pin = spmeta;
                    pin.data = InodeData::Directory { dir_root: src_root };
                    Store::put_inode_in_tx(tx, *src_parent, &pin)?;
                    let mut pin = dpmeta;
                    pin.data = InodeData::Directory { dir_root: dst_root };
                    Store::put_inode_in_tx(tx, *dst_parent, &pin)?;
                }
                // The moved inode's final state.
                match src_child_inode_id {
                    Some(id) => {
                        let child = fetch_inode(tx, id)?;
                        Store::put_inode_in_tx(tx, *src_ino, &child)?;
                    }
                    None => Store::remove_inode_in_tx(tx, *src_ino)?,
                }
                // The replaced destination's final state.
                if let Some(dst_ino) = dst_ino {
                    if *dst_ino != *src_ino {
                        if *src_is_dir {
                            Store::remove_inode_in_tx(tx, *dst_ino)?;
                        } else {
                            let mut t = Store::inode_for_tx(tx, *dst_ino)?;
                            t.nlink = t.nlink.saturating_sub(1);
                            if t.nlink == 0 {
                                Store::remove_inode_in_tx(tx, *dst_ino)?;
                            } else {
                                Store::put_inode_in_tx(tx, *dst_ino, &t)?;
                            }
                        }
                    }
                }
            }
            crate::store::epoch::MutationOp::Write {
                ino,
                size,
                chunks,
                inode_id,
            } => {
                for (off, cid, desc_bytes) in chunks {
                    let rep = crate::format::descriptor::decode(desc_bytes, &limits)?;
                    Store::put_chunk_in_tx(tx, cid, &rep)?;
                    Store::put_extent_in_tx(tx, *ino, *off, &rep)?;
                }
                // The log-staged inode carries the SIZE; its extent_root
                // is stale (the epoch never rebuilt the extent tree), so
                // apply the size to the tx's current inode (whose
                // extent_root the put_extent_in_tx calls just built).
                let fin = fetch_inode(tx, inode_id)?;
                let mut cur = Store::inode_for_tx(tx, *ino)?;
                cur.size = fin.size;
                cur.ctime = fin.ctime;
                Store::put_inode_in_tx(tx, *ino, &cur)?;
                let _ = size;
            }
        }
        Ok(())
    }

    /// Flush the epoch before GC / background optimization / the
    /// durability barrier: the epoch's staged objects are only referenced
    /// by the log, which those walkers do not see as roots.
    pub fn ensure_epoch_flushed(&self, hooks: &CrashHooks) -> Result<(), StoreError> {
        self.epoch_checkpoint(hooks)
    }

    /// Phase-10D size cap: close the epoch when it has accumulated too
    /// many ops (bounded log tail + bounded recovery scope + bounded
    /// memory). Called after each op's log append; the checkpoint merges
    /// the frozen overlay in ONE tree build, so the cap does not fight
    /// the batching win.
    fn maybe_checkpoint_epoch(&self) -> Result<(), StoreError> {
        /// Pending ops per epoch before an automatic close.
        const EPOCH_MAX_OPS: u64 = 1024;
        // Phase-11C: the pending-op count is the lock-free mirror
        // (`epoch.seq − root.log_seq`, maintained under the epoch guard),
        // so the per-write threshold check acquires NO epoch mutex — the
        // acquisition 11B measured as `epoch_wait` (20% of 16-thread
        // request time) was every writer taking the guard at the end of
        // every write just to read the counter. The checkpoint itself,
        // when it fires, reports through its own cp_* rows.
        let pending = self.perf.time_request("epoch_wait", || {
            self.epoch_pending
                .load(std::sync::atomic::Ordering::Relaxed)
        });
        if pending >= EPOCH_MAX_OPS {
            self.epoch_checkpoint(&CrashHooks::none())?;
        }
        Ok(())
    }
}

// Re-exports for the fuse layer.
pub use transaction::{CrashHooks, CrashPoint, Tx};
