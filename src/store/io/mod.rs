//! Storage transport abstraction (Phase 10F, ADR-0021).
//!
//! `Store` / transactions / epoch checkpoint sit above an `IoBackend`:
//!
//! ```text
//! Store / transactions / epoch checkpoint
//!                  │
//!                  ▼
//!               IoBackend
//!              /         \
//!         SyncIo           UringIo
//!      reference path    performance path
//! ```
//!
//! - [`sync::SyncIo`] is the pre-10F synchronous engine, preserved
//!   byte-for-byte as the crash-consistency oracle (the default).
//! - [`uring::UringIo`] implements the same record format and the exact
//!   same durability ordering with the syscalls issued through an
//!   io_uring ring.
//!
//! Every backend call completes its durability work before returning, so
//! the store's orchestration — and its crash-court injection points
//! between calls (`CrashPoint`) — is unchanged. The acceptance test for
//! `UringIo` is crash-court parity: at every injection point the store
//! directory must be byte-identical to the `SyncIo` state, and recovery
//! must produce the same admissible state.

#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::Arc;

use crate::format::version::SEGMENT_MAGIC;
use crate::store::StoreError;
use crate::store::segment::SegmentError;

pub mod sync;
pub mod uring;

/// Which transport the store uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoBackendKind {
    /// The reference synchronous engine (`SyncIo`; the crash-consistency
    /// oracle). Default.
    Sync,
    /// The io_uring performance path (`UringIo`).
    Uring,
}

impl IoBackendKind {
    /// Every backend kind (drives the crash-court parity matrix).
    pub const ALL: [IoBackendKind; 2] = [IoBackendKind::Sync, IoBackendKind::Uring];

    /// Parse a CLI value (`sync` | `uring`).
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "sync" => Ok(IoBackendKind::Sync),
            "uring" => Ok(IoBackendKind::Uring),
            other => Err(format!(
                "unknown --io-backend {other:?} (expected sync | uring)"
            )),
        }
    }

    /// Canonical name.
    pub fn name(self) -> &'static str {
        match self {
            IoBackendKind::Sync => "sync",
            IoBackendKind::Uring => "uring",
        }
    }
}

/// One payload read request for [`IoBackend::read_many`]: the record
/// payload at `(segment_seq, offset)` with `stored_len` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadRequest {
    /// Segment sequence number.
    pub segment_seq: u64,
    /// Record start offset within the segment (the payload begins at
    /// `offset + RECORD_HEADER_SIZE`).
    pub offset: u64,
    /// Stored payload length.
    pub stored_len: u64,
}

/// The storage transport. Every method has the same semantics in both
/// implementations; the difference is only *how* the syscalls are issued.
pub trait IoBackend: Send + Sync {
    /// Backend kind.
    fn kind(&self) -> IoBackendKind;

    /// Canonical name (`sync` | `uring`).
    fn name(&self) -> &'static str;

    // --- segment lifecycle -------------------------------------------------

    /// Open (create if needed) the segment file for append; returns the
    /// durable end offset after torn-tail truncation (`SegmentWriter::open`
    /// semantics: a fresh segment gets the magic header made durable; an
    /// existing segment gets its torn tail truncated).
    fn open_segment(&self, seq: u64) -> Result<u64, StoreError>;

    /// Current length of the segment file (0 when absent).
    fn segment_len(&self, seq: u64) -> Result<u64, StoreError>;

    /// Read the whole segment file (open-time torn-tail scan; also used by
    /// the parity harness). Empty when absent.
    fn read_segment_file(&self, seq: u64) -> Result<Vec<u8>, StoreError>;

    /// Write bytes at an absolute offset (pwrite semantics; the segment fd
    /// is created on demand and cached). Completes when the write has been
    /// accepted by the kernel page cache.
    fn write_at(&self, seq: u64, offset: u64, bytes: &[u8]) -> Result<(), StoreError>;

    /// Truncate the segment file (torn-tail removal at open).
    fn truncate_segment(&self, seq: u64, len: u64) -> Result<(), StoreError>;

    /// Full fsync of the segment file (fresh-magic durability).
    fn sync_segment_file(&self, seq: u64) -> Result<(), StoreError>;

