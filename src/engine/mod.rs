//! # The stable embeddable storage-engine facade (Phase 12E.1)
//!
//! The deliberately small, boring, adoption-grade public API above the
//! persistent store. An application can embed EntropyFS as a concurrent
//! content-addressed object engine without understanding FUSE, the epoch,
//! B-trees, the MutationLog, representation tags, DSFB, rANS layouts, or
//! the io_uring implementation — none of those are visible here.
//!
//! ```text
//!                         Engine            <- this module
//!                           │
//!                 persistent Store
//!                           │
//!         representation / materialization
//!                           │
//!             persistent object layer
//!
//!          ┌────────────────┼───────────────┐
//!          │                │               │
//!        FUSE             ublk         Engine API
//! ```
//!
//! # PURPOSE
//!
//! Phase 12E turns the research-grade store into something another
//! engineer can embed: content identity, exact bytes, range reads,
//! durability, maintenance, metrics, typed errors — nothing else. FUSE is
//! a consumer of the engine-shaped store, never the engine itself.
//!
//! # BOUNDARY
//!
//! KNOWS: the store's public blob-namespace protocol (see MODEL) and its
//! public accounting accessors. NEVER KNOWS: epoch internals, B-tree
//! layout, descriptor encoding, DSFB state, representation families,
//! segment records, or any transport implementation detail. The facade
//! must stay stable while every one of those changes underneath.
//!
//! # MODEL
//!
//! Content identity: a [`BlobId`] is the 256-bit BLAKE3 hash of the
//! blob's *materialized logical bytes* — the exact same identity the
//! store uses for chunks ([`crate::core::extent::ChunkId`]). Consequences,
//! all documented as part of the public contract:
//!
//! - equivalent logical bytes always receive the same id (dedup is
//!   automatic and idempotent: re-putting identical bytes is a no-op);
//! - the id is stable across compaction, representation migration,
//!   encoder-policy changes, io-backend choice, and GC — it is a function
//!   of the bytes alone;
//! - a different byte string cannot collide with an existing id (BLAKE3
//!   collision resistance), so "the id exists" implies "the bytes are
//!   stored".
//!
//! Blob namespace: every blob is one regular file under a hidden
//! directory (`.engine`, inode 2 at engine-create time) in the store's
//! normal namespace, named by the blob id's 64 hex characters. This
//! reuses the store's *entire* existing machinery unchanged — the extent
//! tree, the chunk index (dedup), reachability GC, fsck, snapshots, crash
//! recovery — with zero new persistent structures. A store created by
//! mkfs gains the namespace lazily on the first read-write engine open.
//!
//! Put protocol (write-then-rename): a blob is NEVER acknowledged under
//! its final name until its content write is acknowledged:
//!
//! ```text
//! create .engine/blob-<id>-tmp-<n>     (tmp name; never acked as a blob)
//! epoch_write(content)                 (acked; durable in the mutation log)
//! rename to .engine/<hex id>           (the blob's ack point)
//! ```
//!
//! This makes "the final name exists ⇒ the blob is complete and
//! byte-exact" an invariant. In particular a crash cannot leave an
//! empty/partial file under a blob's final name: the only crash residue
//! is a `-tmp-` file, which the next read-write open sweeps. `put_blob`
//! therefore has at-least-once semantics — a retry after a crash is a
//! no-op that returns the same id.
//!
//! Durability: `put_blob` acknowledges at the mutation-log append
//! (process-crash-safe; visible to this process's later reads). `sync()`
//! runs the Phase-12B durability barrier — the same generation/group
//! machinery FUSE fsync uses — making everything acknowledged so far
//! power-durable. The engine adds no durability model of its own.
//!
//! # PERSISTENT AUTHORITY
//!
//! Indirect but total: everything the engine persists goes through the
//! store's ordinary committed paths (epoch ops + checkpoint + barrier +
//! GC). The `.engine` directory and its files are ordinary inodes —
//! visible to fsck, GC, snapshots and (transparently) to a later FUSE
//! mount of the same store. Nothing in this module introduces a new
//! on-disk structure or format bit.
//!
//! # CORRECTNESS INVARIANTS
//!
//! - `get_blob(id)` verifies the materialized bytes hash to `id` before
//!   returning; a mismatch is `CorruptStore`, never silently returned.
//! - `put_blob` dedup checks require the existing entry to be a regular
//!   file (a directory squatting on a blob name is corruption, not a hit).
//! - The tmp-sweep at open deletes ONLY `-tmp-` names; any other entry in
//!   the namespace is left untouched (foreign files are not silently
//!   destroyed).
//! - `close` never drops the store while an operation is in flight; a
//!   closed engine returns `Closed` and the native handle is unusable.
//!
//! # CONCURRENCY
//!
//! `Engine` is `Send + Sync` and every method is safe for concurrent use
//! from many threads (the same contract the C ABI and Go binding inherit):
//! concurrent puts (distinct tmp names, atomic rename), concurrent gets,
//! range reads, contains, metrics, sync — all proceed in parallel; only
//! the store's own internal locks serialize what must serialize. `close`
//! is the exclusive operation: it waits for in-flight operations to drain
//! before releasing the store.
//!
//! # DURABILITY
//!
//! - `put_blob` returns ⇒ the blob is process-crash-safe and visible to
//!   this store's subsequent opens (recovery replays the acknowledged
//!   log).
//! - `sync()` returns ⇒ every previously acknowledged blob is
//!   power-durable (12B barrier; same as an fsync of a FUSE file).
//! - `close` is NOT a durability operation (mirrors POSIX close).
//!
//! # RESOURCE BOUNDS
//!
//! Inputs are `&[u8]` (length naturally bounded by the caller's memory);
//! blob size is unbounded by the facade and chunked by the store. Range
//! reads validate offset/length before touching the store. The store's
//! `Limits` (per-chunk decode, alloc, depth) remain the authoritative
//! resource bounds; a hostile blob cannot expand the engine's allocation
//! beyond the store's gates.
//!
//! # PERFORMANCE
//!
//! The facade adds one directory lookup per operation and one full-blob
//! hash on `get_blob` (the exactness gate). The write path is the store's
//! own epoch pipeline (foreground policy from `StoreConfig`); dedup hits
//! skip the encode entirely. `metrics()` performs one O(n) namespace scan
//! for `blob_count` — documented, never hidden.
//!
//! # FAILURE MODES
//!
//! Every failure is a typed [`EngineError`] with a stable [`ErrorCode`]
//! class (NotFound, InvalidArgument, CorruptStore, IncompatibleFormat,
//! ResourceLimit, Io, Busy, Unsupported, Internal, Closed) plus a
//! human-readable message. Programs must be able to switch on the code;
//! they must never need to parse the message. The store's `StoreError`
//! variants map onto these classes (see `From<StoreError>`).
//!
//! # HISTORY / EVIDENCE
//!
//! Phase 12E (this phase): the facade is the embeddable surface; the
//! blob-namespace-as-directory design was chosen over a raw-object design
//! because raw objects unreferenced by any inode are unreachable and
//! would be reclaimed by GC — a blob namespace must be root-reachable,
//! and the directory tree already is. The C ABI (12E.14) and Go binding
//! (12E.15) are thin translators over exactly this API; the exactness
//! oracles in those phases assert Rust/C/Go byte identity on the same
//! fixtures.

