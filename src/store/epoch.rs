//! Phase-10D metadata writeback epochs: the recoverable dirty state
//! between checkpoints (ADR pending, `docs/architecture/transaction-model.md`).
//!
//! The foreground write path accumulates acknowledged namespace/writeback
//! mutations in an ACTIVE EPOCH instead of committing one immutable
//! transaction per operation. Each op's data objects (inodes, model/enc
//! payloads) are appended to the append-only store as ordinary records and
//! a small `MutationLog` ENVELOPE records the op — the recoverable dirty
//! state. The root and superblock are untouched per op: the committed
//! trees still describe the last CHECKPOINT, and the envelope sequence
//! numbers order the pending mutations on top of it.
//!
//! On checkpoint (size / time / fsync / syncfs / unmount / pressure), the
//! frozen overlay is merged into the immutable trees ONCE — bulk-load for
//! the small per-directory trees, `apply_sorted_batch` (bulk COW) for the
//! global inode index and the chunk index — and ONE root publication
//! carries the merged state plus the consumed log sequence.
//!
//! Recovery replays every `MutationLog` envelope with
//! `seq > root.log_seq` (in seq order) against the checkpoint root, so an
//! acknowledged op survives a process crash exactly as the deferred-commit
//! path always guaranteed: the envelope and its objects were flushed to
//! the segment's page cache before the ack. Power loss is bounded by the
//! same [`Store::durability_barrier`] contract as every other commit.

#![forbid(unsafe_code)]

use crate::core::extent::ChunkId;
use crate::format::codec::{CodecError, Reader, Writer};
use crate::store::StoreError;
use crate::store::directory::DirEntry;
use crate::store::inode::Inode;

/// One acknowledged mutation, persisted as a `MutationLog` record.
///
/// The op's DATA objects (inode bytes, model/enc payloads) are staged as
/// ordinary records with content-addressed ids; the envelope references
/// them by id (or embeds the small descriptor bytes the checkpoint trees
/// need as values). Replay rebuilds the op's effect from the envelope +
/// the staged objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationOp {
    /// Create a new entry (file/dir/symlink/device): a fresh inode object
    /// plus a directory entry under `parent`.
    Create {
        /// Parent directory inode.
        parent: u64,
        /// Entry name.
        name: Vec<u8>,
        /// Fresh inode number.
        ino: u64,
        /// `d_type` of the new entry.
        d_type: u8,
        /// The new inode's object id (staged as an `Inode` record).
        inode_id: ChunkId,
        /// The parent inode's object id AFTER the mtime/nlink update
        /// (staged as an `Inode` record).
        parent_inode_id: ChunkId,
    },
    /// Attribute update (mode/uid/gid/times/size). A size change also
    /// truncates (extents at or beyond the new size are dropped).
    Setattr {
        /// Target inode.
        ino: u64,
        /// The updated inode's object id.
        inode_id: ChunkId,
    },
    /// Remove an entry. The child inode is either updated (nlink-1,
    /// staged object) or removed outright (nlink reached zero / rmdir).
    Unlink {
        /// Parent directory inode.
        parent: u64,
        /// Entry name.
        name: Vec<u8>,
        /// The removed entry's inode.
        child: u64,
        /// rmdir semantics.
        is_dir: bool,
        /// The parent inode's object id after the update.
        parent_inode_id: ChunkId,
        /// The child inode's object id after the update (`None` when the
        /// child was removed).
        child_inode_id: Option<ChunkId>,
    },
    /// Rename between (possibly equal) parents, POSIX type rules applied.
    Rename {
        /// Source parent.
        src_parent: u64,
        /// Source name.
        src_name: Vec<u8>,
        /// Destination parent.
        dst_parent: u64,
        /// Destination name.
        dst_name: Vec<u8>,
        /// The moved entry's inode.
        src_ino: u64,
        /// A replaced destination's inode, if any.
        dst_ino: Option<u64>,
        /// Whether the moved entry is a directory.
        src_is_dir: bool,
        /// Source parent inode object id after the update.
        sp_inode_id: ChunkId,
        /// Destination parent inode object id after the update.
        dp_inode_id: ChunkId,
        /// Source child inode object id after the update (`None` = the
        /// child was dropped on replace).
        src_child_inode_id: Option<ChunkId>,
        /// Replaced destination child inode object id after the update
        /// (`None` = no replace, or the replaced inode was dropped).
        dst_child_inode_id: Option<ChunkId>,
    },
    /// Data write: chunk descriptors + the inode's size update. The
    /// descriptors become chunk-index values and extent-tree values at the
    /// checkpoint; the referenced model/enc objects are staged records.
    Write {
        /// Target inode.
        ino: u64,
        /// File size after this write.
        size: u64,
        /// (offset, content id, descriptor bytes) per touched chunk, in
        /// offset order (the extent entries; also the chunk-index
        /// entries).
        chunks: Vec<(u64, ChunkId, Vec<u8>)>,
        /// The updated inode's object id.
        inode_id: ChunkId,
    },
}

