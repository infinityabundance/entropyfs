//! The `fuser::Filesystem` implementation (ADR-0002).
//!
//! # PURPOSE
//!
//! The kernel-facing FRONTEND of EntropyFS: translates FUSE request
//! frames into storage-engine operations on `Store` and replies with the
//! kernel's protocol responses. Implements every operation the mount
//! exposes — `init` (kernel-config negotiation), the namespace ops
//! (`lookup`, `mknod`, `mkdir`, `unlink`, `rmdir`, `symlink`, `rename`,
//! `link`, `create`), attribute ops (`getattr`, `setattr`, `access`),
//! data ops (`open`, `read`, `write`, `flush`, `release`, `fallocate`,
//! `lseek`, `copy_file_range`), directory ops (`opendir`, `readdir`,
//! `releasedir`), `readlink`, `statfs`, the xattr family, `fsync`, and
//! the record-lock pair (`getlk`/`setlk`) — plus the background
//! optimizer worker's lifecycle hooks.
//!
//! The three architecturally significant request paths:
//!
//! - **Write path** — `write` stages data into the ACTIVE EPOCH
//!   (`Store::epoch_write`): append the staged records + `MUTATION_LOG`
//!   envelope, flush to the page cache, ack. The committed trees are
//!   untouched until the checkpoint (Phase-10D).
//! - **Read path** — `read` is a Phase-11C TWO-PHASE operation: a
//!   guard-held PREPARE (`Store::read_file_epoch_prepare`: overlay-aware
//!   extent collection + dependency enumeration + batched object fetch),
//!   then a pure-CPU DECODE (`Store::materialize_decode`) with the epoch
//!   guard released.
//! - **Durability path** — `fsync` runs the full durability barrier
//!   (`Store::durability_barrier`: epoch checkpoint, segment fdatasync,
//!   superblock slot write + fsync) — the only place a FUSE request is
//!   made power-durable.
//!
//! The background optimizer worker (spawned by `mount_fs`) is driven by
//! this module's `ops` counter (idle detection) and `worker_stop` flag
//! (set when the instance drops, so the store's advisory lock is
//! released).
//!
//! # BOUNDARY
//!
//! Knows: the FUSE wire protocol (`fuser`), inode-attribute adaptation
//! (`super::inode`), directory entry formatting (`super::directory`),
//! xattr name policy (`super::xattr`), POSIX record locks
//! (`super::locking`), the handle table, the kernel-cache invalidation
//! queue, and the `Store` / `StoreError` surface.
//!
//! Never knows: entropy algorithms, descriptor codecs, segment layout,
//! or any representation decision — all of that lives in the engine and
//! is validated byte-exact before commit. The store never depends on
//! this module (layering: `filesystem` → adapters → `Store`).
//!
//! # MODEL
//!
//! The kernel sees a POSIX filesystem; the daemon sees an epoch overlay
//! over immutable committed trees. Each request is one of:
//!
//! - a store READ — overlay-aware (`get_inode_epoch`, `dir_lookup_epoch`,
//!   `entry_list_epoch`, `read_file_epoch_prepare` resolve pending state
//!   BEFORE the checkpoint);
//! - a store MUTATION — appended to the epoch, acked after the
//!   page-cache flush (process-crash-durable, power-durable only after
//!   the next barrier);
//! - a DURABILITY request — `fsync` / unmount checkpoints the epoch and
//!   drives the barrier.
//!
//! A successful store call is acked to the kernel unchanged: this module
//! never reinterprets store results, only maps them to FUSE replies.
//!
//! # PERSISTENT AUTHORITY
//!
//! This module writes no bytes itself and makes no representation or
//! durability-ordering decisions — the store owns all of that. Its
//! authority is the ACKNOWLEDGEMENT CONTRACT: a reply to the kernel is
//! where a mutation becomes user-visible, so it must never ack an
//! operation the store has not made process-crash-durable (epoch append
//! + page-cache flush; for `fsync`, the full barrier). `Drop` (unmount)
//!   checkpoints + barriers so a clean unmount never leaves
//!   acknowledged mutations only in the mutation log.
//!
//! # CORRECTNESS INVARIANTS
//!
//! - **Read-your-writes.** The kernel must never observe
//!   partially-staged file data. Enforced twice: the mount never
//!   negotiates `FUSE_WRITEBACK_CACHE` (write-through — every `write()`
//!   waits for the daemon's ack), and the read handler serializes with
//!   the file's in-flight writes via the per-inode mutation lock. The
//!   Phase-10G violation that established this is in HISTORY / EVIDENCE.
//! - **Handle uniqueness.** `next_handle` increases monotonically and is
//!   never reused within a mount; `release` / `releasedir` remove
//!   entries; `flush` drops the closing owner's POSIX locks.
//! - **Lock ordering.** The handle and lock tables are never held while
//!   acquiring any store lock, and the per-inode mutation lock is always
//!   taken before the epoch guard — the same order as
//!   `Store::epoch_write`, so a concurrent read/write on one file cannot
//!   deadlock.
//! - **One reply per request**, mapped from a single store result; store
//!   errors become errno via `errno()`.
//! - **Cache coherence is TTL-bounded.** Kernel attribute/dentry caches
//!   expire after `ATTR_TTL` / `ENTRY_TTL` (1 s); mutations enqueue
//!   best-effort kernel notifications (`NotifyReq`) so staleness is
//!   bounded, never correctness-relevant.
//!
//! # CONCURRENCY
//!
//! The `fuser` session runs `n_threads` handler threads concurrently.
//! Shared state is partitioned:
//!
//! - `Store` — interior-mutability concurrency (Phase-8, ADR-0013):
//!   committed-tree reads traverse immutable state without any lock;
//!   writes serialize only the short commit. The active epoch has its
//!   own mutex; the Phase-11B/11C write and read paths hold it only for
//!   overlay reads and staging, never across candidate preparation or
//!   decode (see PERFORMANCE).
//! - Per-inode mutation locks — serialize file-data writes and truncates
//!   with each other AND with reads; the read handler holds the file's
//!   lock for its whole duration.
//! - `handles` / `locks` — separate mutexes for the handle table and the
//!   POSIX lock table; never held while a store lock is taken.
//! - A dedicated notifier thread drains `NotifyReq` OUTSIDE any request
//!   context: kernel notify paths can block on locks the in-flight
//!   request holds (deadlock risk, §24).
//! - The background optimizer worker polls `ops` for idle and exits on
//!   `worker_stop`; it never takes the store while a request is in
//!   flight (try-lock + idle gate).
//!
//! # DURABILITY
//!
//! - Mutations (`write`, `create`, `setattr`, namespace ops): acked
//!   after the epoch's append + page-cache flush — survive process crash
//!   (MutationLog replay, `seq > root.log_seq`), NOT power loss.
//! - `fsync` (and unmount): full barrier — epoch checkpoint + segment
//!   fdatasync + superblock flip + superblock fsync — power-durable.
//!   v1 applies the full barrier to FDATASYNC as well (data and metadata
//!   are interleaved in the segments).
//! - Power loss may lose every acknowledged write since the last barrier
//!   (POSIX: only fsync'd data is power-durable); recovery can never
//!   wedge (ADR-0008).
//!
//! # RESOURCE BOUNDS
//!
//! All request-borne sizes are kernel- or protocol-bounded and are
//! either clamped by the store's limits or mapped to errno:
//!
//! - `write` data: ≤ `max_write` (1 MiB per request, negotiated in
//!   `init`); offsets/lengths are u64 byte offsets into the file.
//! - `read` size: u32 bytes; the store clamps to its limits.
//! - names, xattr names/values: bounded by store limits
//!   (`StoreError::Limit` → `EFBIG` / `E2BIG`).
//! - The notification queue is capped (`NOTIFY_QUEUE_CAPACITY` = 4096);
//!   overflow drops the invalidation (TTLs bound staleness).
//! - The handle table grows only with the kernel's open-file count;
//!   kernel `max_background` / congestion thresholds bound request
//!   concurrency.
//! - `statfs` never advertises more capacity than the physical backing
//!   store (§22).
//!
//! # PERFORMANCE
//!
//! The module's shape is evidence-driven:
//!
//! - **Phase-8**: `max_write` 1 MiB + `FUSE_BIG_WRITES` so the kernel
//!   aggregates each `write()` into one transaction-sized request; queue
//!   tuning (`max_background` 64, congestion 48).
//! - **Phase-11B/11C**: the request envelope
//!   (`Store::perf().request(...)`) partitions every write/fsync (and
//!   read) into exclusive phases — the reconciliation identity
//!   `total == Σ phases + residual` is asserted per thread count. The
//!   epoch-mutex convoy it measured drove the two-phase write AND read:
//!   no materialization runs under the epoch mutex.
//! - **Phase-10A**: `ReqGuard` records per-op latency and in-flight
//!   concurrency (diagnostic only; dumped to `stats_file` on unmount).
//! - `ATTR_TTL` / `ENTRY_TTL` are conservative (1 s); Phase 6 tunes
//!   them.
//!
//! # FAILURE MODES
//!
//! Expected: store errors mapped to errno by `errno()` — `ENOSPC` (store
//! full), `EFBIG` / `E2BIG` (limits), `EIO` (missing/corrupt objects,
//! invariant, descriptor, or index failures), `ENOENT` / `ENOTEMPTY` /
//! `EEXIST` / `EISDIR` / `ENOTDIR` (namespace outcomes), `EAGAIN` (lock
//! conflict), `ERANGE` (xattr buffer too small), `EINVAL` / `ENXIO`
//! (lseek / fallocate / rename flags).
//!
//! Must never happen: a double reply; an ack of a write the store did
//! not stage (read-your-writes violation); a synchronous kernel
//! notification from inside a handler (deadlock); an unmount that skips
//! the checkpoint (acknowledged mutations stranded in the log); a
//! poisoned-store panic escaping to the kernel.
//!
//! # HISTORY / EVIDENCE
//!
//! - **Phase-8 (M1)** negotiated `FUSE_WRITEBACK_CACHE | ASYNC_READ |
//!   PARALLEL_DIROPS | BIG_WRITES` with 1 MiB `max_write` (CHANGELOG
//!   8(M1), `d90772c`).
//! - **Phase-10G** removed `FUSE_WRITEBACK_CACHE`: in writeback mode the
//!   kernel flushes dirty pages asynchronously and can interleave READS
//!   between a file's write requests; the epoch overlay is only complete
//!   once every write is staged, so such reads returned partial extents
//!   the kernel then cached — a read-your-writes violation (mount
//!   corruption at chunk boundaries, found by the parallel-workload
//!   court). The mount now runs write-through, and the read handler
//!   serializes with the file's in-flight writes (CHANGELOG 10G item 5).
//!   The full causal story is commented at the `init` negotiation site.
//! - **Phase-10A** added `ReqGuard` / `FuseStats`; **Phase-10D** routed
//!   the write path through the active epoch; **Phase-11B/11C** added
//!   the request envelope and the two-phase read
//!   (`docs/performance/reconciliation.md`; sealed recon courts).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::sync::mpsc::{self, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use fuser::{
    AccessFlags, CopyFileRangeFlags, Errno, FileAttr, FileHandle, Filesystem, INodeNo,
    KernelConfig, LockOwner, Notifier, OpenFlags, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyLock, ReplyOpen, ReplyStatfs, ReplyWrite,
    ReplyXattr, Request, TimeOrNow,
};