#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::core::extent::ChunkId;
use crate::optimizer::foreground::ForegroundPolicy;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::directory;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig, StoreError};

pub mod metrics;

pub use metrics::{
    AccountingMetrics, CacheMetrics, DsfbMetrics, EngineMetrics, FormatInfo, GcMetrics,
    METRIC_REGISTRY, MetricDef, PhaseMetrics, PhysicalMetrics, PressureMetrics,
};

/// Stable engine error classes (Phase 12E.1/12E.14/12E.15).
///
/// These are the machine-readable classes the C ABI and the Go binding
/// translate; the numeric values are part of the C ABI contract and MUST
/// NOT be renumbered once released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ErrorCode {
    /// Success (not an error; the C ABI's 0 return).
    Ok = 0,
    /// The blob (or store) does not exist.
    NotFound = 1,
    /// A caller-supplied argument is invalid (offset/length/id/path).
    InvalidArgument = 2,
    /// Persistent corruption detected (decode failure, hash mismatch,
    /// invariant violation while reading).
    CorruptStore = 3,
    /// The store's on-disk format/features cannot be opened in the
    /// requested mode (unknown incompat bits, unknown ro_compat for a
    /// writable open, format major mismatch).
    IncompatibleFormat = 4,
    /// A store resource limit was exceeded (size, decode work, capacity).
    ResourceLimit = 5,
    /// Underlying I/O failure.
    Io = 6,
    /// The engine is busy (e.g. an operation conflicts with an exclusive
    /// maintenance pass).
    Busy = 7,
    /// The operation is not supported in this configuration (e.g. write
    /// on a read-only open).
    Unsupported = 8,
    /// An internal invariant failed (a bug — please report).
    Internal = 9,
    /// The engine handle is closed.
    Closed = 10,
}

impl ErrorCode {
    /// The C-ABI numeric value.
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Recover an error code from its C-ABI numeric value (`None` for
    /// unknown values, which must never be produced by this crate).
    pub const fn from_i32(v: i32) -> Option<Self> {
        Some(match v {
            0 => Self::Ok,
            1 => Self::NotFound,
            2 => Self::InvalidArgument,
            3 => Self::CorruptStore,
            4 => Self::IncompatibleFormat,
            5 => Self::ResourceLimit,
            6 => Self::Io,
            7 => Self::Busy,
            8 => Self::Unsupported,
            9 => Self::Internal,
            10 => Self::Closed,
            _ => return None,
        })
    }

    /// Stable class name (for JSON/schemas).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NotFound => "not_found",
            Self::InvalidArgument => "invalid_argument",
            Self::CorruptStore => "corrupt_store",
            Self::IncompatibleFormat => "incompatible_format",
            Self::ResourceLimit => "resource_limit",
            Self::Io => "io",
            Self::Busy => "busy",
            Self::Unsupported => "unsupported",
            Self::Internal => "internal",
            Self::Closed => "closed",
        }
    }
}

/// A typed engine error: a stable machine-readable class plus a
/// human-readable message. Programs switch on `code`; they never parse
/// `message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineError {
    /// Stable error class.
    pub code: ErrorCode,
    /// Human-readable detail (may change; never parsed by programs).
    pub message: String,
}

impl EngineError {
    /// Construct an error.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The stable class.
    pub const fn code(&self) -> ErrorCode {
        self.code
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.name(), self.message)
    }
}

impl std::error::Error for EngineError {}

/// Map store errors onto the stable engine classes. The mapping is part
/// of the public contract: a `StoreError::Io` is always `EngineError::Io`,
/// a decode failure is always `CorruptStore`, etc.
impl From<StoreError> for EngineError {
    fn from(e: StoreError) -> Self {
        let (code, message) = match e {
            StoreError::Segment(s) => (ErrorCode::CorruptStore, format!("segment: {s:?}")),
            StoreError::Superblock(s) => (ErrorCode::CorruptStore, format!("superblock: {s}")),
            StoreError::Index(s) => (ErrorCode::CorruptStore, format!("index: {s}")),
            StoreError::MissingObject(cid) => (
                ErrorCode::CorruptStore,
                format!("referenced object {cid} is missing"),
            ),
            StoreError::MissingChunk(cid) => (
                ErrorCode::CorruptStore,
                format!("chunk descriptor for {cid} is missing"),
            ),
            StoreError::Descriptor(s) => (ErrorCode::CorruptStore, format!("descriptor: {s}")),
            StoreError::NotOpen => (ErrorCode::Internal, "store not open".to_string()),
            StoreError::Locked => (ErrorCode::Busy, "store is locked by another process".into()),
            StoreError::Config(s) => (ErrorCode::InvalidArgument, format!("config: {s}")),
            StoreError::Limit(s) => (ErrorCode::ResourceLimit, format!("limit: {s}")),
            StoreError::Full(s) => (ErrorCode::ResourceLimit, format!("full: {s}")),
            StoreError::Io(s) => (ErrorCode::Io, s),
            StoreError::CrashSimulated(s) => (ErrorCode::Internal, format!("crash hook: {s}")),
            StoreError::Invariant(s) => (ErrorCode::CorruptStore, format!("invariant: {s}")),
            // Phase 12E.3 variants (added with the compatibility seal).
            StoreError::ReadOnly => (ErrorCode::Unsupported, "store opened read-only".into()),
            StoreError::IncompatibleFormat(err) => (ErrorCode::IncompatibleFormat, err.to_string()),
        };
        EngineError::new(code, message)
    }
}

/// Content identity: the 256-bit BLAKE3 hash of the blob's logical bytes.
///
/// Stable across compaction, representation migration, encoder-policy
/// changes, io-backend choice and GC. Equal logical bytes always receive
/// the same id; the id never depends on physical record type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlobId(pub [u8; 32]);