/// Op tags (persisted in the envelope).
const OP_CREATE: u8 = 0x01;
const OP_SETATTR: u8 = 0x02;
const OP_UNLINK: u8 = 0x03;
const OP_RENAME: u8 = 0x04;
const OP_WRITE: u8 = 0x05;

impl MutationOp {
    /// Encode the op body (without the sequence number).
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        match self {
            MutationOp::Create {
                parent,
                name,
                ino,
                d_type,
                inode_id,
                parent_inode_id,
            } => {
                w.u8(OP_CREATE);
                w.u64(*parent);
                w.bytes16(name).expect("name fits u16");
                w.u64(*ino);
                w.u8(*d_type);
                w.bytes(inode_id.as_bytes());
                w.bytes(parent_inode_id.as_bytes());
            }
            MutationOp::Setattr { ino, inode_id } => {
                w.u8(OP_SETATTR);
                w.u64(*ino);
                w.bytes(inode_id.as_bytes());
            }
            MutationOp::Unlink {
                parent,
                name,
                child,
                is_dir,
                parent_inode_id,
                child_inode_id,
            } => {
                w.u8(OP_UNLINK);
                w.u64(*parent);
                w.bytes16(name).expect("name fits u16");
                w.u64(*child);
                w.u8(*is_dir as u8);
                w.bytes(parent_inode_id.as_bytes());
                match child_inode_id {
                    Some(id) => {
                        w.u8(1);
                        w.bytes(id.as_bytes());
                    }
                    None => w.u8(0),
                }
            }
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
            } => {
                w.u8(OP_RENAME);
                w.u64(*src_parent);
                w.bytes16(src_name).expect("name fits u16");
                w.u64(*dst_parent);
                w.bytes16(dst_name).expect("name fits u16");
                w.u64(*src_ino);
                match dst_ino {
                    Some(i) => {
                        w.u8(1);
                        w.u64(*i);
                    }
                    None => w.u8(0),
                }
                w.u8(*src_is_dir as u8);
                w.bytes(sp_inode_id.as_bytes());
                w.bytes(dp_inode_id.as_bytes());
                match src_child_inode_id {
                    Some(id) => {
                        w.u8(1);
                        w.bytes(id.as_bytes());
                    }
                    None => w.u8(0),
                }
                match dst_child_inode_id {
                    Some(id) => {
                        w.u8(1);
                        w.bytes(id.as_bytes());
                    }
                    None => w.u8(0),
                }
            }
            MutationOp::Write {
                ino,
                size,
                chunks,
                inode_id,
            } => {
                w.u8(OP_WRITE);
                w.u64(*ino);
                w.u64(*size);
                w.u32(chunks.len() as u32);
                for (off, cid, desc) in chunks {
                    w.u64(*off);
                    w.bytes(cid.as_bytes());
                    w.bytes32(desc).expect("descriptor fits u32");
                }
                w.bytes(inode_id.as_bytes());
            }
        }
        w.into_bytes()
    }

    /// Decode an op body (the sequence number is read by the caller).
    pub fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
        let mut r = Reader::new(bytes);
        let op = r.u8()?;
        Ok(match op {
            OP_CREATE => {
                let parent = r.u64()?;
                let name = r.bytes16()?.to_vec();
                let ino = r.u64()?;
                let d_type = r.u8()?;
                let inode_id = read_id(&mut r)?;
                let parent_inode_id = read_id(&mut r)?;
                MutationOp::Create {
                    parent,
                    name,
                    ino,
                    d_type,
                    inode_id,
                    parent_inode_id,
                }
            }
            OP_SETATTR => {
                let ino = r.u64()?;
                let inode_id = read_id(&mut r)?;
                MutationOp::Setattr { ino, inode_id }
            }
            OP_UNLINK => {
                let parent = r.u64()?;
                let name = r.bytes16()?.to_vec();
                let child = r.u64()?;
                let is_dir = r.u8()? != 0;
                let parent_inode_id = read_id(&mut r)?;
                let child_inode_id = if r.u8()? != 0 {
                    Some(read_id(&mut r)?)
                } else {
                    None
                };
                MutationOp::Unlink {
                    parent,
                    name,
                    child,
                    is_dir,
                    parent_inode_id,
                    child_inode_id,
                }
            }
            OP_RENAME => {
                let src_parent = r.u64()?;
                let src_name = r.bytes16()?.to_vec();
                let dst_parent = r.u64()?;
                let dst_name = r.bytes16()?.to_vec();
                let src_ino = r.u64()?;
                let dst_ino = if r.u8()? != 0 { Some(r.u64()?) } else { None };
                let src_is_dir = r.u8()? != 0;
                let sp_inode_id = read_id(&mut r)?;
                let dp_inode_id = read_id(&mut r)?;
                let src_child_inode_id = if r.u8()? != 0 {
                    Some(read_id(&mut r)?)
                } else {
                    None
                };
                let dst_child_inode_id = if r.u8()? != 0 {
                    Some(read_id(&mut r)?)
                } else {
                    None
                };
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
                }
            }
            OP_WRITE => {
                let ino = r.u64()?;
                let size = r.u64()?;
                let n = r.u32()? as usize;
                if n > 1 << 20 {
                    return Err(CodecError::Malformed);
                }
                let mut chunks = Vec::with_capacity(n);
                for _ in 0..n {
                    let off = r.u64()?;
                    let cid = read_id(&mut r)?;
                    let desc = r.bytes32()?.to_vec();
                    chunks.push((off, cid, desc));
                }
                let inode_id = read_id(&mut r)?;
                MutationOp::Write {
                    ino,
                    size,
                    chunks,
                    inode_id,
                }
            }
            _ => return Err(CodecError::Malformed),
        })
    }
}