use crate::store::inode::Timespec;
use crate::store::transaction::CrashHooks;
use crate::store::{EntryKind, Store, StoreError};

use super::inode as inode_attr;
use super::locking::{self, LockTable};

/// Open-file state: one entry per kernel open, keyed by `fh` in
/// `EntropyFs::handles`.
///
/// Role: handle-lifecycle bookkeeping. Entries are inserted by `open` /
/// `create` / `opendir` and removed by `release` / `releasedir`, so the
/// table's live purpose is to track which handles are still open.
///
/// Invariants:
/// - `ino` is the open inode; `opendir` reuses this table, so it is not
///   necessarily a regular file.
/// - `write` records the open mode's write intent (`O_WRONLY` /
///   `O_RDWR`); advisory only — the kernel enforces access modes via the
///   `DefaultPermissions` mount option.
/// - Handle numbers come from `next_handle` (monotonic u64, never
///   reused within a mount).
///
/// `ino` and `write` are currently diagnostic placeholders
/// (`#[allow(dead_code)]`): they are written but not yet read. Nothing
/// here affects on-disk state.
#[derive(Debug, Clone)]
struct OpenFile {
    #[allow(dead_code)]
    ino: u64,
    #[allow(dead_code)]
    write: bool,
}

/// Kernel-cache invalidation requests, queued to a dedicated thread.
///
/// FUSE notifications must never be sent synchronously from inside a
/// request handler: the kernel's notify path (`fuse_reverse_inval_entry`
/// and friends) can block on locks held by the in-flight request (e.g.
/// the parent directory's `i_mutex` during `unlink`), deadlocking the
/// session. The queue is bounded and best-effort: a dropped invalidation
/// only delays cache freshness until an entry/attr TTL expires — it can
/// never corrupt data (§24).
#[derive(Debug)]
enum NotifyReq {
    /// Parent's cached dirents + attrs changed.
    DirChanged { parent: u64 },
    /// A name was removed from `parent`; drop its dentry + child cache.
    EntryRemoved {
        parent: u64,
        child: u64,
        name: Vec<u8>,
    },
}

/// Maximum queued invalidations (beyond this, requests are dropped;
/// see `NotifyReq` docs).
const NOTIFY_QUEUE_CAPACITY: usize = 4096;

/// Spawn the notifier thread. It drains `rx`, performing kernel
/// notifications outside any FUSE request context, and exits when the
/// filesystem (the sender) is dropped.
fn spawn_notifier(notifier: Arc<Mutex<Option<Notifier>>>) -> mpsc::SyncSender<NotifyReq> {
    let (tx, rx) = mpsc::sync_channel::<NotifyReq>(NOTIFY_QUEUE_CAPACITY);
    std::thread::Builder::new()
        .name("entropyfs-notify".into())
        .spawn(move || {
            for req in rx {
                let Some(n) = notifier.lock().ok().and_then(|g| g.clone()) else {
                    continue;
                };
                match req {
                    NotifyReq::DirChanged { parent } => {
                        // `0, 0` invalidates the whole inode (dirents + attrs).
                        let _ = n.inval_inode(INodeNo(parent), 0, 0);
                    }
                    NotifyReq::EntryRemoved {
                        parent,
                        child,
                        name,
                    } => {
                        let _ = n.inval_inode(INodeNo(parent), 0, 0);
                        let name = std::ffi::OsStr::from_bytes(&name);
                        // FUSE_NOTIFY_DELETE (kernel >= 4.18) drops the
                        // dentry, invalidates the child inode, and signals
                        // inotify; fall back to entry + inode invalidation
                        // on kernels without it.
                        if n.delete(INodeNo(parent), INodeNo(child), name).is_err() {
                            let _ = n.inval_entry(INodeNo(parent), name);
                            let _ = n.inval_inode(INodeNo(child), 0, 0);
                        }
                    }
                }
            }
        })
        .expect("spawn notifier thread");
    tx
}