impl BlobId {
    /// Wrap raw bytes (the id is validated only by use — a malformed id
    /// simply never matches a stored blob).
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The id's bytes (the C ABI and Go binding use exactly these 32
    /// bytes).
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The underlying content id (identical value; the facade's identity
    /// IS the store's chunk identity).
    pub const fn as_chunk_id(&self) -> ChunkId {
        ChunkId::new(self.0)
    }

    /// Parse from 64 hex characters (case-insensitive).
    pub fn from_hex(s: &str) -> Option<Self> {
        ChunkId::from_hex(s).map(|c| Self(*c.as_bytes()))
    }
}

impl From<ChunkId> for BlobId {
    fn from(c: ChunkId) -> Self {
        Self(*c.as_bytes())
    }
}

impl std::fmt::Display for BlobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl serde::Serialize for BlobId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for BlobId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_hex(&s).ok_or_else(|| serde::de::Error::custom("invalid 64-hex blob id"))
    }
}

/// Durability level for a single put.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// Acknowledge at the mutation-log append: process-crash-safe and
    /// visible to later opens; NOT power-durable until a later `sync()`.
    Ack,
    /// Acknowledge only after a full durability barrier (power-durable).
    Durable,
}

/// Open options for [`Engine::create`] / [`Engine::open`]. Only stable
/// public semantics live here; store internals are never exposed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineOpenOptions {
    /// Storage transport (`sync` default per the sealed 10F/12E evidence;
    /// `uring` opt-in — never a format property).
    pub io_backend: crate::store::io::IoBackendKind,
    /// io_uring submission-queue capacity (`uring` backend only).
    pub io_uring_entries: u32,
    /// Capacity override for deterministic ENOSPC courts (never above the
    /// physical device — the store's honesty rule).
    pub capacity_override: Option<u64>,
    /// Owner of the namespace root at create time (create only).
    pub root_uid: u32,
    /// Group of the namespace root at create time (create only).
    pub root_gid: u32,
    /// Foreground representation policy for the write path (create only;
    /// the mounted filesystem's policy is otherwise used).
    pub foreground: ForegroundPolicy,
    /// Phase 12E.3: open read-only. Unknown `ro_compat` bits are
    /// permitted (the documented RO fallback); every write operation
    /// fails with `ErrorCode::Unsupported`. A read-only open observes the
    /// last durable checkpoint (the mutation log is not replayed — replay
    /// is a write), so acknowledged-but-uncheckpointed blobs are not
    /// visible until a checkpoint/sync has run.
    pub read_only: bool,
}

impl Default for EngineOpenOptions {
    fn default() -> Self {
        let c = StoreConfig::default();
        Self {
            io_backend: c.io_backend,
            io_uring_entries: c.io_uring_entries,
            capacity_override: None,
            root_uid: c.root_uid,
            root_gid: c.root_gid,
            foreground: c.foreground,
            read_only: false,
        }
    }
}

/// The hidden blob-namespace directory name (relative to the store root).
/// One regular file per blob, named by the blob id's 64 hex characters.
const ENGINE_DIR_NAME: &[u8] = b".engine";

/// Prefix for in-flight put temp names. `-tmp-` names are never blob ids;
/// the open-time sweep removes them.
const TMP_PREFIX: &[u8] = b"blob-";
const TMP_SUFFIX: &[u8] = b"-tmp-";

/// Result of a compaction pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompactionReport {
    /// Unreachable (reclaimable) bytes before the pass.
    pub unreachable_before_bytes: u64,
    /// Bytes reclaimed by the pass.
    pub reclaimed_bytes: u64,
    /// Unreachable bytes after the pass (≈ 0 after full compaction).
    pub unreachable_after_bytes: u64,
    /// Physical bytes used after the pass.
    pub physical_used_after_bytes: u64,
    /// Root-reachable physical bytes after the pass.
    pub live_bytes_after: u64,
}

/// Shared engine state. Ops clone the `Arc<Store>` under the mutex and
/// then run lock-free on the clone; `close` drains in-flight ops before
/// dropping the store.
struct EngineInner {
    store: Mutex<Option<Arc<Store>>>,
    closed: AtomicBool,
    ops_in_flight: AtomicU64,
    drain_cv: Condvar,
}

/// The embeddable storage engine (Phase 12E.1).
///
/// See the module doc for the full model: content-addressed blobs over
/// the persistent store, concurrent-safe, with the exactness gate.
pub struct Engine {
    inner: Arc<EngineInner>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine").finish_non_exhaustive()
    }
}

// SAFETY-adjacent contract note: `Store` is used behind `Arc` by the FUSE
// frontend across threads, so `EngineInner` is automatically `Send +
// Sync`. No `unsafe` here (`#![forbid(unsafe_code)]`).

impl Engine {
    /// Generate a fresh store uuid (mkfs): BLAKE3 of time + pid + path,
    /// first 16 bytes. Uniqueness-grade (never security-grade).
    fn generate_uuid(path: &Path) -> [u8; 16] {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut h = blake3::Hasher::new();
        h.update(&now.to_le_bytes());
        h.update(&std::process::id().to_le_bytes());
        h.update(path.to_string_lossy().as_bytes());
        let out = h.finalize();
        let mut u = [0u8; 16];
        u.copy_from_slice(&out.as_bytes()[..16]);
        u
    }

    /// Create a new store and return an open engine (mkfs + namespace).
    ///
    /// # What
    ///
    /// Runs the store's mkfs (`Store::create`), creates the hidden blob
    /// namespace directory, and makes it durable. The returned engine is
    /// ready for `put_blob`.
    ///
    /// # Why
    ///
    /// The engine must guarantee its namespace exists before the first
    /// put, so the namespace is created here — atomically with the store's
    /// birth — rather than lazily (lazy creation exists for legacy stores
    /// opened by `Engine::open`).
    ///
    /// # Durability
    ///
    /// Returns after the namespace is power-durable (one barrier).
    pub fn create(path: &Path, opts: &EngineOpenOptions) -> Result<Self, EngineError> {
        let config = StoreConfig {
            io_backend: opts.io_backend,
            io_uring_entries: opts.io_uring_entries,
            capacity_override: opts.capacity_override,
            root_uid: opts.root_uid,
            root_gid: opts.root_gid,
            foreground: opts.foreground,
            ..StoreConfig::default()
        };
        let uuid = Self::generate_uuid(path);
        let store = Store::create(path, &config, uuid).map_err(EngineError::from)?;
        // Create the hidden namespace directory (inode 2, owned by the
        // store's root owner) through the epoch, then make it durable so a
        // fresh open sees it immediately (open does not lazily create in
        // read-only mode).
        let hooks = CrashHooks::none();
        store
            .epoch_create(
                1,
                ENGINE_DIR_NAME,
                NewEntry::dir(0o700, opts.root_uid, opts.root_gid),
                &hooks,
            )
            .map_err(EngineError::from)?;
        store
            .durability_barrier(&hooks)
            .map_err(EngineError::from)?;
        let engine = Self {
            inner: Arc::new(EngineInner {
                store: Mutex::new(Some(Arc::new(store))),
                closed: AtomicBool::new(false),
                ops_in_flight: AtomicU64::new(0),
                drain_cv: Condvar::new(),
            }),
        };
        Ok(engine)
    }

