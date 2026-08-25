# Linux / FUSE / CachyOS research report (2026-08-25)

Phase 0 deliverable #1. State of the platform EntropyFS targets, verified
locally where possible; items that cannot be verified from this host are
explicitly marked.

## 1. Target host facts (verified 2026-08-25)

| Item | Value |
|------|-------|
| Kernel | `7.2.0-1-cachyos` (CachyOS, x86_64, `PREEMPT_DYNAMIC`) |
| `fusermount3` | 3.18.2 |
| `/dev/fuse` | present |
| FUSE module | builtin (`CONFIG_FUSE_FS=y`); `fuse`, `fuseblk`, `fusectl` in `/proc/filesystems` |
| `CONFIG_FUSE_DAX` | `y` |
| `CONFIG_FUSE_PASSTHROUGH` | `y` (passthrough is present in this kernel; EntropyFS does not depend on it, ADR-0002/0014) |
| `CONFIG_FUSE_IO_URING` | `y`, gated by module param `enable_uring` (experimental; EntropyFS does not build on it, ADR-0002) |
| rustc / cargo | 1.98.0 stable |

Implications: a normal CachyOS user can mount EntropyFS with zero kernel
changes. `/dev/fuse` exists; `fusermount3` handles setuid mounting.

## 2. FUSE kernel ABI status (as targeted by `fuser` 0.18.0)

`fuser` 0.18.0 (published 2026-07-22) targets the current stable FUSE ABI.
Observations from the crate:

- Default build is pure Rust talking to `/dev/fuse` directly — no `libfuse`
  dependency (`libfuse` is an opt-in feature for those who want it). This
  removes a build-time C dependency from the data path.
- The `abi-7-*` feature gates of older `fuser` releases are gone in 0.18.0;
  the crate speaks the modern ABI surface: `FUSE_READDIRPLUS`,
  `FUSE_ATOMIC_O_TRUNC`, `FUSE_EXPORT_SUPPORT`, `FUSE_DONT_MASK`,
  `FUSE_SPLICE_WRITE/READ`, `FUSE_FLOCK_LOCKS`, `FUSE_HAS_IOCTL_DIR`,
  `FUSE_AUTO_INVAL_DATA`, `FUSE_DO_READDIRPLUS`, `FUSE_READDIRPLUS_AUTO`,
  `FUSE_ASYNC_READ`, `FUSE_PARALLEL_DIROPS`, `FUSE_HANDLE_KILLPRIV`,
  `FUSE_POSIX_ACL`, `FUSE_ABORT_ERROR`, `FUSE_MAX_PAGES`,
  `FUSE_CACHE_SYMLINKS`, `FUSE_NO_OPENDIR_SUPPORT`, `FUSE_EXPLICIT_INVAL_DATA`,
  `FUSE_HANDLE_KILLPRIV_V2`, `FUSE_SETXATTR_EXT`,
  `FUSE_INIT_EXT`/`FUSE_SECURITY_CTX`, `FUSE_CREATE_SUPP_GROUP`,
  `FUSE_HAS_EXPIRE_ONLY`, `FUSE_DIRECT_IO_ALLOW_MMAP`, `FUSE_NO_EXPORT_SUPPORT`
  (kernel 6.6+), `FUSE_WRITEBACK_CACHE`, `FUSE_NO_OPEN_SUPPORT` (6.7+),
  `FUSE_PARALLEL_DIROPS` (6.7+), `FUSE_HAS_INODE_DAX`, `FUSE_ASYNC_DIO`,
  `FUSE_CACHE_IO_MODE` (6.9+), `FUSE_PASSTHROUGH` (6.9+), `FUSE_HAS_RESEND`
  (6.10+), `FUSE_URING` (experimental), `FUSE_SUBMOUNTS`, `FUSE_ALLOW_IDMAP`
  (6.10+), `FUSE_WRITE_CACHE_STABLE` (6.10+), `FUSE_NO_READDIRPLUS_AUTO`,
  `FUSE_READDIR_CACHE` (6.12+), `FUSE_SET_UUID` (6.12+), `FUSE_DIRECT_IO_RELAX`
  (6.13+), `FUSE_CACHE_IO_MODE`/`FUSE_MAX_PAGES` growth, `FUSE_INIT_SECURITY_CTX`,
  `FUSE_CREATE_SUPP_GROUP` etc. — with `FUSE_INIT_EXT` carrying the
  64-bit feature word.

