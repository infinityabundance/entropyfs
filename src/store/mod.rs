//! The store: crash-consistent persistent immutable object store
//! (ADR-0007/0008). Mounts, recovers, reads, writes, and accounts.

#![forbid(unsafe_code)]

pub mod directory;
pub mod extent_tree;
pub mod gc;
pub mod index;
pub mod inode;
pub mod object;
pub mod recovery;
pub mod root;
pub mod segment;
pub mod snapshot;
pub mod transaction;

use std::collections::HashMap;
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

/// The filesystem store.
pub struct Store {
    dir: PathBuf,
    config: StoreConfig,
    /// In-memory derived object index.
    object_index: ObjectIndex,
    /// Current committed root.
    root: Root,
    /// Current superblock (for the commit flip).
    superblock: Superblock,
    /// Committed generation.
    generation: u64,
    /// Current segment writer (None between commits is not allowed; kept
    /// open from mount).
    current_segment: Option<SegmentWriter>,
    /// Feature bits in use (incompat bits for representations).
    features_in_use: u64,
    /// Statistics.
    stats: StoreStats,
    /// Advisory lock file.
    _lock: File,
    /// Superblock file path.
    superblock_path: PathBuf,
    /// Bounded decoded-model cache (performance only).
    model_cache: std::sync::Mutex<crate::cache::model::ModelCache>,
    /// DSFB storage observer (performance-only; zero decoding authority).
    /// Bounded by `DSFB_MAX_CHUNKS`; dropping it affects only search
    /// ordering, never bytes (ADR-0004).
    dsfb: crate::dsfb::observer::StorageObserver,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("dir", &self.dir)
            .field("generation", &self.generation)
            .field("root", &self.root)
            .finish_non_exhaustive()
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
        // Initial root.  Ino 1 is the filesystem root so FUSE's mount
        // root (always ino 1) maps 1:1 to the store (ADR-0002).
        let root = Root {
            uuid,
            root_dir_ino: 1,
            generation: 0,
            ..Default::default()
        };
        // Initial superblock in slot A (generation 0 is even).
        let sb = Superblock {
            uuid,
            generation: 0,
            segment_seq: 0,
            ..Default::default()
        };
        let sb_path = dir.join("superblock");
        crate::store::root::write_slot(&sb_path, SUPERBLOCK_SLOT_A_OFFSET, &sb, true)?;
        // Root object record lives in segment 0.
        let mut store = Self {
            dir: dir.to_path_buf(),
            config: *config,
            object_index: ObjectIndex::new(),
            root,
            superblock: sb,
            generation: 0,
            current_segment: None,
            features_in_use: 0,
            stats: StoreStats::default(),
            _lock: lock,
            superblock_path: sb_path,
            model_cache: std::sync::Mutex::new(crate::cache::model::ModelCache::new(64)),
            dsfb: crate::dsfb::observer::StorageObserver::default(),
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
        store.stats.physical_capacity = store.physical_capacity();
        Ok(store)
    }