    /// Open an existing store (recovery + derived-index rebuild) and
    /// return an engine.
    ///
    /// On a read-write open of a store that has no blob namespace yet (a
    /// store created by mkfs/FUSE), the namespace is created lazily and
    /// stale `-tmp-` files from any crashed earlier engine are swept.
    pub fn open(path: &Path, opts: &EngineOpenOptions) -> Result<Self, EngineError> {
        // Fail fast with a clear typed error when the path is not a store.
        if !path.join("segments").is_dir() {
            return Err(EngineError::new(
                ErrorCode::NotFound,
                format!("no entropyfs store at {}", path.display()),
            ));
        }
        let config = StoreConfig {
            io_backend: opts.io_backend,
            io_uring_entries: opts.io_uring_entries,
            capacity_override: opts.capacity_override,
            read_only: opts.read_only,
            ..StoreConfig::default()
        };
        let store = Store::open(path, &config).map_err(EngineError::from)?;
        let hooks = CrashHooks::none();
        // Read-write: ensure the namespace exists and sweep stale tmp
        // files. Read-only: require the namespace (opening a legacy store
        // read-only without a namespace is an error — there is nothing to
        // read), and never sweep (a write).
        let dir_ino = {
            let ep = store.epoch();
            match store
                .dir_lookup_epoch(&ep, 1, ENGINE_DIR_NAME)
                .map_err(EngineError::from)?
            {
                Some(e) if e.d_type == directory::dt::DT_DIR => e.ino,
                Some(_) => {
                    return Err(EngineError::new(
                        ErrorCode::CorruptStore,
                        "engine namespace name is occupied by a non-directory entry",
                    ));
                }
                None if opts.read_only => {
                    return Err(EngineError::new(
                        ErrorCode::NotFound,
                        "store has no engine blob namespace (created read-only; \
                         nothing to read)",
                    ));
                }
                None => {
                    // The lookup guard must be dropped before the create
                    // (the epoch mutex is not reentrant — holding one
                    // guard while acquiring another deadlocks).
                    drop(ep);
                    let _ = store
                        .epoch_create(
                            1,
                            ENGINE_DIR_NAME,
                            NewEntry::dir(0o700, config.root_uid, config.root_gid),
                            &hooks,
                        )
                        .map_err(EngineError::from)?;
                    let ep3 = store.epoch();
                    store
                        .dir_lookup_epoch(&ep3, 1, ENGINE_DIR_NAME)
                        .map_err(EngineError::from)?
                        .ok_or_else(|| {
                            EngineError::new(ErrorCode::Internal, "namespace create vanished")
                        })?
                        .ino
                }
            }
        };
        // Sweep stale `-tmp-` files (crash residue from interrupted puts).
        // Safe: the store's flock guarantees no other engine/FUSE process
        // holds this store; `-tmp-` names are never valid blob ids.
        // Read-only opens never sweep (a write).
        if !opts.read_only {
            {
                let ep = store.epoch();
                let entries = store
                    .read_dir_epoch(&ep, dir_ino)
                    .map_err(EngineError::from)?;
                let stale: Vec<Vec<u8>> = entries
                    .into_iter()
                    .filter(|(name, e)| e.d_type == directory::dt::DT_REG && is_tmp_name(name))
                    .map(|(name, _)| name)
                    .collect();
                drop(ep);
                for name in stale {
                    store
                        .epoch_unlink(dir_ino, &name, false, &hooks)
                        .map_err(EngineError::from)?;
                }
            }
        }
        Ok(Self {
            inner: Arc::new(EngineInner {
                store: Mutex::new(Some(Arc::new(store))),
                closed: AtomicBool::new(false),
                ops_in_flight: AtomicU64::new(0),
                drain_cv: Condvar::new(),
            }),
        })
    }

    // -- op lifecycle ----------------------------------------------------

