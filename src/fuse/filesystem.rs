//! The `fuser::Filesystem` implementation (ADR-0002).
//!
//! Converts FUSE operations into storage-engine transactions. Contains no
//! entropy algorithms; every representation decision lives in the engine.
//!
//! Concurrency model (§25): the store is the single write coordinator
//! behind one mutex; reads and writes are serialized on it in v1
//! (correctness first — multi-threaded session threads contend on the
//! store mutex; profiling-driven refinement lands in Phase 6). Lock
//! ordering: the store mutex is never held while taking the handle or
//! lock tables.

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

use super::directory;
use super::inode as inode_attr;
use super::locking::{self, LockTable};

/// Open file state (the handle table).
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

/// Shared filesystem state.
pub struct EntropyFs {
    /// The store (interior-mutability concurrency: reads traverse
    /// immutable state without any lock; writes serialize only the short
    /// commit; ADR-0013 Phase 8). Shared with the background optimizer
    /// worker.
    store: Arc<Store>,
    /// Open file handles: fh → state.
    handles: Mutex<HashMap<u64, OpenFile>>,
    /// Advisory POSIX record locks.
    locks: Mutex<LockTable>,
    /// Next handle number.
    next_handle: std::sync::atomic::AtomicU64,
    /// Operation counter (any request = one op); the background
    /// optimizer uses it to detect idle periods.
    ops: Arc<std::sync::atomic::AtomicU64>,
    /// Set when the filesystem instance is dropped; the background
    /// optimizer worker exits on it (the store lock file must be released
    /// when the session ends).
    worker_stop: Arc<std::sync::atomic::AtomicBool>,
    /// Kernel invalidation queue (see `NotifyReq`).
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

/// Phase-10A per-request guard: records latency + concurrency on drop.
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

    /// Access the store (counts the request for idle detection).
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

    /// Look up an inode's attributes.
    fn get_attr(&self, ino: u64) -> Result<FileAttr, StoreError> {
        let store = self.store();
        let inode = store
            .get_inode(ino)?
            .ok_or_else(|| StoreError::Invariant(format!("inode {ino} missing")))?;
        Ok(inode_attr::attr_for(&inode, ino))
    }

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

    /// Create a new entry under `parent`.
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
        let ino = store.create_entry(parent, name, entry, &CrashHooks::none())?;
        drop(store);
        self.notify_dir_changed(parent);
        Ok(ino)
    }
}

/// Attribute/entry TTLs (conservative; Phase 6 tunes them).
const ATTR_TTL: Duration = Duration::from_secs(1);
const ENTRY_TTL: Duration = Duration::from_secs(1);

fn inon(v: INodeNo) -> u64 {
    v.0
}