fn read_id(r: &mut Reader<'_>) -> Result<ChunkId, CodecError> {
    Ok(ChunkId::new(r.take(32)?.try_into().unwrap()))
}

/// A `DecoderContext` that serves PREFETCHED objects (Phase-10F
/// `read_many`: one submission queue for a materialization's model/stream/
/// dictionary/descriptor dependencies) and resolves chunk descriptors
/// through the ACTIVE EPOCH first (the committed chunk index second).
///
/// Objects not in the prefetch map fall back to the store — the batch is
/// an optimization, never a correctness dependency. The epoch overlay is
/// optional: the committed read path passes `None`; the FUSE read path
/// passes the active epoch so pending descriptors (in-batch SequenceDict
/// chains, EXACT_REF aliases) resolve before the committed index.
pub struct PrefetchContext<'a> {
    store: &'a crate::store::Store,
    objects: &'a std::collections::HashMap<crate::core::extent::ChunkId, Vec<u8>>,
    ep: Option<&'a Epoch>,
}

impl<'a> PrefetchContext<'a> {
    /// Build the prefetch context.
    pub fn new(
        store: &'a crate::store::Store,
        objects: &'a std::collections::HashMap<crate::core::extent::ChunkId, Vec<u8>>,
        ep: Option<&'a Epoch>,
    ) -> Self {
        Self { store, objects, ep }
    }
}

impl crate::core::materialize::DecoderContext for PrefetchContext<'_> {
    fn fetch_object(
        &self,
        id: &crate::core::extent::ChunkId,
    ) -> Result<Vec<u8>, crate::core::materialize::MaterializeError> {
        if let Some(b) = self.objects.get(id) {
            return Ok(b.clone());
        }
        self.store.fetch_object_impl(id)
    }

    fn fetch_descriptor(
        &self,
        id: &crate::core::extent::ChunkId,
    ) -> Result<
        crate::core::representation::Representation,
        crate::core::materialize::MaterializeError,
    > {
        if let Some(bytes) = self.ep.and_then(|e| e.overlay_chunk(id)) {
            let limits = *self.store.limits();
            return crate::format::descriptor::decode(
                &bytes,
                limits.max_descriptor_bytes,
                limits.max_inline_bytes,
                limits.max_palette,
                limits.max_period,
                limits.max_chunk_size,
            )
            .map_err(|e| {
                crate::core::materialize::MaterializeError::InvalidDescriptor(e.to_string())
            });
        }
        self.store.fetch_descriptor(id)
    }

    fn decode_rans(
        &self,
        model: &[u8],
        encoded: &[u8],
        scale_bits: u8,
        codec: crate::core::representation::RansCodec,
        out_len: u64,
    ) -> Result<Vec<u8>, crate::core::materialize::MaterializeError> {
        self.store
            .decode_rans(model, encoded, scale_bits, codec, out_len)
    }

    fn universe_bytes(
        &self,
        universe: crate::core::representation::UniverseId,
        seed: [u8; 16],
        coordinate: u64,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>, crate::core::materialize::MaterializeError> {
        self.store.universe_bytes(universe, seed, coordinate, range)
    }
}