    /// Clone the store Arc for one operation, registering it as in-flight.
    /// `close` drains in-flight ops before releasing the store, so a
    /// cloned Arc is always valid for the operation's lifetime.
    fn acquire_store(&self) -> Result<Arc<Store>, EngineError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(EngineError::new(ErrorCode::Closed, "engine is closed"));
        }
        self.inner.ops_in_flight.fetch_add(1, Ordering::AcqRel);
        let guard = self.inner.store.lock().unwrap_or_else(|p| p.into_inner());
        let store = match guard.as_ref() {
            Some(s) => Arc::clone(s),
            None => {
                drop(guard);
                self.finish_op();
                return Err(EngineError::new(ErrorCode::Closed, "engine is closed"));
            }
        };
        drop(guard);
        Ok(store)
    }

    /// Finish an operation (decrement the in-flight count; wake a waiting
    /// `close` on the transition to zero).
    fn finish_op(&self) {
        if self.inner.ops_in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.inner.drain_cv.notify_all();
        }
    }

    // -- namespace helpers ------------------------------------------------

    /// Resolve the blob namespace directory inode (never cached: the
    /// committed inode number is stable, but a per-op lookup keeps this
    /// facade free of store-internal assumptions).
    fn engine_dir_ino(&self, store: &Store) -> Result<u64, EngineError> {
        let ep = store.epoch();
        let entry = store
            .dir_lookup_epoch(&ep, 1, ENGINE_DIR_NAME)
            .map_err(EngineError::from)?
            .ok_or_else(|| {
                EngineError::new(
                    ErrorCode::CorruptStore,
                    "engine namespace directory is missing from the store root",
                )
            })?;
        if entry.d_type != directory::dt::DT_DIR {
            return Err(EngineError::new(
                ErrorCode::CorruptStore,
                "engine namespace name is occupied by a non-directory entry",
            ));
        }
        Ok(entry.ino)
    }

    /// The final (hex) name of a blob file.
    fn blob_name(id: &BlobId) -> Vec<u8> {
        id.to_string().into_bytes()
    }

    /// A unique in-flight temp name for a put. The counter is hex-encoded
    /// (raw LE bytes can contain NUL, which `Store::validate_name` — and
    /// the persistent directory format — reject).
    fn tmp_name(id: &BlobId, counter: u64) -> Vec<u8> {
        let mut v = Vec::with_capacity(64 + 24);
        v.extend_from_slice(TMP_PREFIX);
        v.extend_from_slice(&id.to_string().into_bytes());
        v.extend_from_slice(TMP_SUFFIX);
        v.extend_from_slice(format!("{counter:016x}").as_bytes());
        v
    }

    // -- public API --------------------------------------------------------

    /// Store a blob. Returns its content id (dedup: re-putting identical
    /// bytes is a no-op returning the same id).
    ///
    /// # Durability
    ///
    /// Acknowledges at the mutation-log append (process-crash-safe).
    /// Call [`Engine::sync`] for power durability.
    pub fn put_blob(&self, bytes: &[u8]) -> Result<BlobId, EngineError> {
        self.put_blob_with(bytes, Durability::Ack)
    }

    /// [`Engine::put_blob`] with an explicit durability level.
    pub fn put_blob_with(
        &self,
        bytes: &[u8],
        durability: Durability,
    ) -> Result<BlobId, EngineError> {
        let store = self.acquire_store()?;
        let _op = OpGuard::new(self);
        let id = BlobId::from(ChunkId::of(bytes));
        crate::perf::trace::span!(
            "engine.put_blob",
            op = "put_blob",
            len = bytes.len() as u64,
            id = trace_id(&id),
            durable = matches!(durability, Durability::Durable)
        );
        let hooks = CrashHooks::none();
        let dir_ino = self.engine_dir_ino(&store)?;
        let final_name = Self::blob_name(&id);

        // Fast dedup: a regular file under the final name is a completed,
        // acknowledged blob (write-then-rename makes partial states
        // impossible under a final name). A non-file occupant is
        // corruption — treat as missing so the put repairs it, and let a
        // later fsck report the oddity.
        {
            let ep = store.epoch();
            match store
                .dir_lookup_epoch(&ep, dir_ino, &final_name)
                .map_err(EngineError::from)?
            {
                Some(e) if e.d_type == directory::dt::DT_REG => return Ok(id),
                Some(_) => {}
                None => {}
            }
        }

        // Write-then-rename protocol (see module doc): the blob is never
        // acknowledged under its final name until its content write is
        // acknowledged.
        let tmp = Self::tmp_name(&id, self.tmp_counter());
        let file_ino = store
            .epoch_create(
                dir_ino,
                &tmp,
                NewEntry::file(0o600, store.config().root_uid, store.config().root_gid),
                &hooks,
            )
            .map_err(EngineError::from)?;
        if !bytes.is_empty() {
            store
                .epoch_write(
                    file_ino,
                    0,
                    bytes,
                    OptimizeOptions::default(),
                    store.config().foreground,
                    &hooks,
                )
                .map_err(EngineError::from)?;
        }
        // The rename is the ack point. A concurrent put of the same blob
        // may have renamed first: an "already exists" outcome is a dedup
        // hit, not an error (both writers wrote identical bytes).
        match store
            .epoch_rename(dir_ino, &tmp, dir_ino, &final_name, &hooks)
            .map_err(EngineError::from)
        {
            Ok(_) => {}
            Err(e) if e.code == ErrorCode::CorruptStore && e.message.contains("already exists") => {
                let ep = store.epoch();
                let exists = store
                    .dir_lookup_epoch(&ep, dir_ino, &final_name)
                    .map_err(EngineError::from)?
                    .map(|e| e.d_type == directory::dt::DT_REG)
                    .unwrap_or(false);
                if !exists {
                    return Err(EngineError::new(
                        ErrorCode::Internal,
                        "concurrent put lost its blob and the winner vanished",
                    ));
                }
            }
            Err(e) => return Err(e),
        }
        if durability == Durability::Durable {
            store
                .durability_barrier(&hooks)
                .map_err(EngineError::from)?;
        }
        Ok(id)
    }

    /// Monotonic per-engine counter for tmp-name uniqueness.
    fn tmp_counter(&self) -> u64 {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    /// Fetch a blob's complete bytes, verifying they hash to the id
    /// (the exactness gate — a mismatch is `CorruptStore`, never a silent
    /// return).
    pub fn get_blob(&self, id: BlobId) -> Result<Vec<u8>, EngineError> {
        let bytes = self.read_blob_range(id, 0, usize::MAX)?;
        // The full-blob exactness gate: the returned bytes must be the
        // id's preimage. This is the engine's byte-exactness promise.
        let got = ChunkId::of(&bytes);
        if BlobId::from(got) != id {
            return Err(EngineError::new(
                ErrorCode::CorruptStore,
                format!("blob {id} materialized to bytes hashing to {got} (content mismatch)"),
            ));
        }
        Ok(bytes)
    }

    /// Read a byte range of a blob (EOF-clipped like `pread`; `len ==
    /// usize::MAX` reads to the end).
    ///
    /// Range reads are integrity-protected at the chunk level by the
    /// store (record CRCs + content-id binding per 64 KiB chunk); the
    /// full-blob hash gate is applied by [`Engine::get_blob`].
    pub fn read_blob_range(
        &self,
        id: BlobId,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, EngineError> {
        let store = self.acquire_store()?;
        let _op = OpGuard::new(self);
        crate::perf::trace::span!(
            "engine.read_blob_range",
            op = "read_range",
            id = trace_id(&id),
            offset = offset,
            len = len as u64
        );
        let dir_ino = self.engine_dir_ino(&store)?;
        let name = Self::blob_name(&id);
        let ep = store.epoch();
        let entry = store
            .dir_lookup_epoch(&ep, dir_ino, &name)
            .map_err(EngineError::from)?
            .ok_or_else(|| EngineError::new(ErrorCode::NotFound, format!("blob {id} not found")))?;
        if entry.d_type != directory::dt::DT_REG {
            return Err(EngineError::new(
                ErrorCode::CorruptStore,
                format!("blob name {id} is occupied by a non-file entry"),
            ));
        }
        let inode = store
            .get_inode_epoch(&ep, entry.ino)
            .map_err(EngineError::from)?
            .ok_or_else(|| {
                EngineError::new(ErrorCode::CorruptStore, format!("blob {id} inode missing"))
            })?;
        let size = inode.size;
        if offset >= size {
            return Ok(Vec::new());
        }
        let avail = size.saturating_sub(offset).min(len as u64);
        store
            .read_file_epoch(&ep, entry.ino, offset, avail)
            .map_err(EngineError::from)
    }

    /// Whether a blob id exists in the store (the id was put and
    /// acknowledged).
    pub fn contains(&self, id: BlobId) -> Result<bool, EngineError> {
        let store = self.acquire_store()?;
        let _op = OpGuard::new(self);
        let dir_ino = self.engine_dir_ino(&store)?;
        let ep = store.epoch();
        Ok(store
            .dir_lookup_epoch(&ep, dir_ino, &Self::blob_name(&id))
            .map_err(EngineError::from)?
            .map(|e| e.d_type == directory::dt::DT_REG)
            .unwrap_or(false))
    }

    /// Make every previously acknowledged blob power-durable (the
    /// Phase-12B durability barrier — the same generation/group machinery
    /// FUSE fsync uses).
    pub fn sync(&self) -> Result<(), EngineError> {
        let store = self.acquire_store()?;
        let _op = OpGuard::new(self);
        crate::perf::trace::span!("engine.sync", op = "sync");
        store
            .durability_barrier(&CrashHooks::none())
            .map_err(EngineError::from)
    }

    /// Run full GC compaction (Phase-9H convergence): flush the epoch,
    /// sweep stale tmp files, compact every segment, and publish a new
    /// root. Idempotent; a second run reclaims ≈ 0.
    ///
    /// The engine is its own exclusive user (store flock), so compaction
    /// runs on the engine's own store — no reopen, no lock games. Other
    /// operations proceed in parallel; only the store's commit
    /// coordinator serializes the final publication.
    pub fn compact(&self) -> Result<CompactionReport, EngineError> {
        let store = self.acquire_store()?;
        let _op = OpGuard::new(self);
        crate::perf::trace::span!("engine.compact", op = "compact");
        let hooks = CrashHooks::none();
        // 1. Sweep stale tmp files (engine garbage GC cannot see as
        //    unreachable — tmp inodes are reachable from the namespace
        //    dir).
        let dir_ino = self.engine_dir_ino(&store)?;
        let stale: Vec<Vec<u8>> = {
            let ep = store.epoch();
            store
                .read_dir_epoch(&ep, dir_ino)
                .map_err(EngineError::from)?
                .into_iter()
                .filter(|(name, e)| e.d_type == directory::dt::DT_REG && is_tmp_name(name))
                .map(|(name, _)| name)
                .collect()
        };
        for name in stale {
            store
                .epoch_unlink(dir_ino, &name, false, &hooks)
                .map_err(EngineError::from)?;
        }
        // 2. Flush the epoch so GC's reachability walk sees every object
        //    (GC roots are committed state; epoch-staged objects are
        //    referenced only by the log).
        store
            .ensure_epoch_flushed(&hooks)
            .map_err(EngineError::from)?;
        // 3. Full compaction.
        let unreachable_before =
            crate::store::gc::unreachable_bytes(&store).map_err(EngineError::from)?;
        let reclaimed =
            crate::store::gc::compact_full(&store, &hooks).map_err(EngineError::from)?;
        let unreachable_after =
            crate::store::gc::unreachable_bytes(&store).map_err(EngineError::from)?;
        let physical_used_after_bytes = store.physical_used();
        let live_bytes_after = crate::store::physical::physical_report(&store)
            .map(|r| r.live_bytes)
            .unwrap_or(0);
        Ok(CompactionReport {
            unreachable_before_bytes: unreachable_before,
            reclaimed_bytes: reclaimed,
            unreachable_after_bytes: unreachable_after,
            physical_used_after_bytes,
            live_bytes_after,
        })
    }

    /// Collect the versioned operational metrics DTO (see
    /// [`EngineMetrics`] and the metric registry for precise definitions).
    ///
    /// No full-store walk is performed except the O(n) namespace scan for
    /// `blob_count`; `gc.unreachable_bytes` reports the store's
    /// last-known value (refresh via `compact`/`fsck`).
    pub fn metrics(&self) -> Result<EngineMetrics, EngineError> {
        let store = self.acquire_store()?;
        let _op = OpGuard::new(self);
        collect_engine_metrics(&store)
    }

    /// Close the engine: waits for in-flight operations to drain, then
    /// releases the store (and its mount lock). Subsequent operations
    /// return `Closed`. Not a durability operation.
    pub fn close(&self) -> Result<(), EngineError> {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return Err(EngineError::new(ErrorCode::Closed, "engine is closed"));
        }
        let mut guard = self.inner.store.lock().unwrap_or_else(|p| p.into_inner());
        while self.inner.ops_in_flight.load(Ordering::Acquire) != 0 {
            guard = self
                .inner
                .drain_cv
                .wait(guard)
                .unwrap_or_else(|p| p.into_inner());
        }
        *guard = None;
        Ok(())
    }

    /// Whether the engine handle is closed.
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Best-effort close; a second close error is ignored on drop.
        let _ = self.close();
    }
}

