# ADR-0019: FUSE kernel-cache invalidation from a dedicated notifier thread

**Status:** accepted · **Date:** 2026-08-25

## Context

Phase 3 live testing (git clone on a real mount) exposed two related
kernel-cache failures:

1. After `unlink`/`rmdir`, the kernel dcache and the per-directory readdir
   page cache kept stale entries: `ls -la` showed ghost `config.lock`
   entries (`-?????????`), and `rmdir` returned `ENOTEMPTY` because
   `shrink_dcache_parent` refused to prune the busy stale dentry — even
   though the store was correct and `lookup` already returned `ENOENT`.
2. Sending `FUSE_NOTIFY_INVAL_ENTRY` synchronously from inside the
   `unlink` request handler **deadlocked the entire session**: the kernel's
   `fuse_reverse_inval_entry` path blocks on locks held by the in-flight
   `unlink` request (observed in-kernel as the daemon thread wedged in
   `fuse_reverse_inval_entry`, with every subsequent request queued
   behind it).

The kernel's view of a directory (dcache + readdir page cache) is
independent of the store. `ReplyDirectory::add` (plain readdir) carries no
per-entry TTL, so the kernel caches dirents until invalidated. Without
explicit invalidation, removed names linger.

## Decision

- **All kernel notifications are queued to a dedicated `entropyfs-notify`
  thread** (`mpsc::sync_channel`, capacity 4096) that performs the actual
  `Notifier` calls. No notification is ever sent from inside a FUSE
  request handler — the kernel notify path can block on locks held by the
  in-flight request.
- On every directory mutation the parent's cached dirents + attrs are
  invalidated (`inval_inode(parent, 0, 0)`), and for removed names the
  stale dentry + child inode are dropped via `FUSE_NOTIFY_DELETE`
  (kernel >= 4.18; drops the dentry, invalidates the child, signals
  inotify), falling back to `inval_entry` + `inval_inode` on kernels
  without it.
- The queue is **bounded and best-effort**: a dropped invalidation
  (`try_send` full/disconnected) only delays cache freshness until an
  entry/attr TTL expires (1 s in v1). Invalidation can never corrupt data
  (§24); it is a performance/observability optimization.
- The notifier slot is seeded by the mount after `spawn_mount`
  (`BackgroundSession::notifier()`); unit tests and pre-mount operation
  simply no-op.

## Consequences

- Mutating handlers (`create`, `mknod`, `mkdir`, `symlink`, `link`,
  `unlink`, `rmdir`, `rename`) queue invalidations after their store
  transaction commits; `rename` invalidates both the source name and the
  destination name (and the replaced destination, if any).
- The notifier thread lives for the lifetime of the filesystem instance
  and exits when the sender is dropped.
- Phase 6 may tune TTLs or switch to readdirplus; the invalidation
  discipline remains.