/// The active epoch: pending namespace/writeback state shadowing the
/// committed trees, plus the log sequence and the staged-object dedup set.
///
/// The overlay is the READ view (a later op — and the FUSE read path —
/// sees earlier epoch mutations) and the CHECKPOINT source (the frozen
/// overlay is merged into the trees once).
#[derive(Debug, Default)]
pub struct Epoch {
    /// Next log sequence number (1-based; the checkpoint root records the
    /// highest consumed sequence).
    pub seq: u64,
    /// ino -> final inode (creates, setattrs, parent updates).
    pub pending_inodes: std::collections::BTreeMap<u64, Inode>,
    /// ino dropped outright (unlink of the last link, rmdir).
    pub removed_inodes: std::collections::BTreeSet<u64>,
    /// (parent, name) -> entry (creates, rename destinations).
    pub pending_entries: std::collections::BTreeMap<(u64, Vec<u8>), DirEntry>,
    /// (parent, name) removed (unlinks, rename sources).
    pub removed_entries: std::collections::BTreeSet<(u64, Vec<u8>)>,
    /// (ino, offset) -> descriptor bytes (writes).
    pub pending_extents: std::collections::BTreeMap<(u64, u64), Vec<u8>>,
    /// cid -> descriptor bytes (writes; the chunk-index pending entries).
    pub pending_chunks: std::collections::BTreeMap<ChunkId, Vec<u8>>,
    /// Object ids already appended by earlier epoch ops (per-epoch dedup
    /// for the staged-object records).
    pub staged_objects: std::collections::HashSet<ChunkId>,
    /// Whether the MutationLog incompat feature bit has been persisted.
    pub feature_persisted: bool,
    /// Highest inode number known (committed max when 0 and unset; the
    /// epoch's allocator is monotonic and cached here to avoid a full
    /// inode-index scan per create).
    pub max_ino: u64,
}

impl Epoch {
    /// Whether the epoch has any pending state.
    pub fn is_empty(&self) -> bool {
        self.seq == 0
            && self.pending_inodes.is_empty()
            && self.removed_inodes.is_empty()
            && self.pending_entries.is_empty()
            && self.removed_entries.is_empty()
            && self.pending_extents.is_empty()
            && self.pending_chunks.is_empty()
    }

    /// Register an object id as staged (dedup for the record append).
    pub fn mark_staged(&mut self, id: ChunkId) {
        self.staged_objects.insert(id);
    }

    /// Whether the object id was already staged by this epoch.
    pub fn is_staged(&self, id: &ChunkId) -> bool {
        self.staged_objects.contains(id)
    }

    /// Overlay-aware inode read: pending, else removed, else `None`.
    pub fn overlay_inode(&self, ino: u64, committed: Option<Inode>) -> Option<Inode> {
        if self.removed_inodes.contains(&ino) {
            return None;
        }
        if let Some(i) = self.pending_inodes.get(&ino) {
            return Some(i.clone());
        }
        committed
    }

    /// Overlay-aware directory lookup: removals first (a key removed in
    /// the epoch shadows any pending or committed entry), then pending
    /// entries, then `None` (the caller consults the committed tree).
    pub fn overlay_entry(&self, parent: u64, name: &[u8]) -> Option<DirEntry> {
        if self.removed_entries.contains(&(parent, name.to_vec())) {
            return None;
        }
        if let Some(e) = self.pending_entries.get(&(parent, name.to_vec())) {
            return Some(*e);
        }
        None
    }

    /// Overlay-aware chunk descriptor: pending first, else `None` (the
    /// caller falls back to the committed chunk index).
    pub fn overlay_chunk(&self, cid: &ChunkId) -> Option<Vec<u8>> {
        self.pending_chunks.get(cid).cloned()
    }

    /// Overlay-aware extent descriptor at `(ino, offset)`: pending first,
    /// else `None` (the caller falls back to the committed extent tree).
    pub fn overlay_extent(&self, ino: u64, offset: u64) -> Option<Vec<u8>> {
        self.pending_extents.get(&(ino, offset)).cloned()
    }

    /// Encode a `MutationOp` into a `MutationLog` envelope payload with
    /// the next sequence number (and bump it).
    pub fn envelope(&mut self, op: &MutationOp) -> Vec<u8> {
        self.seq += 1;
        let mut w = Writer::new();
        w.u64(self.seq);
        w.bytes(&op.encode());
        w.into_bytes()
    }

    /// Recover the sequence number from an envelope payload.
    pub fn envelope_seq(bytes: &[u8]) -> Result<u64, StoreError> {
        let mut r = Reader::new(bytes);
        let seq = r.u64().map_err(|e| StoreError::Descriptor(e.to_string()))?;
        Ok(seq)
    }

    /// Decode an envelope payload (sequence + op).
    pub fn decode_envelope(bytes: &[u8]) -> Result<(u64, MutationOp), StoreError> {
        let mut r = Reader::new(bytes);
        let seq = r.u64().map_err(|e| StoreError::Descriptor(e.to_string()))?;
        let op = MutationOp::decode(&bytes[r.pos()..])
            .map_err(|e| StoreError::Descriptor(e.to_string()))?;
        Ok((seq, op))
    }
}