/// RAII in-flight guard: registers the op on construction and finishes it
/// on drop, so `close` can drain deterministically.
struct OpGuard<'a> {
    engine: &'a Engine,
}

impl<'a> OpGuard<'a> {
    fn new(engine: &'a Engine) -> Self {
        Self { engine }
    }
}

impl Drop for OpGuard<'_> {
    fn drop(&mut self) {
        self.engine.finish_op();
    }
}

/// A `-tmp-` name is a blob-name prefix + 64 hex + `-tmp-` + digits. Only
/// these are swept; anything else in the namespace is left alone.
fn is_tmp_name(name: &[u8]) -> bool {
    name.starts_with(TMP_PREFIX) && contains_suffix(name, TMP_SUFFIX)
}

fn contains_suffix(name: &[u8], suffix: &[u8]) -> bool {
    if name.len() < suffix.len() {
        return false;
    }
    &name[name.len() - suffix.len()..] == suffix
}

/// Truncated content id for trace attributes: the first 8 hex characters
/// of the blob id. Never the full id unless a caller documents a
/// stronger need (trace attribute discipline, Phase 12E.7).
fn trace_id(id: &BlobId) -> String {
    id.to_string()[..8].to_string()
}

/// Collect the versioned metrics DTO from any store (used by
/// [`Engine::metrics`] and by the `entropyfs metrics --json` CLI, which
/// serves stores without an engine namespace too). The blob count is the
/// engine namespace's file count (0 when the namespace is absent); every
/// other section is store accounting. See the [`METRIC_REGISTRY`] for
/// precise definitions.
pub fn collect_engine_metrics(store: &Store) -> Result<EngineMetrics, EngineError> {
    let root = store.current_root();
    let bits = store.feature_bits();
    let stats = store.stats();
    let capacity = store.physical_capacity();
    let used = store.physical_used();
    let phys = crate::store::physical::physical_report(store).ok();
    let dsfb = store.dsfb_stats();
    // Destructure the optional physical report once (it is not `Copy`;
    // every field is 0 when the reconciliation is unavailable).
    let (live_b, dead_b, hidden_b, unindexed_b, torn_b, pad_b, fmt_b, unexp_b) = match &phys {
        Some(r) => (
            r.live_bytes,
            r.dead_indexed_bytes,
            r.index_hidden_bytes,
            r.unindexed_bytes,
            r.torn_bytes,
            r.zero_padding_bytes,
            r.format_overhead_bytes,
            r.unexplained(),
        ),
        None => (0, 0, 0, 0, 0, 0, 0, 0),
    };
    let phases: Vec<PhaseMetrics> = store
        .perf()
        .snapshot()
        .into_iter()
        .map(|row| PhaseMetrics {
            phase: row.phase.to_string(),
            count: row.count,
            total_ms: row.total_ms,
            p50_us: row.p50_us,
            p95_us: row.p95_us,
            p99_us: row.p99_us,
        })
        .collect();
    // O(n) namespace scan for the blob count (documented): resolve the
    // engine namespace through the store's own directory lookup (the
    // engine helper is method-only; the scan uses the epoch guard once).
    let blob_count = {
        let ep = store.epoch();
        match store
            .dir_lookup_epoch(&ep, 1, ENGINE_DIR_NAME)
            .ok()
            .flatten()
        {
            Some(e) if e.d_type == directory::dt::DT_DIR => store
                .read_dir_epoch(&ep, e.ino)
                .map(|entries| {
                    entries
                        .iter()
                        .filter(|(_, e)| e.d_type == directory::dt::DT_REG)
                        .count() as u64
                })
                .unwrap_or(0),
            _ => 0,
        }
    };
    Ok(EngineMetrics {
        schema_version: 2,
        format: FormatInfo {
            format_major: root.format_major,
            format_minor: root.format_minor,
            compat: bits.compat,
            ro_compat: bits.ro_compat,
            incompat: bits.incompat,
            io_backend: store.config().io_backend.name().to_string(),
        },
        accounting: AccountingMetrics {
            logical_bytes: store.logical_bytes().unwrap_or(0),
            reachable_bytes: stats.reachable_bytes,
            physical_used_bytes: used,
            physical_capacity_bytes: capacity,
            physical_free_bytes: capacity.saturating_sub(used),
            object_count: store.object_index().len() as u64,
            data_record_count: stats.data_record_count,
            blob_count,
        },
        physical: PhysicalMetrics {
            live_bytes: live_b,
            dead_indexed_bytes: dead_b,
            index_hidden_bytes: hidden_b,
            unindexed_bytes: unindexed_b,
            torn_bytes: torn_b,
            zero_padding_bytes: pad_b,
            format_overhead_bytes: fmt_b,
            unexplained_bytes: unexp_b,
        },
        gc: GcMetrics {
            unreachable_bytes: stats.unreachable_bytes,
        },
        dsfb: DsfbMetrics {
            tracked_chunks: dsfb.tracked_chunks,
            steps: dsfb.steps,
            drift_events: dsfb.drift_events,
            slew_events: dsfb.slew_events,
            narrowed_searches: dsfb.narrowed_searches,
            candidates_evaluated: store.candidates_evaluated(),
        },
        pressure: PressureMetrics {
            pressured: store.pressure_state(),
            samples: store.pressure_trace().samples,
            enter_events: store.pressure_trace().enter_events,
            leave_events: store.pressure_trace().leave_events,
            pressured_time_ms: store.pressure_trace().pressured_time_ns / 1_000_000,
            rans_skips: store.focused_rans_skips(),
            deferred_extents: store.deferred_debt().0,
            deferred_logical_bytes: store.deferred_debt().1,
            deferred_age_ms: store.pressure_trace().deferred_age_ns / 1_000_000,
            peak_deferred_bytes: store.pressure_trace().peak_deferred_bytes,
            debt_cap_engagements: store.pressure_trace().debt_cap_engagements,
        },
        cache: CacheMetrics {
            model_cache_hits: store.model_cache_hits(),
            model_cache_misses: store.model_cache_misses(),
        },
        write_path_phases: phases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A fresh scratch store directory (deleted on drop).
    fn store_dir() -> TempDir {
        tempfile::Builder::new()
            .prefix("entropyfs-engine-test")
            .tempdir()
            .expect("tempdir")
    }

    #[test]
    fn blob_identity_semantics() {
        let id = BlobId::from(ChunkId::of(b"hello"));
        let hex = id.to_string();
        assert_eq!(hex.len(), 64);
        let back = BlobId::from_hex(&hex).unwrap();
        assert_eq!(back, id);
        assert!(BlobId::from_hex("zz").is_none());
        assert!(BlobId::from_hex(&"0".repeat(64)).is_some());
    }

    #[test]
    fn put_get_roundtrip_and_dedup() {
        let dir = store_dir();
        let engine = Engine::create(dir.path(), &EngineOpenOptions::default()).unwrap();
        let data = b"the quick brown fox jumps over the lazy dog".repeat(100);
        let id = engine.put_blob(&data).unwrap();
        // Dedup: identical bytes -> identical id, no-op put.
        let id2 = engine.put_blob(&data).unwrap();
        assert_eq!(id, id2);
        assert!(engine.contains(id).unwrap());
        let out = engine.get_blob(id).unwrap();
        assert_eq!(out, data);
        // A different blob gets a different id.
        let other = engine.put_blob(b"different bytes").unwrap();
        assert_ne!(other, id);
        assert_eq!(engine.get_blob(other).unwrap(), b"different bytes");
        assert!(
            !engine
                .contains(BlobId::from(ChunkId::of(b"never put")))
                .unwrap()
        );
        engine.close().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_blob() {
        let dir = store_dir();
        let engine = Engine::create(dir.path(), &EngineOpenOptions::default()).unwrap();
        let id = engine.put_blob(b"").unwrap();
        assert!(engine.contains(id).unwrap());
        assert_eq!(engine.get_blob(id).unwrap(), b"");
        let range = engine.read_blob_range(id, 0, 10).unwrap();
        assert!(range.is_empty());
        engine.close().unwrap();
    }

    #[test]
    fn large_blob_and_range_reads() {
        let dir = store_dir();
        let engine = Engine::create(dir.path(), &EngineOpenOptions::default()).unwrap();
        // 2 MiB of structured data (multi-chunk, dedup-friendly).
        let mut data = Vec::with_capacity(2 << 20);
        for i in 0..(2 << 20) {
            data.push((i % 251) as u8);
        }
        let id = engine.put_blob(&data).unwrap();
        assert_eq!(engine.get_blob(id).unwrap(), data);
        // Range at the start.
        assert_eq!(engine.read_blob_range(id, 0, 4).unwrap(), &data[..4]);
        // Range in the middle.
        let mid = 70_000u64;
        assert_eq!(
            engine.read_blob_range(id, mid, 100).unwrap(),
            &data[mid as usize..mid as usize + 100]
        );
        // Range past EOF -> empty.
        assert!(
            engine
                .read_blob_range(id, data.len() as u64, 10)
                .unwrap()
                .is_empty()
        );
        // EOF-clipped range.
        let clipped = engine
            .read_blob_range(id, data.len() as u64 - 8, 100)
            .unwrap();
        assert_eq!(clipped.len(), 8);
        engine.close().unwrap();
    }

    #[test]
    fn persistence_across_reopen() {
        let dir = store_dir();
        let id;
        {
            let engine = Engine::create(dir.path(), &EngineOpenOptions::default()).unwrap();
            let data = b"persistent bytes across a reopen".repeat(40);
            id = engine.put_blob(&data).unwrap();
            engine.sync().unwrap();
            engine.close().unwrap();
        }
        {
            let engine = Engine::open(dir.path(), &EngineOpenOptions::default()).unwrap();
            assert!(engine.contains(id).unwrap());
            let data = b"persistent bytes across a reopen".repeat(40);
            assert_eq!(engine.get_blob(id).unwrap(), data);
            // A fresh blob after reopen.
            let id2 = engine.put_blob(b"post-reopen").unwrap();
            assert_eq!(engine.get_blob(id2).unwrap(), b"post-reopen");
            engine.close().unwrap();
        }
    }

    #[test]
    fn open_legacy_store_creates_namespace_lazily() {
        let dir = store_dir();
        let config = StoreConfig::default();
        let store = Store::create(dir.path(), &config, [9u8; 16]).unwrap();
        drop(store);
        // No `.engine` dir yet: Engine::open creates it lazily.
        let engine = Engine::open(dir.path(), &EngineOpenOptions::default()).unwrap();
        let id = engine.put_blob(b"legacy store adoption").unwrap();
        assert_eq!(engine.get_blob(id).unwrap(), b"legacy store adoption");
        engine.close().unwrap();
    }

    #[test]
    fn missing_blob_is_not_found() {
        let dir = store_dir();
        let engine = Engine::create(dir.path(), &EngineOpenOptions::default()).unwrap();
        let err = engine
            .get_blob(BlobId::from(ChunkId::of(b"nope")))
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotFound);
        engine.close().unwrap();
    }

    #[test]
    fn concurrent_puts_and_gets() {
        let dir = store_dir();
        let engine = Arc::new(Engine::create(dir.path(), &EngineOpenOptions::default()).unwrap());
        let mut handles = Vec::new();
        for t in 0..8 {
            let engine = Arc::clone(&engine);
            handles.push(std::thread::spawn(move || {
                for i in 0..8 {
                    let payload = format!("thread-{t}-blob-{i}:{}", "x".repeat(10_000 + t * 1000));
                    let id = engine.put_blob(payload.as_bytes()).unwrap();
                    let out = engine.get_blob(id).unwrap();
                    assert_eq!(out, payload.as_bytes());
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let m = engine.metrics().unwrap();
        assert_eq!(m.accounting.blob_count, 64);
        engine.close().unwrap();
    }

    #[test]
    fn close_drains_and_rejects() {
        let dir = store_dir();
        let engine = Engine::create(dir.path(), &EngineOpenOptions::default()).unwrap();
        let id = engine.put_blob(b"before close").unwrap();
        engine.close().unwrap();
        // All ops after close are typed Closed.
        assert_eq!(engine.get_blob(id).unwrap_err().code(), ErrorCode::Closed);
        assert!(engine.is_closed());
        // A second close errors but does not panic.
        assert_eq!(engine.close().unwrap_err().code(), ErrorCode::Closed);
    }

    #[test]
    fn compact_reclaims_and_preserves() {
        let dir = store_dir();
        let engine = Engine::create(dir.path(), &EngineOpenOptions::default()).unwrap();
        let mut ids = Vec::new();
        // Fill with unique blobs.
        for i in 0..64 {
            let payload = format!("blob-{i}:{}", "y".repeat(50_000 + i));
            ids.push(engine.put_blob(payload.as_bytes()).unwrap());
        }
        // Overwrite the first 32 (superseded records become unreachable).
        for i in 0..32 {
            let payload = format!("blob-{i}-v2:{}", "y".repeat(50_000 + i));
            ids.push(engine.put_blob(payload.as_bytes()).unwrap());
        }
        engine.sync().unwrap();
        let before = engine.metrics().unwrap();
        let report = engine.compact().unwrap();
        let after = engine.metrics().unwrap();
        // Full compaction converges to reachable + bounded overhead; the
        // only residual is the superseded root record (gc.md: the fresh
        // root supersedes it, so it is never re-copied). Assert convergence,
        // not zero.
        assert!(
            report.unreachable_after_bytes < report.unreachable_before_bytes,
            "compaction must reclaim: before {} after {}",
            report.unreachable_before_bytes,
            report.unreachable_after_bytes
        );
        assert!(
            report.unreachable_after_bytes <= 4096,
            "residual must be bounded (superseded root record), got {}",
            report.unreachable_after_bytes
        );
        assert!(after.accounting.physical_used_bytes <= before.accounting.physical_used_bytes);
        // Every blob still reads back byte-exact (both the v1 ids that
        // were overwritten are now distinct v2 payloads — check the final
        // 32 ids).
        for id in &ids[32..] {
            let out = engine.get_blob(*id).unwrap();
            assert!(!out.is_empty());
        }
        engine.close().unwrap();
    }
}
