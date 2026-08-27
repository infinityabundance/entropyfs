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
//!
//! # PURPOSE
//!
//! Define the transport seam below the store and make the syscall
//! *issuing* strategy replaceable without touching format, durability
//! ordering, or recovery: `SyncIo` issues each syscall directly and
//! synchronously; `UringIo` issues the same logical operations through
//! one io_uring ring. The on-disk format is untouched by the choice — a
//! store is equally mountable with either backend.
//!
//! # BOUNDARY
//!
//! KNOWS: the store directory layout, segment file naming and open mode,
//! the record header size (payloads begin at `offset +
//! RECORD_HEADER_SIZE`), and the durability primitives the store needs.
//! NEVER KNOWS: record format semantics, transaction / epoch
//! orchestration, recovery, or any policy. This module is safe Rust
//! (`#![forbid(unsafe_code)]`); the crate's one `unsafe` surface is
//! [`crate::platform::io_uring`], with a ledger entry and a
//! walk-the-src enforcement test.
//!
//! # MODEL
//!
//! A backend is a byte-addressed transport: every operation addresses
//! `(segment_seq, offset, length)` where offsets and lengths are in
//! bytes within a segment file (or the superblock file, byte offsets).
//! Backends are `Send + Sync` handles; the store holds one `Arc<dyn
//! IoBackend>` for its lifetime. `open_segment_common` and
//! `find_clean_end_bytes` are backend-agnostic and shared so the
//! open-time torn-tail state machine cannot drift between engines.
//!
//! # PERSISTENT AUTHORITY
//!
//! Yes — this seam writes the persistent-data surface: segment bytes,
//! torn-tail truncation, superblock slots + `fsync`, GC unlinks. The
//! contract per call is identical across backends, and the acceptance
//! test for `UringIo` is canonically byte-identical store directories at
//! every crash injection point (inode wall-clock times canonicalized),
//! with recovery producing the same admissible state.
//!
//! # CORRECTNESS INVARIANTS
//!
//! - Every call completes its durability work before returning (the
//!   ADR-0008 recovery contract holds for both backends by
//!   construction).
//! - A fresh segment is the 4-byte `SEGMENT_MAGIC` made durable before
//!   `open_segment` returns; an existing segment has its torn tail
//!   truncated and made durable; a truncated magic (< 4 bytes) or a
//!   wrong magic is `Malformed`, never silently accepted.
//! - `write_at` is pwrite semantics (page-cache accept); short writes
//!   loop, 0-byte completions are errors.
//! - `read_many` returns results in request order (the i-th result
//!   corresponds to the i-th request) — the parallel-decode consumer
//!   relies on this.
//! - `delete_segment` is idempotent (missing file tolerated) and only
//!   runs after the new root is durable (GC ordering lives above).
//!
//! # CONCURRENCY
//!
//! The seam itself adds no locks: `SyncIo` guards only its fd map
//! (10E/10E1 discipline — clone the `Arc`, never hold across an op);
//! `UringIo` guards only its ring. The write path is serialized above
//! (`commit_lock` + segment mutex); `read_many` is the read-path
//! parallelism unit (one submission for `UringIo`, sequential preads for
//! `SyncIo`).
//!
//! # DURABILITY
//!
//! Acknowledgment semantics are spelled out per method: `write_at` =
//! page-cache accept; `sync_segment_file` = full fsync (fresh-magic
//! durability); `fdatasync_segment` = record durability;
//! `sync_segments_dir` = new segment directory entry durable;
//! `write_superblock_slot` = page cache; `fsync_superblock` = commit
//! durable. The store composes these into checkpoints and barriers.
//!
//! # RESOURCE BOUNDS
//!
//! `read_payload` / `read_many` allocate `stored_len` bytes per request
//! (record `stored_len` is a `u32` field; the read path validates via
//! `Limits` above this seam). `uring_entries` bounds the submission
//! queue capacity of `UringIo` only.
//!
//! # PERFORMANCE
//!
//! The two implementations exist because the synchronous syscall-per-op
//! shape dominated the read path (a materialization fetches a model, an
//! encoded stream, a dictionary and B-tree nodes individually) and the
//! commit durability sequence. `read_many` batches those fetches into
//! one ring submission for `UringIo`. The sealed 10F court pair
//! (tmpfs-backed, `fuse-court-*-10f-sync/uring`) measured `UringIo`
//! trailing by 5–27% on writes and 7–12% on reads — the ~2.3 µs ring
//! submit/wait floor on sub-µs tmpfs I/O; the default stays `sync`
//! (the oracle) until real-device evidence flips it.
//!
//! # FAILURE MODES
//!
//! `StoreError::Io` for syscall failures; `StoreError::Limit` for
//! arithmetic overflow in payload offsets; `SegmentError::Malformed` for
//! a torn/wrong magic; `SegmentError::Overflow` in `find_clean_end_bytes`
//! for a record whose size overflows. A record that fails decode is a
//! clean-end boundary, never an error, at open time (torn tail).
//!
//! # HISTORY / EVIDENCE
//!
//! Phase 10F (v0.6.2, ADR-0021): the seam was introduced with `SyncIo`
//! as the preserved oracle and `UringIo` as the opt-in performance path;
//! crash and durability courts are parameterized over both backends
//! (`src/tests/io_backend_parity.rs`); the sealed pair is
//! `fuse-court-*-10f-sync/uring`. The `Arc<File>` fd-cache shape came
//! from Phase 10E1 (`fuse-court-*-10e1-before/after`). The write-path
//! hunt during 10F also found and fixed `apply_sorted_batch` walking the
//! whole tree per tiny batch (empty-batch short-circuit, ~50× win on
//! both backends).

