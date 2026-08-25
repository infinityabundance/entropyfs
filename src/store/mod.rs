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
    /// Store is full (ENOSPC equivalent).
    Full,
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
        }
    }
}

/// B-tree fanout (order).
pub const BTREE_ORDER: u16 = 64;

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
        // Initial root.
        let root = Root {
            uuid,
            root_dir_ino: 2,
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
        };
        store.open_segment(0)?;
        // Create the root directory inode (ino 2) and commit the initial
        // root through the normal transaction protocol, so the store is
        // mountable (verify_root requires the root dir inode to exist).
        {
            let mut tx = store.begin_tx()?;
            let root_inode = Inode::new_dir(0, 0, 0o755);
            Store::put_inode_in_tx(&mut tx, 2, &root_inode)?;
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

    // ------------------------------------------------------------------
    // Segments
    // ------------------------------------------------------------------

    fn open_segment(&mut self, seq: u64) -> Result<(), StoreError> {
        let w = SegmentWriter::open(&self.dir, seq)?;
        self.current_segment = Some(w);
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

    /// Physical capacity of the backing directory (statfs basis).
    pub fn physical_capacity(&self) -> u64 {
        // Best-effort statvfs of the store dir.
        let md = std::fs::metadata(&self.dir);
        if let Ok(md) = md {
            let _ = md;
        }
        // Use the segments dir's filesystem via statvfs through rustix.
        use rustix::fs::statvfs;
        match statvfs(&self.dir) {
            Ok(s) => s.f_blocks.saturating_mul(s.f_frsize),
            Err(_) => 0,
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

    /// Allocate a fresh inode number (monotonic; simple for v1 — the max
    /// ino + 1, found by scanning; the fuse layer caches the counter).
    pub fn alloc_ino(&self) -> Result<u64, StoreError> {
        let inodes = self.all_inodes()?;
        Ok(inodes.iter().copied().max().unwrap_or(1) + 1)
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
        let parsed = crate::rans::metadata::decode_model(model, self.config.limits.max_model_bytes)
            .map_err(|e| MaterializeError::RansDecode(e.to_string()))?;
        if parsed.scale_bits != scale_bits || parsed.codec != codec {
            return Err(MaterializeError::RansDecode("model tag mismatch".into()));
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
    /// Internal fetch helper for the DecoderContext impl.
    fn fetch_object_impl(&self, id: &ChunkId) -> Result<Vec<u8>, MaterializeError> {
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