/// Shared filesystem state: the bridge between the kernel session and the
/// storage engine.
///
/// Role: one instance per mount. Created by `new` / `with_stats`, moved
/// into the `fuser` session by `mount_fs`, and shared with the background
/// optimizer worker through the accessors below (`shared_store`, `ops`,
/// `worker_stop`).
///
/// Invariants:
/// - Handle numbers (`next_handle`) are unique for the lifetime of the
///   mount and never reused.
/// - The handle table and the lock table are independent mutexes, and
///   neither is ever held while a store lock is acquired.
/// - `ops` counts every request that reaches the store (the background
///   worker's idle gate); `worker_stop` is set exactly once, when the
///   instance drops.
/// - Dropping the instance checkpoints the epoch and runs the durability
///   barrier before the store's advisory lock is released (`Drop`).
pub struct EntropyFs {
    /// The store (interior-mutability concurrency: committed-tree reads
    /// traverse immutable state without any lock; writes serialize only
    /// the short commit; ADR-0013 Phase 8). Shared with the background
    /// optimizer worker.
    store: Arc<Store>,
    /// Open file handles: `fh` (u64 handle number, from `next_handle`) →
    /// `OpenFile` state. Inserted by `open` / `create` / `opendir`,
    /// removed by `release` / `releasedir`.
    handles: Mutex<HashMap<u64, OpenFile>>,
    /// Advisory POSIX record locks (`getlk` / `setlk`), keyed by inode +
    /// owner.
    locks: Mutex<LockTable>,
    /// Next handle number. Monotonic u64 (Relaxed ordering suffices:
    /// uniqueness is the only requirement); starts at 1; never reused
    /// within a mount.
    next_handle: std::sync::atomic::AtomicU64,
    /// Operation counter — one increment per request that reaches the
    /// store (`store()`); the background optimizer uses it to detect
    /// idle periods.
    ops: Arc<std::sync::atomic::AtomicU64>,
    /// Set when the filesystem instance is dropped; the background
    /// optimizer worker exits on it (the store lock file must be released
    /// when the session ends).
    worker_stop: Arc<std::sync::atomic::AtomicBool>,
    /// Kernel invalidation queue (see `NotifyReq`); bounded by
    /// `NOTIFY_QUEUE_CAPACITY`.
    notify_tx: mpsc::SyncSender<NotifyReq>,
    /// Notifier handle slot, seeded by the mount once the session exists
    /// (`mount_fs`). `None` before mount and in unit tests; notifications
    /// are then no-ops.
    notifier: Arc<Mutex<Option<Notifier>>>,
    /// Phase-10A FUSE request statistics (diagnostic only).
    stats: Arc<crate::perf::FuseStats>,
    /// Phase-10A: dump the stats render to this file when the instance
    /// drops (the daemon's unmount), `None` = no dump.
    stats_file: Option<std::path::PathBuf>,
}

/// Phase-10A per-request instrumentation: records the op's latency on
/// drop and holds an `InFlight` lease that tracks the session's maximum
/// request concurrency.
///
/// Every handler starts with `let _g = ReqGuard::begin(...)`: the guard
/// lives exactly as long as the handler body, so the recorded latency and
/// concurrency correspond to one kernel request. Diagnostic only — it
/// never affects the reply. On drop, the latency is recorded while the
/// request is still counted in-flight (`_inflight` releases after the
/// explicit `Drop` runs).
struct ReqGuard<'a> {
    stats: &'a crate::perf::FuseStats,
    op: &'static str,
    t0: std::time::Instant,
    _inflight: crate::perf::InFlight<'a>,
}

impl<'a> ReqGuard<'a> {
    fn begin(stats: &'a crate::perf::FuseStats, op: &'static str) -> Self {
        Self {
            stats,
            op,
            t0: std::time::Instant::now(),
            _inflight: crate::perf::InFlight::begin(stats),
        }
    }
}

impl Drop for ReqGuard<'_> {
    fn drop(&mut self) {
        self.stats
            .record_op(self.op, self.t0.elapsed().as_nanos() as u64);
    }
}

impl EntropyFs {
    /// Wrap a store into a filesystem.
    pub fn new(store: Arc<Store>) -> Self {
        Self::with_stats(store, None)
    }

    /// Wrap a store with an optional stats dump on drop (Phase-10A).
    pub fn with_stats(store: Arc<Store>, stats_file: Option<std::path::PathBuf>) -> Self {
        let notifier = Arc::new(Mutex::new(None));
        let notify_tx = spawn_notifier(Arc::clone(&notifier));
        Self {
            store,
            handles: Mutex::new(HashMap::new()),
            locks: Mutex::new(LockTable::new()),
            next_handle: std::sync::atomic::AtomicU64::new(1),
            ops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            worker_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            notify_tx,
            notifier,
            stats: Arc::new(crate::perf::FuseStats::default()),
            stats_file,
        }
    }

    /// The op counter (for the background optimizer's idle detection).
    pub fn ops(&self) -> Arc<std::sync::atomic::AtomicU64> {
        Arc::clone(&self.ops)
    }

    /// The shared store (for the background optimizer worker).
    pub fn shared_store(&self) -> Arc<Store> {
        Arc::clone(&self.store)
    }