Practically, EntropyFS uses: cached reads, write-through writes (writeback
off by default, ADR-0014), readdirplus, `SEEK_DATA`/`SEEK_HOLE` via
lseek ops, `copy_file_range`, `fallocate`, `FUSE_HANDLE_KILLPRIV_V2`,
`FUSE_DONT_MASK` (permissions handled in userspace), parallel dirops,
max pages (large read sizes), and stable attribute TTLs.

## 3. FUSE caching semantics relevant to EntropyFS

- **Cached reads**: kernel page cache; `read` requests are whole pages.
  Good for a filesystem whose materialization is CPU-bound: the kernel
  absorbs repeat reads.
- **Write-through (default, no `FUSE_WRITEBACK_CACHE`)**: each `write`
  syscall becomes a FUSE write request; the page cache is updated after the
  daemon acks. Simplest correct model; our Phase-1 choice.
- **Writeback cache (`FUSE_WRITEBACK_CACHE`)**: kernel aggregates dirty
  pages and issues larger, page-aligned write requests; requires
  `setattr`-size handling for partial-page tails and careful `fsync`
  semantics. Off by default; gated behind the crash/partial-write test
  suite (ADR-0014).
- **mmap**: with cached I/O, mmap read/write faults go through the page
  cache and generate `read`/`write` FUSE requests (writes are
  write-through when writeback is off). Direct-I/O globally is rejected as
  a Phase-1 default because it disables page caching and complicates mmap.
- **`FUSE_DIRECT_IO_ALLOW_MMAP` / `FUSE_ASYNC_DIO`**: available; deferred
  to Phase 6.
- **Attribute TTLs**: conservative by default (e.g., 1 s attr, 1 s entry);
  a background optimizer must invalidate (`inval_inode`) when it rewrites
  an inode's representation, or rely on short TTLs.

## 4. FUSE passthrough (kernel 6.9+, present here)

`CONFIG_FUSE_PASSTHROUGH=y` in this kernel. Passthrough lets a FUSE daemon
back certain files with an underlying file's pages directly. For EntropyFS
it is a *possible Phase 6 optimization* for RAW extents on top of a
backing file — never a correctness dependency, and only meaningful when the
materialized bytes exist verbatim (which is precisely the case where
EntropyFS's representation is not saving anything). ADR-0014 records this.

## 5. FUSE-over-io_uring (experimental)

This kernel builds `CONFIG_FUSE_IO_URING=y` behind the module parameter
`enable_uring`. This is the experimental FUSE-over-io_uring path. Per the
spec and ADR-0002, EntropyFS does **not** build on it: not a foundation,
not required, revisit only after profiling and after the ABI stabilizes.

## 6. Crash / unmount behavior

- Daemon death (SIGKILL, panic, `kill -9`): kernel sends `FUSE_DESTROY` on
  clean unmount only; on abnormal exit the mount stays "connected" until
  the fd closes, then all requests fail with `ENOTCONN` and the mount is
  aborted by the kernel (`FUSE_ABORT_ERROR`). Unmounting after daemon death
  is `fusermount3 -u` (or lazy `-uz`).
- Our crash courts therefore kill the daemon process (not the machine) for
  daemon-crash testing, and VM/power-cut scripts for host-crash testing.
  `tools/vm-court.sh` documents the latter; host-crash semantics are
  exercised in VMs only, never on the development host (ADR-0016).
- Durability: EntropyFS's crash consistency lives entirely in the store
  protocol (ADR-0008) and is independent of FUSE request timing. FUSE
  requests are synchronous w.r.t. the daemon's commit protocol.

## 7. CachyOS packaging

- `fuse3` provides `fusermount3` (verified 3.18.2) and `/dev/fuse`;
  required runtime dependency. `fuse2` not needed.
- CachyOS ships the CachyOS kernel (7.2.0-1-cachyos here) with the FUSE
  config above; no custom kernel or module is required.
- Distribution: `PKGBUILD` in `packaging/` builds the single crate with
  `--release`, installs the `entropyfs` binary, completions, and a systemd
  user service example. `entropyfs mkfs` + `entropyfs mount` work without
  root for single-user use (no `allow_other` by default).

## 8. Kernel/ABI verification commands

Recorded for evidence reproduction (see `docs/performance/methodology.md`):

```sh
uname -r
fusermount3 --version
grep -i fuse /proc/filesystems
zgrep -E 'CONFIG_FUSE' /proc/config.gz
cat /sys/module/fuse/parameters/enable_uring
```