    /// fdatasync of the segment file (record durability).
    fn fdatasync_segment(&self, seq: u64) -> Result<(), StoreError>;

    /// fsync of the segments directory (new segment directory entries).
    fn sync_segments_dir(&self) -> Result<(), StoreError>;

    /// Delete a segment file (GC; only after the new root is durable) and
    /// drop its cached handle.
    fn delete_segment(&self, seq: u64) -> Result<(), StoreError>;

    // --- payload reads -----------------------------------------------------

    /// Read one record payload.
    fn read_payload(&self, seq: u64, offset: u64, stored_len: u64) -> Result<Vec<u8>, StoreError>;

    /// Read many record payloads. For `UringIo` this is ONE submission
    /// queue; for `SyncIo` it is the reference sequential path. Results are
    /// returned in request order; the i-th result corresponds to the i-th
    /// request.
    fn read_many(&self, reqs: &[ReadRequest]) -> Vec<Result<Vec<u8>, StoreError>>;

    // --- superblock --------------------------------------------------------

    /// Write one superblock slot (page cache; fsync at the barrier).
    fn write_superblock_slot(&self, offset: u64, slot: &[u8]) -> Result<(), StoreError>;

    /// fsync the superblock file (commit durable).
    fn fsync_superblock(&self) -> Result<(), StoreError>;
}

/// Build the backend for a store directory (mkfs / mount).
pub fn build_backend(
    kind: IoBackendKind,
    dir: &Path,
    uring_entries: u32,
) -> Result<Arc<dyn IoBackend>, StoreError> {
    match kind {
        IoBackendKind::Sync => Ok(Arc::new(sync::SyncIo::new(dir))),
        IoBackendKind::Uring => Ok(Arc::new(uring::UringIo::new(dir, uring_entries)?)),
    }
}

/// Backend-agnostic `open_segment`: create (magic, made durable) or
/// validate + truncate the torn tail. Both backends share this; the
/// primitives (`segment_len`, `write_at`, `truncate_segment`,
/// `sync_segment_file`) are backend-specific.
pub(crate) fn open_segment_common(io: &dyn IoBackend, seq: u64) -> Result<u64, StoreError> {
    let len = io.segment_len(seq)?;
    if len == 0 {
        // Fresh segment: write the magic header and make it durable
        // (existing semantics: sync_all before returning).
        io.write_at(seq, 0, &SEGMENT_MAGIC)?;
        io.sync_segment_file(seq)?;
        return Ok(4);
    }
    let bytes = io.read_segment_file(seq)?;
    if bytes.len() < 4 {
        // Torn magic (a crash mid-magic-write): malformed segment. The
        // durability ordering makes this unreachable in practice (the dir
        // entry is synced only after open returns), but it must never be
        // silently treated as valid.
        return Err(SegmentError::Malformed.into());
    }
    if bytes[..4] != SEGMENT_MAGIC {
        return Err(SegmentError::Malformed.into());
    }
    let clean = find_clean_end_bytes(&bytes)?;
    if clean < len {
        // Truncate the torn tail so new appends never follow garbage, then
        // make the truncation durable (existing semantics: sync_data).
        io.truncate_segment(seq, clean)?;
        io.fdatasync_segment(seq)?;
    }
    Ok(io.segment_len(seq)?.max(4))
}

/// Find the last clean record boundary in segment bytes (the offset at
/// which sequential record validation first fails, or EOF). The pure
/// version of `segment::find_clean_end`, shared by both backends.
pub(crate) fn find_clean_end_bytes(bytes: &[u8]) -> Result<u64, SegmentError> {
    if bytes.len() < 4 {
        return Ok(bytes.len() as u64);
    }
    let mut offset = 4u64;
    while offset < bytes.len() as u64 {
        match crate::format::record::decode(bytes, offset) {
            Ok(Some(rec)) => {
                offset = offset
                    .checked_add(rec.total_size())
                    .ok_or(SegmentError::Overflow)?;
            }
            Ok(None) | Err(_) => break,
        }
    }
    Ok(offset)
}