impl Drop for EntropyFs {
    fn drop(&mut self) {
        // Tell the background optimizer worker to exit so the store's
        // advisory lock is released when the session ends.
        self.worker_stop
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Phase-10A: dump the request/phase instrumentation when the
        // daemon unmounts (the court reads this file for its analysis).
        if let Some(path) = &self.stats_file {
            let mut out = String::new();
            out.push_str(&self.stats.render());
            out.push_str("\n");
            out.push_str(&self.store.perf().render());
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
        // Writeback cache: the kernel aggregates tiny application writes
        // into large write() requests and serves reads from its page
        // cache, so read-your-writes never depends on the daemon's commit
        // cadence. Async reads and parallel directory ops make the
        // lock-free read path (Phase 8) reachable from multiple kernel
        // threads. If the kernel does not offer writeback, we continue
        // without it (correctness is unaffected; only aggregation is
        // lost).
        let available = config.capabilities();
        let wanted = fuser::InitFlags::FUSE_WRITEBACK_CACHE
            | fuser::InitFlags::FUSE_ASYNC_READ
            | fuser::InitFlags::FUSE_PARALLEL_DIROPS
            | fuser::InitFlags::FUSE_BIG_WRITES;
        let supported = wanted & available;
        let _ = config.add_capabilities(supported);
        if !supported.contains(fuser::InitFlags::FUSE_WRITEBACK_CACHE) {
            log::warn!(
                "entropyfs: kernel does not offer FUSE_WRITEBACK_CACHE; write aggregation degraded"
            );
        }
        Ok(())
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &std::ffi::OsStr, reply: ReplyEntry) {
        let _g = ReqGuard::begin(&self.stats, "lookup");
        let name = name.as_bytes();
        let store = self.store();
        let entry = match store.dir_lookup(inon(parent), name) {
            Ok(e) => e,
            Err(e) => return reply.error(Self::errno(&e)),
        };
        let entry = match entry {
            Some(e) => e,
            None => return reply.error(Errno::ENOENT),
        };
        let inode = match store.get_inode(entry.ino) {
            Ok(Some(i)) => i,
            Ok(None) => return reply.error(Errno::ENOENT),
            Err(e) => return reply.error(Self::errno(&e)),
        };
        let attr = inode_attr::attr_for(&inode, entry.ino);
        reply.entry(&ENTRY_TTL, &attr, fuser::Generation(0));
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let _g = ReqGuard::begin(&self.stats, "getattr");
        match self.get_attr(ino.0) {
            Ok(attr) => reply.attr(&ATTR_TTL, &attr),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

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
        let inode = match store.setattr_inode(
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

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        let _g = ReqGuard::begin(&self.stats, "readlink");
        let store = self.store();
        match store.get_inode(ino.0) {
            Ok(Some(inode)) => match inode.data {
                crate::store::inode::InodeData::Symlink { target } => reply.data(&target),
                _ => reply.error(Errno::EINVAL),
            },
            Ok(None) => reply.error(Errno::ENOENT),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

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

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &std::ffi::OsStr, reply: ReplyEmpty) {
        let _g = ReqGuard::begin(&self.stats, "unlink");
        let store = self.store();
        match store.unlink(inon(parent), name.as_bytes(), false, &CrashHooks::none()) {
            Ok(child) => {
                drop(store);
                self.notify_entry_removed(inon(parent), child, name.as_bytes());
                reply.ok()
            }
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &std::ffi::OsStr, reply: ReplyEmpty) {
        let _g = ReqGuard::begin(&self.stats, "rmdir");
        let store = self.store();
        match store.unlink(inon(parent), name.as_bytes(), true, &CrashHooks::none()) {
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
        match store.rename(
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

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let _g = ReqGuard::begin(&self.stats, "open");
        let store = self.store();
        let inode = match store.get_inode(ino.0) {
            Ok(Some(i)) => i,
            Ok(None) => return reply.error(Errno::ENOENT),
            Err(e) => return reply.error(Self::errno(&e)),
        };
        if !inode.is_file() {
            return reply.error(Errno::EISDIR);
        }
        drop(store);
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
        match store.read_file(ino.0, offset, size as u64) {
            Ok(data) => reply.data(&data),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

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
        match store.write_region(ino.0, offset, data) {
            Ok(()) => reply.written(data.len() as u32),
            Err(StoreError::Full(_)) => reply.error(Errno::ENOSPC),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

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
        // Durability barrier (Phase 6): deferred writes become
        // power-durable here (ADR-0008: records → fdatasync → superblock
        // flip → fsync). v1 applies the full barrier for both FSYNC and
        // FDATASYNC (data and metadata are interleaved in the segments).
        let store = self.store();
        match store.durability_barrier(&CrashHooks::none()) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    fn opendir(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let _g = ReqGuard::begin(&self.stats, "opendir");
        let store = self.store();
        match store.get_inode(ino.0) {
            Ok(Some(i)) if i.is_dir() => {
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
        match directory::fill_reply(&store, ino.0, offset, &mut reply) {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

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

    fn removexattr(&self, _req: &Request, ino: INodeNo, name: &std::ffi::OsStr, reply: ReplyEmpty) {
        let _g = ReqGuard::begin(&self.stats, "removexattr");
        let store = self.store();
        match store.remove_xattr(ino.0, name.as_bytes(), &CrashHooks::none()) {
            Ok(true) => reply.ok(),
            Ok(false) => reply.error(Errno::ENODATA),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

    fn access(&self, _req: &Request, ino: INodeNo, _mask: AccessFlags, reply: ReplyEmpty) {
        let _g = ReqGuard::begin(&self.stats, "access");
        let store = self.store();
        match store.get_inode(ino.0) {
            Ok(Some(_)) => reply.ok(),
            Ok(None) => reply.error(Errno::ENOENT),
            Err(e) => reply.error(Self::errno(&e)),
        }
    }

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
                let attr = match store.get_inode(i) {
                    Ok(Some(inode)) => inode_attr::attr_for(&inode, i),
                    _ => return reply.error(Errno::EIO),
                };
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
            let inode = match store.get_inode(ino.0) {
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
        let inode = match store.get_inode(ino.0) {
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