    /// Open (mount) an existing store: recovery + derived index rebuild.
    pub fn open(dir: &Path, config: &StoreConfig) -> Result<Self, StoreError> {
        let lock = open_lock(dir)?;
        let sb_path = dir.join("superblock");
        let pair = SuperblockPair::read(&sb_path)?;
        let sb = pair.choose()?;
        // Rebuild the object index from segments.
        let mut object_index = ObjectIndex::new();
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
        // Load the root object.
        let root_bytes = object_index
            .get(&sb.root_object_id)
            .map(|loc| segment::read_payload(dir, loc.segment_seq, loc.offset, loc.stored_len))
            .transpose()?
            .ok_or_else(|| StoreError::Superblock("root object missing".into()))?;
        let root = Root::decode(&root_bytes)
            .map_err(|e| StoreError::Superblock(format!("root decode: {e:?}")))?;
        if root.generation != sb.generation {
            return Err(StoreError::Superblock(
                "root generation mismatch with superblock".into(),
            ));
        }
        let mut store = Self {
            dir: dir.to_path_buf(),
            config: *config,
            object_index,
            root,
            superblock: sb.clone(),
            generation: sb.generation,
            current_segment: None,
            features_in_use: sb.incompat,
            stats: StoreStats::default(),
            _lock: lock,
            superblock_path: sb_path,
            model_cache: std::sync::Mutex::new(crate::cache::model::ModelCache::new(64)),
            dsfb: crate::dsfb::observer::StorageObserver::default(),
        };
        store.stats.physical_capacity = store.physical_capacity();
        store.open_segment(sb.segment_seq)?;
        // Deep-verify the chosen root quickly (structural).
        recovery::verify_root(&store)?;
        Ok(store)
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

    /// The current committed root.
    pub fn current_root(&self) -> &Root {
        &self.root
    }

    /// The committed generation.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Current segment sequence.
    pub fn current_segment_seq(&self) -> u64 {
        self.current_segment.as_ref().map(|w| w.seq()).unwrap_or(0)
    }

    /// The object index (derived; for GC/fsck).
    pub fn object_index(&self) -> &ObjectIndex {
        &self.object_index
    }

    /// Mutable object index access (GC compaction).
    pub fn object_index_mut(&mut self) -> &mut ObjectIndex {
        &mut self.object_index
    }

    /// The committed stats.
    pub fn stats(&self) -> &StoreStats {
        &self.stats
    }

    /// Mutable stats (fsck/GC update them).
    pub fn stats_mut(&mut self) -> &mut StoreStats {
        &mut self.stats
    }

    /// Feature bits in use.
    pub fn features_in_use(&self) -> u64 {
        self.features_in_use
    }

    /// The DSFB search plan for a chunk (trust-ordered, budget-bounded).
    pub fn dsfb_plan(
        &self,
        key: &crate::dsfb::features::ChunkKey,
    ) -> crate::dsfb::selection::SearchPlan {
        self.dsfb.plan(key)
    }

    /// DSFB trust for one channel of a chunk.
    pub fn dsfb_trust(
        &self,
        key: &crate::dsfb::features::ChunkKey,
        channel: crate::dsfb::features::Channel,
    ) -> f64 {
        self.dsfb.trust(key, channel)
    }

    /// Feed the DSFB observer (performance-only state). Bounded eviction
    /// keeps the observer from growing without limit.
    pub fn dsfb_observe(
        &mut self,
        key: crate::dsfb::features::ChunkKey,
        measurements: &[(crate::dsfb::features::Channel, f64)],
        winner: crate::dsfb::features::Channel,
        outcome_quality: f64,
    ) -> crate::dsfb::drift::Regime {
        let regime = self
            .dsfb
            .observe(key, measurements, winner, outcome_quality);
        if self.dsfb.len() > DSFB_MAX_CHUNKS {
            self.dsfb.evict_one();
        }
        regime
    }

    /// Observer statistics (for `status`).
    pub fn dsfb_stats(&self) -> crate::dsfb::observer::ObserverStats {
        self.dsfb.stats
    }

    /// Materialize the chunk at `offset` of `ino` as a candidate base, but
    /// only when its content id resolves in the chunk index (a future
    /// reader resolves `BaseResidual.base` through the chunk index, so an
    /// unresolvable base would be undecodable). Depth reflects the base
    /// chunk's own reference depth so chains are cost-accounted.
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
        if bytes.len() != len {
            return Ok(None); // shorter than requested: hole/tail at EOF
        }
        let id = crate::core::extent::ChunkId::of(&bytes);
        let Some(desc_bytes) = self.chunk_descriptor(&id)? else {
            return Ok(None);
        };
        let limits = self.config.limits;
        let desc = match crate::format::descriptor::decode(
            &desc_bytes,
            limits.max_descriptor_bytes,
            limits.max_inline_bytes,
            limits.max_palette,
            limits.max_period,
            limits.max_chunk_size,
        ) {
            Ok(d) => d,
            Err(_) => return Ok(None),
        };
        let depth = crate::optimizer::rebase::chain_depth(self, &desc);
        Ok(Some(crate::core::candidate::BaseChunk { id, bytes, depth }))
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

    // ------------------------------------------------------------------
    // Segments
    // ------------------------------------------------------------------

    fn open_segment(&mut self, seq: u64) -> Result<(), StoreError> {
        let w = SegmentWriter::open(&self.dir, seq)?;
        self.current_segment = Some(w);
        Ok(())
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
        if let Some(w) = &self.current_segment {
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
    fn append_records(
        &mut self,
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
                let w = self.current_segment.as_mut().ok_or(StoreError::NotOpen)?;
                let base = w.durable_end() + w.buffered_len();
                if base + encoded.len() as u64 > self.config.segment_size {
                    // Roll: flush + sync current, open the next.  The
                    // borrow of `self.current_segment` ends here (NLL).
                    w.flush()?;
                    w.fdatasync()?;
                    let next = self.current_segment_seq() + 1;
                    self.open_segment(next)?;
                    SegmentWriter::sync_dir(&self.dir)?;
                    base_after_roll(self, &encoded)
                } else {
                    base
                }
            };
            let w = self.current_segment.as_mut().ok_or(StoreError::NotOpen)?;
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
    pub fn fdatasync_segment(&mut self) -> Result<(), StoreError> {
        if let Some(w) = &mut self.current_segment {
            // Flush buffered bytes first (the caller appended them).
            w.flush()?;
            w.fdatasync()?;
        }
        Ok(())
    }

    /// Flush buffered segment bytes.
    pub fn flush_segment(&mut self) -> Result<(), StoreError> {
        if let Some(w) = &mut self.current_segment {
            w.flush()?;
        }
        Ok(())
    }

    /// Ensure the segments directory entries are durable.
    pub fn sync_segments_dir(&self) -> Result<(), StoreError> {
        SegmentWriter::sync_dir(&self.dir)?;
        Ok(())
    }

    /// Fetch an object payload by content id.
    pub fn fetch_object(&self, id: &ChunkId) -> Result<Option<Vec<u8>>, StoreError> {
        match self.object_index.get(id) {
            Some(loc) => Ok(Some(segment::read_payload(
                &self.dir,
                loc.segment_seq,
                loc.offset,
                loc.stored_len,
            )?)),
            None => Ok(None),
        }
    }

    /// Fetch a record payload by location (fsck).
    pub fn read_payload_at(&self, loc: &Location) -> Result<Vec<u8>, StoreError> {
        Ok(segment::read_payload(
            &self.dir,
            loc.segment_seq,
            loc.offset,
            loc.stored_len,
        )?)
    }

    // ------------------------------------------------------------------
    // Superblock / commit
    // ------------------------------------------------------------------

    /// Write the inactive superblock slot for the new root.
    pub fn write_superblock(&mut self, root_id: ChunkId, root: &Root) -> Result<(), StoreError> {
        let mut sb = self.superblock.clone();
        sb.generation = root.generation;
        sb.root_object_id = root_id;
        sb.segment_seq = root.segment_seq;
        sb.incompat = self.features_in_use;
        let offset = match root.generation & 1 {
            0 => SUPERBLOCK_SLOT_A_OFFSET,
            _ => SUPERBLOCK_SLOT_B_OFFSET,
        };
        crate::store::root::write_slot(&self.superblock_path, offset, &sb, false)?;
        self.superblock = sb;
        Ok(())
    }

    /// fsync the superblock file.
    pub fn fsync_superblock(&self) -> Result<(), StoreError> {
        let f = File::open(&self.superblock_path)?;
        f.sync_all()?;
        Ok(())
    }

    /// Publish a committed root to the in-memory state.
    pub fn publish_commit(&mut self, root: &Root, _root_id: ChunkId) -> Result<(), StoreError> {
        self.root = root.clone();
        self.generation = root.generation;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Begin transaction
    // ------------------------------------------------------------------

    /// Begin a write transaction (exclusive; the commit coordinator
    /// serializes these — ADR-0013).
    pub fn begin_tx(&mut self) -> Result<crate::store::transaction::Tx<'_>, StoreError> {
        // Ensure the segment writer is present.
        if self.current_segment.is_none() {
            self.open_segment(self.root.segment_seq)?;
        }
        Ok(crate::store::transaction::Tx::begin(self))
    }

    // ------------------------------------------------------------------
    // Inode index
    // ------------------------------------------------------------------

    /// Look up an inode by number.
    pub fn get_inode(&self, ino: u64) -> Result<Option<Inode>, StoreError> {
        let key = ino.to_be_bytes();
        match index::get(
            self.root.inode_index_root,
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
            self.root.inode_index_root,
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
            self.root.chunk_index_root,
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
        let mut features = tx.store.features_in_use;
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
        // Stash the features on the tx via a side channel: the store's
        // features_in_use is updated at commit. We record in the root's
        // reserved space? Simpler: keep a thread-local-free approach —
        // recompute at commit from the descriptor set. For v1 we update
        // the store field directly through the &mut (commit runs with the
        // store mutably borrowed).
        tx.store.features_in_use |= features;
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
        let mut out = vec![0u8; avail as usize];
        let mut pos = offset;
        let end = offset.saturating_add(avail);
        let extent_root = match &inode.data {
            InodeData::File { extent_root } => *extent_root,
            _ => unreachable!(),
        };
        while pos < end {
            let (start, desc_bytes) = match crate::store::extent_tree::covering(
                extent_root,
                pos,
                BTREE_ORDER,
                self.config.limits.max_fanout,
                self,
            )? {
                Some(x) => x,
                None => {
                    // Hole before the next extent: advance to its start.
                    match crate::store::extent_tree::next_extent_start(
                        extent_root,
                        pos,
                        BTREE_ORDER,
                        self.config.limits.max_fanout,
                        self,
                    )? {
                        Some(next) => {
                            pos = next;
                            continue;
                        }
                        None => break, // hole to EOF
                    }
                }
            };
            let desc = crate::format::descriptor::decode(
                &desc_bytes,
                self.config.limits.max_descriptor_bytes,
                self.config.limits.max_inline_bytes,
                self.config.limits.max_palette,
                self.config.limits.max_period,
                self.config.limits.max_chunk_size,
            )?;
            let extent_end = start.saturating_add(desc.len());
            if extent_end <= pos {
                // Defensive: this extent makes no progress (zero-length or
                // malformed); skip past it instead of looping forever.
                match crate::store::extent_tree::next_extent_start(
                    extent_root,
                    pos,
                    BTREE_ORDER,
                    self.config.limits.max_fanout,
                    self,
                )? {
                    Some(next) => {
                        pos = next;
                        continue;
                    }
                    None => break,
                }
            }
            let mut chunk = vec![0u8; desc.len() as usize];
            let mut budget = self.config.limits.max_decode_work;
            materialize(&desc, self, &self.config.limits, 0, &mut budget, &mut chunk)
                .map_err(|e| StoreError::Descriptor(e.to_string()))?;
            let take_start = pos.max(start);
            let take_end = end.min(extent_end);
            if take_end > take_start {
                let s = (take_start - start) as usize;
                let n = (take_end - take_start) as usize;
                // Output position is absolute (holes precede this extent).
                let o = (take_start - offset) as usize;
                // Defensive bounds: never write past the clip length.
                let n = n.min(avail as usize - o);
                let d = &mut out[o..o + n];
                d.copy_from_slice(&chunk[s..s + n]);
            }
            pos = extent_end;
        }
        Ok(out)
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
            Box::new(crate::rans::residual::RansEncoder),
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

    /// Commit a set of extent updates for a file region (the FUSE write
    /// path entry point after candidate selection).
    pub fn commit_file_extents(
        &mut self,
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
            inode.size = size;
            inode.mtime = crate::store::inode::Timespec::now();
            Store::put_inode_in_tx(&mut tx, ino, &inode)?;
        }
        tx.commit(hooks)?;
        Ok(())
    }

    /// Truncate a file: drop extents starting at or beyond the new size
    /// and re-encode the trailing partial extent so no extent extends past
    /// `new_size` (fsck invariant: extent end <= file size).
    pub fn truncate_file(&mut self, ino: u64, new_size: u64) -> Result<(), StoreError> {
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
                    let desc = crate::format::descriptor::decode(
                        &desc_bytes,
                        limits.max_descriptor_bytes,
                        limits.max_inline_bytes,
                        limits.max_palette,
                        limits.max_period,
                        limits.max_chunk_size,
                    )?;
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
        &mut self,
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
        &mut self,
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
        &mut self,
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
        &mut self,
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
        &mut self,
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
    pub fn link(
        &mut self,
        parent: u64,
        name: &[u8],
        ino: u64,
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<(), StoreError> {
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
    /// the updated inode.
    pub fn setattr_inode(
        &mut self,
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
                self.truncate_file(ino, s)?;
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

impl Store {
    /// Write `data` at `offset` of file `ino` (chunk-aligned
    /// read-modify-write; one transaction; extends the file size).
    pub fn write_region(&mut self, ino: u64, offset: u64, data: &[u8]) -> Result<(), StoreError> {
        self.write_region_with(
            ino,
            offset,
            data,
            crate::optimizer::policy::OptimizeOptions::default(),
        )
    }

    /// Write with explicit optimization options (ablation benchmarks, §43).
    pub fn write_region_with(
        &mut self,
        ino: u64,
        offset: u64,
        data: &[u8],
        options: crate::optimizer::policy::OptimizeOptions,
    ) -> Result<(), StoreError> {
        if data.is_empty() {
            return Ok(());
        }
        let limits = self.config.limits;
        let chunk_class = limits.chunk_class;
        let end = offset.saturating_add(data.len() as u64);
        let first_chunk = offset / chunk_class;
        let last_chunk = end.div_ceil(chunk_class);
        let inode = self
            .get_inode(ino)?
            .ok_or_else(|| StoreError::Invariant(format!("inode {ino} missing")))?;
        let old_size = inode.size;
        let new_size = old_size.max(end);

        let mut updates = Vec::new();
        let mut chunk = first_chunk;
        while chunk < last_chunk {
            let chunk_off = chunk * chunk_class;
            let in_start = offset.max(chunk_off);
            let in_end = end.min(chunk_off + chunk_class);
            let write_start = (in_start - chunk_off) as usize;
            let write_end = (in_end - chunk_off) as usize;
            let payload = &data[(in_start - offset) as usize..(in_end - offset) as usize];

            // Read the current chunk bytes (zeros for holes / beyond EOF),
            // unless the write covers the entire chunk. The whole chunk is
            // read (clipped to the file size) so untouched bytes survive.
            let full_chunk = write_start == 0 && write_end == chunk_class as usize;
            let mut chunk_bytes = vec![0u8; chunk_class as usize];
            if !full_chunk {
                let read_end = (chunk_off + chunk_class).min(old_size);
                if read_end > chunk_off {
                    let partial = self.read_file(ino, chunk_off, read_end - chunk_off)?;
                    let n = partial.len().min(chunk_class as usize);
                    chunk_bytes[..n].copy_from_slice(&partial[..n]);
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

            // Phase 4: the guided search (DSFB-ordered, §16). P0 is the
            // previous version of this chunk (the natural edit base for
            // versioned data, H2); it is only usable when the old extent
            // resolves in the chunk index.
            let mut prev_version = if old_size > chunk_off {
                self.base_chunk_at(ino, chunk_off, chunk_len)?
            } else {
                None
            };
            // Rebase-on-write (§11): drift workloads edit the same chunk
            // repeatedly, and each edit would otherwise nest another
            // BaseResidual until the depth cap collapses the strategy to
            // RAW. When the previous version is itself a deep chain,
            // re-encode it at depth 0 in the same transaction (the flat
            // extent update lands first; the edit's update replaces it),
            // so the new base+residual stays shallow and decodable.
            let mut flatten_updates: Vec<ExtentUpdate> = Vec::new();
            if let Some(p) = &prev_version {
                if p.depth >= crate::optimizer::rebase::REBASE_DEPTH_THRESHOLD {
                    let policy = self.config.policy;
                    let flat = Store::encode_chunk(&p.bytes, chunk_off, p.id, &limits, &policy)?;
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
                offset: chunk_off,
                target: chunk_bytes,
                prev_version,
                mode: crate::optimizer::search::SearchMode::Foreground,
            };
            let outcome = crate::optimizer::search::encode_guided(self, &ctx, options)?;
            updates.extend(flatten_updates);
            updates.push(outcome.update);
            chunk += 1;
        }

        self.commit_file_extents(ino, updates, Some(new_size), &CrashHooks::none())?;
        Ok(())
    }

    /// Punch a hole: the byte range reads as ZERO (and is stored as ZERO
    /// descriptors, so space is freed). When the punch reaches EOF and
    /// `keep_size` is clear, the file is truncated instead.
    pub fn punch_hole(
        &mut self,
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
            return self.truncate_file(ino, start);
        }
        let punch_end = end.min(size);
        if punch_end > start {
            let zeros = vec![0u8; (punch_end - start) as usize];
            self.write_region(ino, start, &zeros)?;
        }
        Ok(())
    }

    /// `copy_file_range`: copy `len` bytes between files (v1 reads through
    /// the materialization path and writes through the RMW path — correct,
    /// not zero-copy). Returns the number of bytes copied.
    pub fn copy_range(
        &mut self,
        ino_in: u64,
        offset_in: u64,
        ino_out: u64,
        offset_out: u64,
        len: u64,
    ) -> Result<u64, StoreError> {
        let data = self.read_file(ino_in, offset_in, len)?;
        let copied = data.len() as u64;
        if copied > 0 {
            self.write_region(ino_out, offset_out, &data)?;
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
                    if let Ok(d) = crate::format::descriptor::decode(
                        &bytes,
                        limits.max_descriptor_bytes,
                        limits.max_inline_bytes,
                        limits.max_palette,
                        limits.max_period,
                        limits.max_chunk_size,
                    ) {
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
        &mut self,
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

    /// Get an xattr value (raw bytes; `None` when absent).
    pub fn get_xattr(&self, ino: u64, name: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
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

    /// Set an xattr (insert or replace).
    pub fn set_xattr(
        &mut self,
        ino: u64,
        name: &[u8],
        value: &[u8],
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<(), StoreError> {
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

    /// Remove an xattr; returns whether it existed.
    pub fn remove_xattr(
        &mut self,
        ino: u64,
        name: &[u8],
        hooks: &crate::store::transaction::CrashHooks,
    ) -> Result<bool, StoreError> {
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

    /// List xattr names.
    pub fn list_xattr(&self, ino: u64) -> Result<Vec<Vec<u8>>, StoreError> {
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
fn base_after_roll(store: &mut Store, encoded: &[u8]) -> u64 {
    let w = store.current_segment.as_ref().expect("segment open");
    let base = w.durable_end();
    debug_assert!(base + encoded.len() as u64 <= store.config.segment_size);
    base
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
            Some(bytes) => crate::format::descriptor::decode(
                &bytes,
                self.config.limits.max_descriptor_bytes,
                self.config.limits.max_inline_bytes,
                self.config.limits.max_palette,
                self.config.limits.max_period,
                self.config.limits.max_chunk_size,
            )
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

// Re-exports for the fuse layer.
pub use transaction::{CrashHooks, CrashPoint, Tx};

// Keep HashMap import used (public API surface for stats accounting).
#[allow(unused_imports)]
use HashMap as _HashMap;