#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::Arc;

use crate::format::version::SEGMENT_MAGIC;
use crate::store::StoreError;
use crate::store::segment::SegmentError;

pub mod sync;
#[cfg(feature = "uring")]
pub mod uring;

/// Which transport the store uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoBackendKind {
    /// The reference synchronous engine (`SyncIo`; the crash-consistency
    /// oracle). Default.
    Sync,
    /// The io_uring performance path (`UringIo`); compiled only with the
    /// `uring` feature (Phase 12E.2).
    #[cfg(feature = "uring")]
    Uring,
}

impl IoBackendKind {
    /// Every backend kind (drives the crash-court parity matrix). With
    /// the `uring` feature off, the parity matrix is Sync-only (Phase
    /// 12E.2).
    #[cfg(feature = "uring")]
    pub const ALL: [IoBackendKind; 2] = [IoBackendKind::Sync, IoBackendKind::Uring];
    #[cfg(not(feature = "uring"))]
    pub const ALL: [IoBackendKind; 1] = [IoBackendKind::Sync];

    /// Parse a CLI value (`sync` | `uring`).
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "sync" => Ok(IoBackendKind::Sync),
            #[cfg(feature = "uring")]
            "uring" => Ok(IoBackendKind::Uring),
            #[cfg(not(feature = "uring"))]
            "uring" => Err("this build has no io_uring support (compiled without the \
                 `uring` feature); use --io-backend sync"
                .into()),
            other => Err(format!(
                "unknown --io-backend {other:?} (expected sync | uring)"
            )),
        }
    }

    /// Canonical name.
    pub fn name(self) -> &'static str {
        match self {
            IoBackendKind::Sync => "sync",
            #[cfg(feature = "uring")]
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
        #[cfg(feature = "uring")]
        IoBackendKind::Uring => Ok(Arc::new(uring::UringIo::new(dir, uring_entries)?)),
    }
}

/// Backend-agnostic `open_segment`: create (magic, made durable) or
/// validate + truncate the torn tail. Both backends share this; the
/// primitives (`segment_len`, `write_at`, `truncate_segment`,
/// `sync_segment_file`) are backend-specific.
///
/// All offsets and lengths here are byte units within the segment file.
pub(crate) fn open_segment_common(io: &dyn IoBackend, seq: u64) -> Result<u64, StoreError> {
    // -----------------------------------------------------------------
    // Stage 1: fresh segment — write the magic header and make it
    // durable (sync_all) before returning. A segment the store has never
    // seen must not be openable as a 0-length file whose first append
    // lands at offset 0 and is later mistaken for a magic.
    // -----------------------------------------------------------------
    let len = io.segment_len(seq)?;
    if len == 0 {
        // Fresh segment: write the magic header and make it durable
        // (existing semantics: sync_all before returning).
        io.write_at(seq, 0, &SEGMENT_MAGIC)?;
        io.sync_segment_file(seq)?;
        return Ok(4);
    }
    // -----------------------------------------------------------------
    // Stage 2: existing segment — read it whole and validate the magic.
    // A torn magic (crash mid-magic-write) or a wrong magic is
    // Malformed, never silently treated as valid: the durability
    // ordering makes a torn magic unreachable in practice (the dir entry
    // is synced only after open returns), so accepting it here would
    // paper over a real ordering violation.
    // -----------------------------------------------------------------
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
    // -----------------------------------------------------------------
    // Stage 3: torn-tail removal — find the last clean record boundary
    // and truncate beyond it, then make the truncation durable (sync_data).
    // New appends must never follow garbage; a truncation that is not
    // itself durable could resurrect the garbage after a crash.
    // -----------------------------------------------------------------
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