    /// The worker stop flag (set when this instance drops).
    pub fn worker_stop(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.worker_stop)
    }

    /// Access the store: increments `ops` (one op per store-touching
    /// request; the background worker's idle gate) and clones the `Arc`.
    fn store(&self) -> Arc<Store> {
        self.ops.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Arc::clone(&self.store)
    }

    /// The notifier slot, for the mount to seed (§24).
    pub fn notifier_slot(&self) -> Arc<Mutex<Option<Notifier>>> {
        Arc::clone(&self.notifier)
    }

    /// Queue a parent-directory cache refresh (dirents + attrs).
    fn notify_dir_changed(&self, parent: u64) {
        self.enqueue(NotifyReq::DirChanged { parent });
    }

    /// Queue a removed-name invalidation: drop the stale dentry and the
    /// child's cached data, and refresh the parent's dirents.
    fn notify_entry_removed(&self, parent: u64, child: u64, name: &[u8]) {
        self.enqueue(NotifyReq::EntryRemoved {
            parent,
            child,
            name: name.to_vec(),
        });
    }

    /// Push a notification; drop it when the queue is full (§24: cache
    /// invalidation is best-effort and never correctness-critical).
    fn enqueue(&self, req: NotifyReq) {
        match self.notify_tx.try_send(req) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                // Full: the notifier thread is blocked in the kernel;
                // dropping is safe (TTLs bound staleness). Disconnected:
                // shutting down; also safe.
            }
        }
    }

    /// Look up an inode's attributes (overlay-aware: the active epoch's
    /// pending inodes are visible before the checkpoint).
    fn get_attr(&self, ino: u64) -> Result<FileAttr, StoreError> {
        let store = self.store();
        let ep = store.epoch();
        let inode = store
            .get_inode_epoch(&ep, ino)?
            .ok_or_else(|| StoreError::Invariant(format!("inode {ino} missing")))?;
        drop(ep);
        Ok(inode_attr::attr_for(&inode, ino))
    }

    /// Map a store error to the FUSE errno the kernel should see.
    ///
    /// `MissingObject` / `MissingChunk` and the integrity classes
    /// (`Invariant` / `Descriptor` / `Index`) become EIO — the hostile-
    /// media court expects a typed rejection, never a panic. `Full` is
    /// ENOSPC (the store rejects BEFORE any partial append, emergency-
    /// reserve check, ADR-0009); `Limit` is EFBIG. Call sites that need a
    /// specific errno (ENOENT / ENOTEMPTY / EEXIST / …) match the
    /// store's invariant-message strings themselves; everything else
    /// falls through to this mapping.
    fn errno(e: &StoreError) -> Errno {
        match e {
            StoreError::MissingObject(_) | StoreError::MissingChunk(_) => Errno::EIO,
            StoreError::Full(_) => Errno::ENOSPC,
            StoreError::Limit(_) => Errno::EFBIG,
            StoreError::Invariant(_) | StoreError::Descriptor(_) | StoreError::Index(_) => {
                Errno::EIO
            }
            _ => Errno::EIO,
        }
    }

    /// Create a new entry under `parent` (Phase-10D epoch path).
    ///
    /// The mode is masked to 0o7777 (permission + setuid/setgid/sticky
    /// bits; the file type is carried by `kind`). On success the parent's
    /// cached dirents + attrs are invalidated (`notify_dir_changed`) so
    /// concurrent readers don't hold a stale view.
    fn create_entry(
        &self,
        parent: u64,
        name: &[u8],
        kind: EntryKind,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Result<u64, StoreError> {
        let store = self.store();
        let entry = crate::store::NewEntry {
            kind,
            mode: mode & 0o7777,
            uid,
            gid,
        };
        let ino = store.epoch_create(parent, name, entry, &CrashHooks::none())?;
        drop(store);
        self.notify_dir_changed(parent);
        Ok(ino)
    }
}

/// Attribute/entry TTLs (conservative; Phase 6 tunes them).
const ATTR_TTL: Duration = Duration::from_secs(1);
const ENTRY_TTL: Duration = Duration::from_secs(1);

/// Unwrap an `INodeNo` to the store's u64 inode number.
fn inon(v: INodeNo) -> u64 {
    v.0
}

impl Drop for EntropyFs {
    fn drop(&mut self) {
        // Tell the background optimizer worker to exit so the store's
        // advisory lock is released when the session ends.
        self.worker_stop
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Phase-10D: unmount must not leave acknowledged mutations in the
        // log — checkpoint the epoch (merge the frozen overlay into the
        // trees with ONE root publication) and make the merged state
        // durable with the barrier (checkpoint + fdatasync + superblock
        // flip + fsync).
        let store = self.store();
        let _ = store.epoch_checkpoint(&crate::store::transaction::CrashHooks::none());
        let _ = store.durability_barrier(&crate::store::transaction::CrashHooks::none());
        // Phase-10A: dump the request/phase instrumentation when the
        // daemon unmounts (the court reads this file for its analysis).
        if let Some(path) = &self.stats_file {
            let mut out = String::new();
            out.push_str(&self.stats.render());
            out.push('\n');
            out.push_str(&self.store.perf().render());
            out.push('\n');
            out.push_str(&self.store.perf().render_reconciled());
            // Phase 12C-1-3: the pressure state-machine trace (the mounted
            // court's causal evidence: enter/leave events, time pressured,
            // deferrals, peak debt — "the state machine fired").
            out.push('\n');
            out.push_str(&self.store.pressure_trace_render());
            // Phase 12C-1-3: the pool's live diagnostics (the engagement
            // court's worker-pool witness: peak in-flight vs capacity —
            // whether the pool actually saturated).
            out.push('\n');
            out.push_str("worker pool:\n");
            let pd = crate::store::workers::POOL.diagnostics();
            out.push_str(&format!(
                "  peak in-flight: {}\n  capacity: {}\n  workers: {}\n",
                pd.peak_in_flight, pd.capacity, pd.workers
            ));
            let _ = std::fs::write(path, out);
        }
    }
}

impl Filesystem for EntropyFs {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> std::io::Result<()> {
        let _g = ReqGuard::begin(&self.stats, "init");
        // Nanosecond timestamps.
        let _ = config.set_time_granularity(Duration::from_nanos(1));
        // Phase-8 write aggregation (§2/§3 of the Phase-8 directive):
        // tune the kernel queue so writeback arrives as large requests
        // (each write() request commits its chunks in ONE transaction).
        let _ = config.set_max_write(1024 * 1024); // 1 MiB per request
        let _ = config.set_max_readahead(1024 * 1024);
        let _ = config.set_max_background(64);
        let _ = config.set_congestion_threshold(48);
        // Phase-10G WRITEBACK-CACHE REMOVAL (the read-your-writes violation).
        //
        // Phase-8 (M1) negotiated FUSE_WRITEBACK_CACHE | ASYNC_READ |
        // PARALLEL_DIROPS | BIG_WRITES with a 1 MiB max_write. The
        // kernel's writeback cache then let it flush dirty pages
        // asynchronously and interleave READ requests between a file's
        // write requests; the daemon's epoch overlay is only complete once
        // every write is staged, so such reads returned partial extents
        // that the kernel then cached — a read-your-writes violation
        // (mount corruption at chunk boundaries). Phase 10G found this in
        // the parallel-workload court and the flag was removed.
        //
        // Write-through (the default without the flag) makes each write()
        // wait for the daemon's ack, so reads always observe fully-staged
        // writes — read-your-writes is restored. Aggregation is preserved
        // via max_write (1 MiB per request), and the read handler ALSO
        // serializes with the file's in-flight writes via the per-inode
        // mutation lock (see `read`).
        let available = config.capabilities();
        let wanted = fuser::InitFlags::FUSE_ASYNC_READ
            | fuser::InitFlags::FUSE_PARALLEL_DIROPS
            | fuser::InitFlags::FUSE_BIG_WRITES;
        let supported = wanted & available;
        let _ = config.add_capabilities(supported);
        Ok(())
    }

    /// Look up a name under `parent` (overlay-aware: pending creates are
    /// visible before the checkpoint).
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &std::ffi::OsStr, reply: ReplyEntry) {
        let _g = ReqGuard::begin(&self.stats, "lookup");
        let name = name.as_bytes();
        let store = self.store();
        let ep = store.epoch();
        let entry = match store.dir_lookup_epoch(&ep, inon(parent), name) {
            Ok(e) => e,
            Err(e) => return reply.error(Self::errno(&e)),
        };
        let entry = match entry {
            Some(e) => e,
            None => return reply.error(Errno::ENOENT),
        };
        let inode = match store.get_inode_epoch(&ep, entry.ino) {
            Ok(Some(i)) => i,
            Ok(None) => return reply.error(Errno::ENOENT),
            Err(e) => return reply.error(Self::errno(&e)),
        };
        drop(ep);
        let attr = inode_attr::attr_for(&inode, entry.ino);
        reply.entry(&ENTRY_TTL, &attr, fuser::Generation(0));
    }

    /// Attributes for an inode (overlay-aware, via `get_attr`).
    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let _g = ReqGuard::begin(&self.stats, "getattr");
        match self.get_attr(ino.0) {
            Ok(attr) => reply.attr(&ATTR_TTL, &attr),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// Attribute update through the epoch (Phase-10D): mode / uid / gid /
    /// size (the truncate path) / times.
    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let _g = ReqGuard::begin(&self.stats, "setattr");
        let atime_ts = atime.map(|t| match t {
            TimeOrNow::SpecificTime(t) => ts_from_system(t),
            TimeOrNow::Now => Timespec::now(),
        });
        let mtime_ts = mtime.map(|t| match t {
            TimeOrNow::SpecificTime(t) => ts_from_system(t),
            TimeOrNow::Now => Timespec::now(),
        });
        let store = self.store();
        let inode = match store.epoch_setattr(
            ino.0,
            &crate::store::AttrUpdate {
                mode,
                uid,
                gid,
                size,
                atime: atime_ts,
                mtime: mtime_ts,
            },
            &CrashHooks::none(),
        ) {
            Ok(i) => i,
            Err(e) => return reply.error(Self::errno(&e)),
        };
        reply.attr(&ATTR_TTL, &inode_attr::attr_for(&inode, ino.0));
    }

    /// Read a symlink target; EINVAL if the inode is not a symlink.
    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        let _g = ReqGuard::begin(&self.stats, "readlink");
        let store = self.store();
        let ep = store.epoch();
        match store.get_inode_epoch(&ep, ino.0) {
            Ok(Some(inode)) => match inode.data {
                crate::store::inode::InodeData::Symlink { target } => reply.data(&target),
                _ => reply.error(Errno::EINVAL),
            },
            Ok(None) => reply.error(Errno::ENOENT),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// mknod: char/block devices pass through as `EntryKind::Device`;
    /// FIFO and socket are unsupported (EOPNOTSUPP).
    fn mknod(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &std::ffi::OsStr,
        mode: u32,
        _umask: u32,
        rdev: u32,
        reply: ReplyEntry,
    ) {
        let _g = ReqGuard::begin(&self.stats, "mknod");
        let file_type = mode & 0o170000;
        let kind = if file_type == crate::store::inode::mode::S_IFCHR {
            EntryKind::Device(true, rdev)
        } else if file_type == crate::store::inode::mode::S_IFBLK {
            EntryKind::Device(false, rdev)
        } else if file_type == crate::store::inode::mode::S_IFIFO
            || file_type == crate::store::inode::mode::S_IFSOCK
        {
            reply.error(Errno::EOPNOTSUPP);
            return;
        } else {
            EntryKind::File
        };
        match self.create_entry(
            inon(parent),
            name.as_bytes(),
            kind,
            mode,
            _req.uid(),
            _req.gid(),
        ) {
            Ok(i) => match self.get_attr(i) {
                Ok(attr) => reply.entry(&ENTRY_TTL, &attr, fuser::Generation(0)),
                Err(e) => reply.error(Self::errno(&e)),
            },
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// Create a directory (epoch path, via `create_entry`).
    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &std::ffi::OsStr,
        mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let _g = ReqGuard::begin(&self.stats, "mkdir");
        match self.create_entry(
            inon(parent),
            name.as_bytes(),
            EntryKind::Directory,
            mode,
            _req.uid(),
            _req.gid(),
        ) {
            Ok(i) => match self.get_attr(i) {
                Ok(attr) => reply.entry(&ENTRY_TTL, &attr, fuser::Generation(0)),
                Err(e) => reply.error(Self::errno(&e)),
            },
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// Unlink a name (epoch path); on success the kernel is notified so
    /// the stale dentry + the child's cache are dropped.
    fn unlink(&self, _req: &Request, parent: INodeNo, name: &std::ffi::OsStr, reply: ReplyEmpty) {
        let _g = ReqGuard::begin(&self.stats, "unlink");
        let store = self.store();
        match store.epoch_unlink(inon(parent), name.as_bytes(), false, &CrashHooks::none()) {
            Ok(child) => {
                drop(store);
                self.notify_entry_removed(inon(parent), child, name.as_bytes());
                reply.ok()
            }
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// Remove a directory (epoch path); the store's "directory not empty"
    /// invariant maps to ENOTEMPTY.
    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &std::ffi::OsStr, reply: ReplyEmpty) {
        let _g = ReqGuard::begin(&self.stats, "rmdir");
        let store = self.store();
        match store.epoch_unlink(inon(parent), name.as_bytes(), true, &CrashHooks::none()) {
            Ok(child) => {
                drop(store);
                self.notify_entry_removed(inon(parent), child, name.as_bytes());
                reply.ok()
            }
            Err(StoreError::Invariant(m)) if m == "directory not empty" => {
                reply.error(Errno::ENOTEMPTY)
            }
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// Create a symlink (epoch path, via `create_entry`).
    fn symlink(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &std::ffi::OsStr,
        link: &std::path::Path,
        reply: ReplyEntry,
    ) {
        let _g = ReqGuard::begin(&self.stats, "symlink");
        match self.create_entry(
            inon(parent),
            name.as_bytes(),
            EntryKind::Symlink(link.as_os_str().as_bytes().to_vec()),
            0o777,
            _req.uid(),
            _req.gid(),
        ) {
            Ok(i) => match self.get_attr(i) {
                Ok(attr) => reply.entry(&ENTRY_TTL, &attr, fuser::Generation(0)),
                Err(e) => reply.error(Self::errno(&e)),
            },
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// Rename (epoch path). Non-zero flags are rejected (EINVAL) — no
    /// RENAME_NOREPLACE / RENAME_EXCHANGE support. On success the source
    /// entry is invalidated; the destination directory is refreshed, or
    /// the overwritten destination entry is invalidated when one was
    /// replaced.
    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &std::ffi::OsStr,
        newparent: INodeNo,
        newname: &std::ffi::OsStr,
        flags: fuser::RenameFlags,
        reply: ReplyEmpty,
    ) {
        let _g = ReqGuard::begin(&self.stats, "rename");
        if flags.bits() != 0 {
            reply.error(Errno::EINVAL);
            return;
        }
        let store = self.store();
        match store.epoch_rename(
            inon(parent),
            name.as_bytes(),
            inon(newparent),
            newname.as_bytes(),
            &CrashHooks::none(),
        ) {
            Ok(outcome) => {
                drop(store);
                self.notify_entry_removed(inon(parent), outcome.src_ino, name.as_bytes());
                // The destination name appears (and an overwritten
                // destination name disappears) in the destination dir.
                if let Some(replaced) = outcome.replaced_dst_ino {
                    self.notify_entry_removed(inon(newparent), replaced, newname.as_bytes());
                } else {
                    self.notify_dir_changed(inon(newparent));
                }
                reply.ok()
            }
            Err(e) => {
                let errno = match &e {
                    StoreError::Invariant(m) => match m.as_str() {
                        "no such entry" => Errno::ENOENT,
                        "cannot rename dir over file" => Errno::ENOTDIR,
                        "cannot rename file over dir" => Errno::EISDIR,
                        "directory not empty" => Errno::ENOTEMPTY,
                        _ => Self::errno(&e),
                    },
                    _ => Self::errno(&e),
                };
                reply.error(errno);
            }
        }
    }

    /// Hard-link `ino` into `newparent` (epoch path); the destination
    /// directory's cache is refreshed.
    fn link(
        &self,
        _req: &Request,
        ino: INodeNo,
        newparent: INodeNo,
        newname: &std::ffi::OsStr,
        reply: ReplyEntry,
    ) {
        let _g = ReqGuard::begin(&self.stats, "link");
        let store = self.store();
        match store.link(
            inon(newparent),
            newname.as_bytes(),
            ino.0,
            &CrashHooks::none(),
        ) {
            Ok(()) => {
                drop(store);
                self.notify_dir_changed(inon(newparent));
                match self.get_attr(ino.0) {
                    Ok(attr) => reply.entry(&ENTRY_TTL, &attr, fuser::Generation(0)),
                    Err(e) => reply.error(Self::errno(&e)),
                }
            }
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// Open a file: allocate a handle, record the write intent, reply.
    ///
    /// Non-files are rejected with EISDIR. The handle-table insert is
    /// best-effort (`if let Ok`): a poisoned table never fails the open —
    /// the table's operational role is lifecycle bookkeeping only
    /// (see `OpenFile`).
    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let _g = ReqGuard::begin(&self.stats, "open");
        let store = self.store();
        let ep = store.epoch();
        let inode = match store.get_inode_epoch(&ep, ino.0) {
            Ok(Some(i)) => i,
            Ok(None) => return reply.error(Errno::ENOENT),
            Err(e) => return reply.error(Self::errno(&e)),
        };
        if !inode.is_file() {
            return reply.error(Errno::EISDIR);
        }
        drop(ep);
        let fh = self
            .next_handle
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let write = matches!(
            flags.acc_mode(),
            fuser::OpenAccMode::O_WRONLY | fuser::OpenAccMode::O_RDWR
        );
        if let Ok(mut h) = self.handles.lock() {
            h.insert(fh, OpenFile { ino: ino.0, write });
        }
        reply.opened(FileHandle(fh), fuser::FopenFlags::empty());
    }

    /// Overlay-aware file read — the Phase-11C TWO-PHASE read.
    ///
    /// # What
    ///
    /// Returns up to `size` bytes (u32, bytes) at `offset` (u64, byte
    /// offset) to the kernel, observing both the committed extents and the
    /// epoch's pending writes. An empty window (read at/ past EOF, or a
    /// hole with no covering extent) replies with zero bytes — the kernel
    /// reads a short reply as EOF.
    ///
    /// # Why two phases
    ///
    /// The request envelope (`perf().request("fuse_read")`) attaches this
    /// read to the Phase-11B reconciliation accounting; the read-leaf rows
    /// (`read_scan` / `read_deps` / `read_prefetch` / `read_decode`)
    /// partition it, so the identity `total == Σ phases + residual` holds
    /// for reads too. The epoch guard is held ONLY for the PREPARE half —
    /// extent collection + dependency enumeration + the batched object
    /// fetch (`read_file_epoch_prepare`, the only half that needs the
    /// overlay) — and dropped before the pure-CPU DECODE half
    /// (`materialize_decode`). No materialization ever runs under the
    /// epoch mutex, so reads never hold it while their decode waits on the
    /// worker semaphore and writers are not convoyed behind read decodes
    /// (Phase-11C, docs/performance/reconciliation.md §3.4). The prepared
    /// read owns every fetched object and nested descriptor — that
    /// ownership is what makes guard-free decode safe.
    ///
    /// # Concurrency
    ///
    /// The per-inode mutation lock is taken BEFORE the epoch guard (the
    /// same order as `Store::epoch_write`) and held across BOTH halves —
    /// see the lock comment in the body.
    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let _g = ReqGuard::begin(&self.stats, "read");
        let store = self.store();
        // Phase-11B: the request envelope for this read (the read phases
        // partition it; a store-internal read without an outer envelope
        // would open its own).
        let _req = store.perf().request("fuse_read");
        // Serialize with the file's in-flight epoch writes: the kernel can
        // interleave read requests between a file's write requests (the
        // write-through mode makes the kernel SEND them only after a
        // write() ack, but concurrent session threads can still deliver a
        // read while a previous write is mid-stage), and the overlay is
        // only complete once every write is staged. The per-inode
        // mutation lock (also held by `epoch_write`) closes that window
        // (Phase-10G: the parallel-workload court hit read-before-write
        // state — partial extents — that the kernel then cached; the
        // writeback-cache removal story is at `init`).
        let _lock = store.inode_lock(ino.0);
        // Phase-11C: TWO-PHASE read — the epoch guard is held only for the
        // prepare half (extent collection + dependency enumeration + the
        // batched object fetch); the decode half runs without it, so a
        // read never holds the epoch mutex while its decode waits on the
        // worker semaphore (and writers are not convoyed behind read
        // decodes). The per-inode lock above is held across both halves.
        let prepared = {
            let ep = store.epoch();
            match store.read_file_epoch_prepare(&ep, ino.0, offset, size as u64) {
                Ok(p) => p,
                Err(e) => {
                    reply.error(Self::errno(&e));
                    return;
                }
            }
        }; // the epoch guard drops here — DECODE runs without it
        let data = match prepared {
            Some(p) => match store.materialize_decode(p) {
                Ok(d) => d,
                Err(e) => {
                    reply.error(Self::errno(&e));
                    return;
                }
            },
            // No covering extent (past EOF / hole): zero bytes = EOF.
            None => Vec::new(),
        };
        reply.data(&data);
    }

    /// Write-through epoch write: stage `data` at `offset` into the
    /// active epoch and ack.
    ///
    /// # What / why
    ///
    /// The kernel-facing acknowledgement point of the write path
    /// (Phase-10D): `epoch_write` appends the staged records + the
    /// `MUTATION_LOG` envelope and flushes to the page cache BEFORE this
    /// reply, so a successful `reply.written` means the write is visible
    /// to subsequent reads (read-your-writes) and survives a process
    /// crash. Write-through mode (no `FUSE_WRITEBACK_CACHE` — see `init`)
    /// is what makes that ordering observable to the kernel.
    ///
    /// # Accounting
    ///
    /// The request envelope partitions the request: the exclusive rows
    /// inside `epoch_write` (inode_lock_wait … checkpoint) attach here;
    /// the residual row is the FUSE/scheduler/other overhead (Phase-11B).
    /// The write size feeds the Phase-10A histogram.
    fn write(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: fuser::WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        let _g = ReqGuard::begin(&self.stats, "write");
        self.stats.record_write_size(data.len());
        let store = self.store();
        // Phase-11B: the request envelope. The exclusive partition rows
        // inside `epoch_write` (inode_lock_wait … checkpoint) attach here;
        // the residual row is the FUSE/scheduler/other overhead.
        let _req = store.perf().request("fuse_write");
        // Phase-10D: the write goes through the ACTIVE EPOCH (log append
        // + ack; the trees merge at the checkpoint). The store's configured
        // foreground policy applies.
        let opts = crate::optimizer::policy::OptimizeOptions::default();
        let fg = store.foreground_policy();
        match store.epoch_write(ino.0, offset, data, opts, fg, &CrashHooks::none()) {
            Ok(()) => reply.written(data.len() as u32),
            Err(StoreError::Full(_)) => reply.error(Errno::ENOSPC),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// FUSE flush (one `close()` of the fd, including duplicated fds):
    /// release the closing owner's POSIX record locks.
    ///
    /// Locks die with the fd, so they are dropped here rather than in
    /// `release` (the fh's final call).
    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        let _g = ReqGuard::begin(&self.stats, "flush");
        if let Ok(mut locks) = self.locks.lock() {
            locks.release_owner(lock_owner.0);
        }
        reply.ok();
    }

    /// Final close of the handle: remove the `fh` entry and, when the
    /// kernel reports a lock owner, release its locks.
    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let _g = ReqGuard::begin(&self.stats, "release");
        if let Ok(mut h) = self.handles.lock() {
            h.remove(&fh.0);
        }
        if let Some(owner) = lock_owner {
            if let Ok(mut locks) = self.locks.lock() {
                locks.release_owner(owner.0);
            }
        }
        reply.ok();
    }

    fn fsync(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        let _g = ReqGuard::begin(&self.stats, "fsync");
        // Durability barrier (Phase 6): makes deferred writes AND the
        // epoch's acknowledged mutations power-durable (ADR-0008: records
        // → fdatasync → superblock flip → fsync). The barrier first
        // checkpoints the epoch (Phase-10D — the acknowledged-but-
        // uncheckpointed mutations are merged and their commit is covered
        // by this barrier's fsync; a no-op when the epoch is empty), then
        // serializes the physical cut under the commit lock — the "fsync
        // convoy" the 11B/11C accounting measured as `commit_lock_wait`
        // (contract-inherent: a commit completing mid-barrier would break
        // write→fsync durability linearizability). v1 applies the full
        // barrier for both FSYNC and FDATASYNC (data and metadata are
        // interleaved in the segments).
        let store = self.store();
        // Phase-11B: the barrier's exclusive rows partition this request.
        let _req = store.perf().request("fuse_fsync");
        match store.durability_barrier(&CrashHooks::none()) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// Open a directory: allocate a handle like `open`; non-directories
    /// are ENOTDIR.
    fn opendir(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let _g = ReqGuard::begin(&self.stats, "opendir");
        let store = self.store();
        let ep = store.epoch();
        match store.get_inode_epoch(&ep, ino.0) {
            Ok(Some(i)) if i.is_dir() => {
                drop(ep);
                drop(store);
                let fh = self
                    .next_handle
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if let Ok(mut h) = self.handles.lock() {
                    h.insert(
                        fh,
                        OpenFile {
                            ino: ino.0,
                            write: false,
                        },
                    );
                }
                reply.opened(FileHandle(fh), fuser::FopenFlags::empty());
            }
            Ok(Some(_)) => reply.error(Errno::ENOTDIR),
            Ok(None) => reply.error(Errno::ENOENT),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// Overlay-aware directory listing: pending creates/removes are
    /// visible before the checkpoint. `offset` is the 1-based resume
    /// cursor (`idx + 1`), so a kernel-buffer-full reply resumes at the
    /// next entry.
    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let _g = ReqGuard::begin(&self.stats, "readdir");
        let store = self.store();
        let ep = store.epoch();
        // Overlay-aware: pending creates/removes are visible before the
        // checkpoint.
        let res = (|| -> Result<(), StoreError> {
            let all = crate::fuse::directory::entry_list_epoch(&store, &ep, ino.0)?;
            let mut idx = offset as usize;
            while idx < all.len() {
                let (e_ino, d_type, name) = &all[idx];
                let os_name = std::ffi::OsStr::from_bytes(name);
                if reply.add(
                    fuser::INodeNo(*e_ino),
                    (idx + 1) as u64,
                    crate::fuse::directory::file_type_for(*d_type),
                    os_name,
                ) {
                    return Ok(()); // kernel buffer full; resume at idx+1
                }
                idx += 1;
            }
            Ok(())
        })();
        match res {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// Close a directory handle: remove the `fh` entry.
    fn releasedir(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        reply: ReplyEmpty,
    ) {
        let _g = ReqGuard::begin(&self.stats, "releasedir");
        if let Ok(mut h) = self.handles.lock() {
            h.remove(&fh.0);
        }
        reply.ok();
    }

    /// Capacity/usage from the PHYSICAL backing store (§22): never
    /// advertise more capacity than the device; free is saturating.
    /// 512-byte blocks, 4096-byte block size, name length 255.
    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let _g = ReqGuard::begin(&self.stats, "statfs");
        let store = self.store();
        let capacity = store.physical_capacity();
        let used = store.physical_used();
        let free = capacity.saturating_sub(used);
        // Conservative v1 accounting (§22): never advertise more capacity
        // than the physical backing store.
        reply.statfs(
            capacity.div_ceil(512),
            free.div_ceil(512),
            free.div_ceil(512),
            0,
            0,
            4096,
            255,
            4096,
        );
    }

    /// Set an xattr: unsupported names are rejected up front (EOPNOTSUPP);
    /// store-limit violations become E2BIG.
    fn setxattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        name: &std::ffi::OsStr,
        value: &[u8],
        _flags: i32,
        _position: u32,
        reply: ReplyEmpty,
    ) {
        let _g = ReqGuard::begin(&self.stats, "setxattr");
        if !super::xattr::supported_name(name.as_bytes()) {
            return reply.error(Errno::EOPNOTSUPP);
        }
        let store = self.store();
        match store.set_xattr(ino.0, name.as_bytes(), value, &CrashHooks::none()) {
            Ok(()) => reply.ok(),
            Err(StoreError::Limit(_)) => reply.error(Errno::E2BIG),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// Get an xattr. `size == 0` is the kernel's probe protocol: reply
    /// the value length; a too-small buffer is ERANGE; absent is ENODATA.
    fn getxattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        name: &std::ffi::OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        let _g = ReqGuard::begin(&self.stats, "getxattr");
        let store = self.store();
        match store.get_xattr(ino.0, name.as_bytes()) {
            Ok(Some(value)) => {
                if size == 0 {
                    reply.size(value.len() as u32);
                } else if (value.len() as u32) > size {
                    reply.error(Errno::ERANGE);
                } else {
                    reply.data(&value);
                }
            }
            Ok(None) => reply.error(Errno::ENODATA),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// List xattr names (NUL-terminated); same probe/ERANGE protocol as
    /// `getxattr`.
    fn listxattr(&self, _req: &Request, ino: INodeNo, size: u32, reply: ReplyXattr) {
        let _g = ReqGuard::begin(&self.stats, "listxattr");
        let store = self.store();
        match store.list_xattr(ino.0) {
            Ok(names) => {
                let mut buf = Vec::new();
                for n in names {
                    buf.extend_from_slice(&n);
                    buf.push(0);
                }
                if size == 0 {
                    reply.size(buf.len() as u32);
                } else if (buf.len() as u32) > size {
                    reply.error(Errno::ERANGE);
                } else {
                    reply.data(&buf);
                }
            }
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// Remove an xattr; ENODATA when absent.
    fn removexattr(&self, _req: &Request, ino: INodeNo, name: &std::ffi::OsStr, reply: ReplyEmpty) {
        let _g = ReqGuard::begin(&self.stats, "removexattr");
        let store = self.store();
        match store.remove_xattr(ino.0, name.as_bytes(), &CrashHooks::none()) {
            Ok(true) => reply.ok(),
            Ok(false) => reply.error(Errno::ENODATA),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// Kernel access check: existence only. The real permission decision
    /// is the kernel's (`DefaultPermissions` mount option + the mode
    /// served by `getattr`).
    fn access(&self, _req: &Request, ino: INodeNo, _mask: AccessFlags, reply: ReplyEmpty) {
        let _g = ReqGuard::begin(&self.stats, "access");
        let store = self.store();
        let ep = store.epoch();
        match store.get_inode_epoch(&ep, ino.0) {
            Ok(Some(_)) => reply.ok(),
            Ok(None) => reply.error(Errno::ENOENT),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// Atomic create + open: create the file in the epoch, then hand back
    /// an open handle (write intent). The store's "entry already exists"
    /// invariant maps to EEXIST.
    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &std::ffi::OsStr,
        mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let _g = ReqGuard::begin(&self.stats, "create");
        match self.create_entry(
            inon(parent),
            name.as_bytes(),
            EntryKind::File,
            mode,
            _req.uid(),
            _req.gid(),
        ) {
            Ok(i) => {
                let store = self.store();
                let ep = store.epoch();
                let attr = match store.get_inode_epoch(&ep, i) {
                    Ok(Some(inode)) => inode_attr::attr_for(&inode, i),
                    _ => return reply.error(Errno::EIO),
                };
                drop(ep);
                drop(store);
                let fh = self
                    .next_handle
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if let Ok(mut h) = self.handles.lock() {
                    h.insert(
                        fh,
                        OpenFile {
                            ino: i,
                            write: true,
                        },
                    );
                }
                reply.created(
                    &ENTRY_TTL,
                    &attr,
                    fuser::Generation(0),
                    FileHandle(fh),
                    fuser::FopenFlags::empty(),
                );
            }
            Err(StoreError::Invariant(m)) if m == "entry already exists" => {
                reply.error(Errno::EEXIST)
            }
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    /// Test a POSIX record lock (F_GETLK): the conflicting lock, or the
    /// request range as F_UNLCK when free.
    fn getlk(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        lock_owner: LockOwner,
        start: u64,
        end: u64,
        typ: i32,
        pid: u32,
        reply: ReplyLock,
    ) {
        let _g = ReqGuard::begin(&self.stats, "getlk");
        let locks = self.locks.lock().expect("locks poisoned");
        match locks.getlk(ino.0, lock_owner.0, start, end, typ) {
            Some(l) => reply.locked(l.start, l.end, l.typ, l.pid),
            None => reply.locked(start, end, locking::F_UNLCK, pid),
        }
    }

    /// Acquire/release a POSIX record lock (F_SETLK, non-blocking):
    /// EAGAIN on conflict, EINVAL on an invalid range/type. F_SETLKW is
    /// not implemented.
    fn setlk(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        lock_owner: LockOwner,
        start: u64,
        end: u64,
        typ: i32,
        pid: u32,
        _sleep: bool,
        reply: ReplyEmpty,
    ) {
        let _g = ReqGuard::begin(&self.stats, "setlk");
        let mut locks = self.locks.lock().expect("locks poisoned");
        match locks.setlk(ino.0, lock_owner.0, start, end, typ, pid) {
            Ok(true) => reply.ok(),
            Ok(false) => reply.error(Errno::EAGAIN),
            Err(_) => reply.error(Errno::EINVAL),
        }
    }

    /// Allocate or punch: only FALLOC_FL_KEEP_SIZE and
    /// FALLOC_FL_PUNCH_HOLE are supported; any other mode is EOPNOTSUPP.
    /// A punch goes to `Store::punch_hole`; a non-KEEP_SIZE allocation
    /// extends the inode size through `setattr_inode` only when the range
    /// grows past EOF.
    fn fallocate(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        length: u64,
        mode: i32,
        reply: ReplyEmpty,
    ) {
        let _g = ReqGuard::begin(&self.stats, "fallocate");
        // FALLOC_FL_KEEP_SIZE = 0x01, PUNCH_HOLE = 0x02.
        let keep_size = mode & 0x01 != 0;
        let punch_hole = mode & 0x02 != 0;
        if mode & !0x03 != 0 {
            return reply.error(Errno::EOPNOTSUPP);
        }
        let store = self.store();
        let end = offset.saturating_add(length);
        if punch_hole {
            match store.punch_hole(ino.0, offset, end, keep_size) {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(Self::errno(&e)),
            }
            return;
        }
        if !keep_size {
            let store = self.store();
            let ep = store.epoch();
            let inode = match store.get_inode_epoch(&ep, ino.0) {
                Ok(Some(i)) => i,
                _ => return reply.error(Errno::ENOENT),
            };
            if end > inode.size {
                match store.setattr_inode(
                    ino.0,
                    &crate::store::AttrUpdate {
                        size: Some(end),
                        ..Default::default()
                    },
                    &CrashHooks::none(),
                ) {
                    Ok(_) => reply.ok(),
                    Err(e) => reply.error(Self::errno(&e)),
                }
            } else {
                reply.ok();
            }
        } else {
            reply.error(Errno::EOPNOTSUPP);
        }
    }

    /// SEEK_DATA / SEEK_HOLE, v1-conservative: SEEK_DATA returns
    /// `offset` for any in-range offset (next-data is a Phase-6
    /// refinement); SEEK_HOLE returns the file size — the tail past the
    /// last extent is a hole. Offsets past EOF are ENXIO.
    fn lseek(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: i64,
        whence: i32,
        reply: fuser::ReplyLseek,
    ) {
        let _g = ReqGuard::begin(&self.stats, "lseek");
        // SEEK_DATA = 3, SEEK_HOLE = 4.
        if offset < 0 {
            return reply.error(Errno::EINVAL);
        }
        let offset = offset as u64;
        let store = self.store();
        let ep = store.epoch();
        let inode = match store.get_inode_epoch(&ep, ino.0) {
            Ok(Some(i)) => i,
            _ => return reply.error(Errno::ENOENT),
        };
        let size = inode.size;
        if offset > size {
            return reply.error(Errno::ENXIO);
        }
        match whence {
            3 => {
                // SEEK_DATA: v1 returns `offset` when within the file
                // (conservative; next-data is a Phase 6 refinement).
                if offset < size {
                    reply.offset(offset as i64);
                } else {
                    reply.error(Errno::ENXIO);
                }
            }
            4 => {
                // SEEK_HOLE: the tail past the last extent is a hole.
                reply.offset(size as i64);
            }
            _ => reply.error(Errno::EINVAL),
        }
    }

    /// Server-side copy within the store (no kernel data bounce):
    /// `Store::copy_range` returns the number of bytes written.
    fn copy_file_range(
        &self,
        _req: &Request,
        ino_in: INodeNo,
        _fh_in: FileHandle,
        offset_in: u64,
        ino_out: INodeNo,
        _fh_out: FileHandle,
        offset_out: u64,
        len: u64,
        _flags: CopyFileRangeFlags,
        reply: ReplyWrite,
    ) {
        let _g = ReqGuard::begin(&self.stats, "copy_file_range");
        let store = self.store();
        match store.copy_range(ino_in.0, offset_in, ino_out.0, offset_out, len) {
            Ok(n) => reply.written(n as u32),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }
}

/// Convert a `SystemTime` to the store timestamp type.
fn ts_from_system(t: SystemTime) -> Timespec {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => Timespec {
            sec: d.as_secs(),
            nsec: d.subsec_nanos(),
        },
        Err(_) => Timespec::now(),
    }
}

/// Helper used by tests: inode number unwrap.
pub fn ino_of(v: INodeNo) -> u64 {
    v.0
}
