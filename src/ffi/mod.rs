//! Phase 12E.14: the stable C ABI — the opaque-handle engine facade.
//!
//! # PURPOSE
//!
//! The adoption gate (12E.14): expose the embeddable immutable-object
//! engine (12E.1, stabilized through the 12E.13 adoption court) to C —
//! and through C to every other language — with a deliberately narrow
//! opaque-handle API. The facade is the ONLY thing crossing the
//! boundary: no Rust layout, no internal structs, no store types.
//!
//! # BOUNDARY
//!
//! KNOWS: `Engine`, `BlobId`, `ErrorCode`, `EngineError`, and the
//! `EngineMetrics` DTO (as JSON). NEVER KNOWS: the store, the
//! representation machinery, FUSE, the persistent format, or any
//! policy. The ABI version ([`entropyfs_abi_version`]) is INDEPENDENT
//! of the on-disk format version — they are separate compatibility
//! domains (documented in `include/entropyfs.h` and
//! `docs/api/engine.md`).
//!
//! # MODEL — the ownership rules (normative)
//!
//! 1. **Handles.** `entropyfs_engine_open` returns an opaque handle (a
//!    `*mut Engine`) into the caller's out-param. The caller OWNS it;
//!    `entropyfs_engine_close` CONSUMES it (the Rust `Box` is dropped).
//!    Using a handle after close is undefined behavior — the caller's
//!    obligation. The handle is safe to SHARE across threads for
//!    concurrent operations (the Engine's documented concurrency
//!    contract: many concurrent readers + writers; close drains
//!    in-flight ops).
//! 2. **Caller-owned inputs.** `data`/`len` (put) and `id` (32 bytes)
//!    are borrowed for the call's duration; the callee never retains
//!    them.
//! 3. **Callee-allocated outputs.** `entropyfs_blob_get` /
//!    `entropyfs_blob_read_range` / `entropyfs_metrics_json` allocate
//!    the output as a single allocation `[len u64]data…` (the 16-byte
//!    header is PART of the allocation; alignment 1, read via
//!    `read_unaligned`). The caller OWNS the returned pointer and MUST
//!    release it with [`entropyfs_free`] — the ONE release mechanism.
//!    Nothing else may free it; it must not be freed twice.
//! 4. **Errors.** Every function returns the stable numeric error class
//!    ([`ErrorCode`] as i32; `0` = ok). The thread-local human-readable
//!    detail is retrieved with [`entropyfs_last_error`] (never parsed by
//!    programs; the class is the contract). A panic is caught at every
//!    boundary and surfaced as `Internal` — it never unwinds across FFI,
//!    but it is still a defect (see the unsafe-ledger panic-boundary
//!    note in `docs/security/unsafe-ledger.md`).
//!
//! # CONCURRENCY
//!
//! The handle is `Send + Sync` (the Engine is an `Arc`-sharing,
//! internally-synchronized store); concurrent calls from multiple
//! threads are the normal mode. `entropyfs_last_error` is thread-local.
//! `entropyfs_free` is global (a plain deallocation; no shared state).
//!
//! # SAFETY — the ledger preconditions (docs/security/unsafe-ledger.md)
//!
//! This module is the crate's SECOND designated unsafe file
//! (`src/ffi/mod.rs`), with `#![allow(unsafe_code)]` at the module
//! level. Every `unsafe` block sits behind an `extern "C"` boundary
//! whose precondition is checked first:
//!
//! - **Handle validity.** A handle is only ever produced by
//!   `entropyfs_engine_open` (non-null, exactly once). `Box::from_raw`
//!   in close is the ownership transfer; the ledger requires the caller
//!   to close each handle exactly once.
//! - **Input pointer validity.** `data` must be valid for `len` bytes
//!   (or null with `len == 0`); `id` for 32 bytes; out-params non-null.
//!   `CStr::from_ptr` is only called after a null check.
//! - **Output allocation.** The header is written before any use; the
//!   returned pointer is a `Vec<u8>` allocation (alignment 1), and
//!   `entropyfs_free` reconstructs it with `Vec::from_raw_parts` from
//!   the header it reads back. Double-free / wrong-free are the
//!   caller's obligation per the header contract.
//! - **Panic containment.** Every entry point wraps its body in
//!   `catch_unwind` so no panic unwinds across FFI.
//!
//! The C smoke test (`tools/ffi-smoke.sh` + `tools/ffi-smoke/smoke.c`)
//! exercises open/create, put/get/range byte-exactness, contains, sync,
//! compact, metrics JSON, the error path (missing blob -> NotFound),
//! last_error, the free contract, and open/close lifecycle cycles.
//!
//! # HISTORY / EVIDENCE
//!
//! 12E.1 (v0.7.12) stabilized the Rust facade; 12E.13 (v0.7.12) gave it
//! the adoption-court workload; this module is 12E.14. The ABI is
//! versioned from day one so the Rust surface can evolve under it.

#![allow(unsafe_code)] // ledger-designated: docs/security/unsafe-ledger.md

use std::ffi::{CStr, c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use crate::engine::{BlobId, Engine, EngineError, EngineOpenOptions, ErrorCode};

/// The C ABI version (independent of the on-disk format version; see
/// `include/entropyfs.h`). Bump only on a breaking ABI change, and keep
/// `entropyfs_abi_version()` in lockstep.
pub const EFS_ABI_VERSION: u32 = 1;

/// Open mode for [`entropyfs_engine_open`]: open an existing store.
pub const EFS_ENGINE_OPEN: c_int = 0;
/// Open mode: create a fresh store (errors if the path is already a
/// store).
pub const EFS_ENGINE_CREATE: c_int = 1;
/// Open mode: open an existing store READ-ONLY (unknown `ro_compat`
/// bits permitted; every write fails with `EFS_UNSUPPORTED`).
pub const EFS_ENGINE_OPEN_RO: c_int = 2;

/// Stable error classes (the C-ABI contract; the header's
/// `enum entropyfs_error`). Programs switch on these; never parse the
/// last-error string.
///
/// - [`EFS_OK`] = success
/// - [`EFS_NOT_FOUND`] = blob/store does not exist
/// - [`EFS_INVALID_ARGUMENT`] = bad caller argument
/// - [`EFS_CORRUPT_STORE`] = persistent corruption
/// - [`EFS_INCOMPATIBLE_FORMAT`] = format cannot be opened in the mode
/// - [`EFS_RESOURCE_LIMIT`] = a resource bound was exceeded
/// - [`EFS_IO`] = underlying I/O failure
/// - [`EFS_BUSY`] = conflicting exclusive operation
/// - [`EFS_UNSUPPORTED`] = not supported in this configuration
/// - [`EFS_INTERNAL`] = internal invariant failure (a bug)
/// - [`EFS_CLOSED`] = the handle is closed
pub const EFS_OK: c_int = ErrorCode::Ok.as_i32();
/// See the group doc on [`EFS_OK`].
pub const EFS_NOT_FOUND: c_int = ErrorCode::NotFound.as_i32();
/// See the group doc on [`EFS_OK`].
pub const EFS_INVALID_ARGUMENT: c_int = ErrorCode::InvalidArgument.as_i32();
/// See the group doc on [`EFS_OK`].
pub const EFS_CORRUPT_STORE: c_int = ErrorCode::CorruptStore.as_i32();
/// See the group doc on [`EFS_OK`].
pub const EFS_INCOMPATIBLE_FORMAT: c_int = ErrorCode::IncompatibleFormat.as_i32();
/// See the group doc on [`EFS_OK`].
pub const EFS_RESOURCE_LIMIT: c_int = ErrorCode::ResourceLimit.as_i32();
/// See the group doc on [`EFS_OK`].
pub const EFS_IO: c_int = ErrorCode::Io.as_i32();
/// See the group doc on [`EFS_OK`].
pub const EFS_BUSY: c_int = ErrorCode::Busy.as_i32();
/// See the group doc on [`EFS_OK`].
pub const EFS_UNSUPPORTED: c_int = ErrorCode::Unsupported.as_i32();
/// See the group doc on [`EFS_OK`].
pub const EFS_INTERNAL: c_int = ErrorCode::Internal.as_i32();
/// See the group doc on [`EFS_OK`].
pub const EFS_CLOSED: c_int = ErrorCode::Closed.as_i32();

/// Copy `len` bytes from a callee-allocated ABI output into an owned
/// `Vec`, then release the allocation (the [`entropyfs_free`] contract).
/// Safe wrapper for the Rust-side court (`src/tests/ffi_cabi.rs`); C
/// callers use the raw pointer directly and free it themselves. The
/// unsafe dereference stays inside the ledger-designated module.
pub fn take_output(ptr: *mut u8, len: usize) -> Vec<u8> {
    if len == 0 || ptr.is_null() {
        return Vec::new();
    }
    let out = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
    entropyfs_free(ptr);
    out
}

// ---------------------------------------------------------------------------
// Thread-local last-error detail. The CLASS is the return value; this is
// only the human-readable detail (documented as never-parseable).
// ---------------------------------------------------------------------------
thread_local! {
    static LAST_ERROR: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

fn set_last_error(msg: impl Into<String>) {
    LAST_ERROR.with(|m| *m.borrow_mut() = msg.into());
}

/// Fetch the calling thread's last error detail into `buf` (truncated to
/// `cap` bytes, always NUL-terminated when `cap > 0`). Returns `0` when
/// the last call on this thread succeeded, nonzero when it failed. This
/// is diagnostic detail only — programs must switch on the return codes,
/// never parse this string.
#[unsafe(no_mangle)]
pub extern "C" fn entropyfs_last_error(buf: *mut c_char, cap: usize) -> c_int {
    LAST_ERROR.with(|m| {
        let s = m.borrow();
        let failed = if s.is_empty() { 0 } else { 1 };
        if !buf.is_null() && cap > 0 {
            let bytes = s.as_bytes();
            let n = bytes.len().min(cap - 1);
            unsafe {
                ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, n);
                *buf.add(n) = 0;
            }
        }
        failed
    })
}

/// Release a pointer previously returned by [`entropyfs_blob_get`],
/// [`entropyfs_blob_read_range`], or [`entropyfs_metrics_json`] — the
/// ONE release mechanism for callee-allocated outputs. The pointer must
/// have been allocated by this ABI and must be freed exactly once.
#[unsafe(no_mangle)]
pub extern "C" fn entropyfs_free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    // The allocation is `[len u64]data…` (len+16 bytes total; the
    // caller's pointer is base+16). Reconstruct the exact Vec from the
    // header and drop it (alignment 1, read unaligned).
    unsafe {
        let base = ptr.sub(16);
        let len = ptr::read_unaligned(base as *const u64) as usize;
        let cap = len + 16;
        drop(Vec::from_raw_parts(base, cap, cap));
    }
}

/// Allocate a self-describing buffer holding `len` bytes, returning the
/// data pointer (`base+16`). The 16-byte header `[len u64]` is PART of
/// the allocation (never `ptr.sub` on a bare Vec pointer — that would be
/// UB); the ownership moves to the caller (the [`entropyfs_free`]
/// contract).
unsafe fn alloc_output(len: usize) -> *mut u8 {
    let cap = len + 16;
    let mut v: Vec<u8> = Vec::with_capacity(cap);
    v.resize(cap, 0);
    let base = v.as_mut_ptr();
    // edition-2024: unsafe ops inside an `unsafe fn` still need an
    // explicit block.
    unsafe {
        ptr::write_unaligned(base as *mut u64, len as u64);
        std::mem::forget(v); // ownership transfers to the caller
        base.add(16)
    }
}

/// The current C ABI version. Independent of the on-disk format
/// version (a separate compatibility domain).
#[unsafe(no_mangle)]
pub extern "C" fn entropyfs_abi_version() -> u32 {
    EFS_ABI_VERSION
}

/// Open (mode `EFS_ENGINE_OPEN`) or create (mode `EFS_ENGINE_CREATE`)
/// an engine at `path`. On success returns `Ok` and writes the opaque
/// handle into `*out_handle` (the caller owns it; close exactly once).
fn open_impl(
    path: *const c_char,
    mode: c_int,
    out_handle: *mut *mut Engine,
) -> Result<(), EngineError> {
    if path.is_null() || out_handle.is_null() {
        return Err(EngineError::new(
            ErrorCode::InvalidArgument,
            "null path or out_handle",
        ));
    }
    let path = unsafe { CStr::from_ptr(path) }
        .to_str()
        .map_err(|_| EngineError::new(ErrorCode::InvalidArgument, "path is not valid UTF-8"))?
        .to_owned();
    let opts = EngineOpenOptions::default();
    let engine = match mode {
        EFS_ENGINE_OPEN => Engine::open(std::path::Path::new(&path), &opts)?,
        EFS_ENGINE_CREATE => Engine::create(std::path::Path::new(&path), &opts)?,
        EFS_ENGINE_OPEN_RO => {
            let ro = EngineOpenOptions {
                read_only: true,
                ..opts
            };
            Engine::open(std::path::Path::new(&path), &ro)?
        }
        other => {
            return Err(EngineError::new(
                ErrorCode::InvalidArgument,
                format!("unknown engine open mode {other} (0=open, 1=create, 2=open read-only)"),
            ));
        }
    };
    unsafe {
        ptr::write(out_handle, Box::into_raw(Box::new(engine)));
    }
    Ok(())
}

/// Open or create an engine. Returns the error class; `0` on success.
#[unsafe(no_mangle)]
pub extern "C" fn entropyfs_engine_open(
    path: *const c_char,
    mode: c_int,
    out_handle: *mut *mut Engine,
) -> c_int {
    let r = catch_unwind(AssertUnwindSafe(|| open_impl(path, mode, out_handle)));
    match r {
        Ok(Ok(())) => {
            set_last_error("");
            ErrorCode::Ok.as_i32()
        }
        Ok(Err(e)) => {
            set_last_error(format!("entropyfs_engine_open: {}", e.message));
            e.code.as_i32()
        }
        Err(_) => {
            set_last_error(
                "entropyfs_engine_open panicked at the FFI boundary (a bug — see unsafe-ledger panic-boundary note)",
            );
            eprintln!("entropyfs_engine_open: panic caught at the FFI boundary");
            ErrorCode::Internal.as_i32()
        }
    }
}

/// Close an engine, CONSUMING the handle. Using the handle after this
/// call is undefined behavior (close exactly once).
#[unsafe(no_mangle)]
pub extern "C" fn entropyfs_engine_close(handle: *mut Engine) -> c_int {
    if handle.is_null() {
        set_last_error("entropyfs_engine_close: null handle");
        return ErrorCode::InvalidArgument.as_i32();
    }
    let r = catch_unwind(AssertUnwindSafe(|| {
        // The Box ownership transfer: the caller gives the handle back
        // exactly once (ledger precondition).
        let engine = unsafe { Box::from_raw(handle) };
        engine.close()
    }));
    match r {
        Ok(Ok(())) => {
            set_last_error("");
            ErrorCode::Ok.as_i32()
        }
        Ok(Err(e)) => {
            set_last_error(format!("entropyfs_engine_close: {}", e.message));
            e.code.as_i32()
        }
        Err(_) => {
            set_last_error("entropyfs_engine_close panicked at the FFI boundary (a bug)");
            eprintln!("entropyfs_engine_close: panic caught at the FFI boundary");
            ErrorCode::Internal.as_i32()
        }
    }
}

/// Put a blob (Ack durability — process-crash-safe; power-durable after
/// [`entropyfs_sync`]). `id` receives the 32-byte content id (BLAKE3).
/// Equal bytes always produce equal ids.
fn put_impl(
    handle: *mut Engine,
    data: *const u8,
    len: usize,
    id: *mut u8,
) -> Result<BlobId, EngineError> {
    if handle.is_null() || id.is_null() {
        return Err(EngineError::new(
            ErrorCode::InvalidArgument,
            "null handle or id",
        ));
    }
    if len > 0 && data.is_null() {
        return Err(EngineError::new(
            ErrorCode::InvalidArgument,
            "null data with nonzero len",
        ));
    }
    let bytes: &[u8] = if len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(data, len) }
    };
    let engine = unsafe { &*handle };
    let bid = engine.put_blob(bytes)?;
    unsafe {
        ptr::copy_nonoverlapping(bid.as_bytes().as_ptr(), id, 32);
    }
    Ok(bid)
}

/// Put a blob; returns the error class (`0` = ok) and writes the
/// 32-byte content id into `id`.
#[unsafe(no_mangle)]
pub extern "C" fn entropyfs_blob_put(
    handle: *mut Engine,
    data: *const u8,
    len: usize,
    id: *mut u8,
) -> c_int {
    let r = catch_unwind(AssertUnwindSafe(|| put_impl(handle, data, len, id)));
    match r {
        Ok(Ok(_)) => {
            set_last_error("");
            ErrorCode::Ok.as_i32()
        }
        Ok(Err(e)) => {
            set_last_error(format!("entropyfs_blob_put: {}", e.message));
            e.code.as_i32()
        }
        Err(_) => {
            set_last_error("entropyfs_blob_put panicked at the FFI boundary (a bug)");
            eprintln!("entropyfs_blob_put: panic caught at the FFI boundary");
            ErrorCode::Internal.as_i32()
        }
    }
}

/// Fetch a blob's complete bytes (the engine's hash gate: the returned
/// bytes must hash to the id). The callee allocates `*out_buf`; the
/// caller owns it and MUST release it with [`entropyfs_free`].
fn get_impl(
    handle: *mut Engine,
    id: *const u8,
    out_buf: *mut *mut u8,
    out_len: *mut usize,
) -> Result<(), EngineError> {
    if handle.is_null() || id.is_null() || out_buf.is_null() || out_len.is_null() {
        return Err(EngineError::new(ErrorCode::InvalidArgument, "null arg"));
    }
    let bid = BlobId::new(unsafe { ptr::read(id as *const [u8; 32]) });
    let engine = unsafe { &*handle };
    let bytes = engine.get_blob(bid)?;
    let len = bytes.len();
    let ptr = unsafe { alloc_output(len) };
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, len);
        ptr::write(out_buf, ptr);
        ptr::write(out_len, len);
    }
    Ok(())
}

/// Fetch a blob; returns the error class and a callee-allocated buffer
/// (free with [`entropyfs_free`]).
#[unsafe(no_mangle)]
pub extern "C" fn entropyfs_blob_get(
    handle: *mut Engine,
    id: *const u8,
    out_buf: *mut *mut u8,
    out_len: *mut usize,
) -> c_int {
    let r = catch_unwind(AssertUnwindSafe(|| get_impl(handle, id, out_buf, out_len)));
    match r {
        Ok(Ok(())) => {
            set_last_error("");
            ErrorCode::Ok.as_i32()
        }
        Ok(Err(e)) => {
            set_last_error(format!("entropyfs_blob_get: {}", e.message));
            e.code.as_i32()
        }
        Err(_) => {
            set_last_error("entropyfs_blob_get panicked at the FFI boundary (a bug)");
            eprintln!("entropyfs_blob_get: panic caught at the FFI boundary");
            ErrorCode::Internal.as_i32()
        }
    }
}

/// Read a byte range of a blob (EOF-clipped like `pread`; `len ==
/// usize::MAX` reads to the end). Callee-allocated output; free with
/// [`entropyfs_free`].
fn range_impl(
    handle: *mut Engine,
    id: *const u8,
    offset: u64,
    len: usize,
    out_buf: *mut *mut u8,
    out_len: *mut usize,
) -> Result<(), EngineError> {
    if handle.is_null() || id.is_null() || out_buf.is_null() || out_len.is_null() {
        return Err(EngineError::new(ErrorCode::InvalidArgument, "null arg"));
    }
    let bid = BlobId::new(unsafe { ptr::read(id as *const [u8; 32]) });
    let engine = unsafe { &*handle };
    let bytes = engine.read_blob_range(bid, offset, len)?;
    let n = bytes.len();
    let ptr = unsafe { alloc_output(n) };
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, n);
        ptr::write(out_buf, ptr);
        ptr::write(out_len, n);
    }
    Ok(())
}

/// Read a byte range; returns the error class and a callee-allocated
/// buffer (free with [`entropyfs_free`]).
#[unsafe(no_mangle)]
pub extern "C" fn entropyfs_blob_read_range(
    handle: *mut Engine,
    id: *const u8,
    offset: u64,
    len: usize,
    out_buf: *mut *mut u8,
    out_len: *mut usize,
) -> c_int {
    let r = catch_unwind(AssertUnwindSafe(|| {
        range_impl(handle, id, offset, len, out_buf, out_len)
    }));
    match r {
        Ok(Ok(())) => {
            set_last_error("");
            ErrorCode::Ok.as_i32()
        }
        Ok(Err(e)) => {
            set_last_error(format!("entropyfs_blob_read_range: {}", e.message));
            e.code.as_i32()
        }
        Err(_) => {
            set_last_error("entropyfs_blob_read_range panicked at the FFI boundary (a bug)");
            eprintln!("entropyfs_blob_read_range: panic caught at the FFI boundary");
            ErrorCode::Internal.as_i32()
        }
    }
}

/// Whether a blob id exists (was put and acknowledged).
#[unsafe(no_mangle)]
pub extern "C" fn entropyfs_contains(handle: *mut Engine, id: *const u8, out: *mut c_int) -> c_int {
    let r = catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() || id.is_null() || out.is_null() {
            return Err(EngineError::new(ErrorCode::InvalidArgument, "null arg"));
        }
        let bid = BlobId::new(unsafe { ptr::read(id as *const [u8; 32]) });
        let engine = unsafe { &*handle };
        let c = engine.contains(bid)?;
        unsafe { ptr::write(out, if c { 1 } else { 0 }) };
        Ok(())
    }));
    match r {
        Ok(Ok(())) => {
            set_last_error("");
            ErrorCode::Ok.as_i32()
        }
        Ok(Err(e)) => {
            set_last_error(format!("entropyfs_contains: {}", e.message));
            e.code.as_i32()
        }
        Err(_) => {
            set_last_error("entropyfs_contains panicked at the FFI boundary (a bug)");
            eprintln!("entropyfs_contains: panic caught at the FFI boundary");
            ErrorCode::Internal.as_i32()
        }
    }
}

/// Make all acknowledged puts power-durable (the durability boundary;
/// the engine's group-commit machinery — the 12B generations).
#[unsafe(no_mangle)]
pub extern "C" fn entropyfs_sync(handle: *mut Engine) -> c_int {
    let r = catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() {
            return Err(EngineError::new(ErrorCode::InvalidArgument, "null handle"));
        }
        let engine = unsafe { &*handle };
        engine.sync()
    }));
    match r {
        Ok(Ok(())) => {
            set_last_error("");
            ErrorCode::Ok.as_i32()
        }
        Ok(Err(e)) => {
            set_last_error(format!("entropyfs_sync: {}", e.message));
            e.code.as_i32()
        }
        Err(_) => {
            set_last_error("entropyfs_sync panicked at the FFI boundary (a bug)");
            eprintln!("entropyfs_sync: panic caught at the FFI boundary");
            ErrorCode::Internal.as_i32()
        }
    }
}

/// Compact (reclaim unreachable bytes). Writes the reclaimed bytes and
/// the post-pass physical used bytes into the out-params (nullable).
#[unsafe(no_mangle)]
pub extern "C" fn entropyfs_compact(
    handle: *mut Engine,
    out_reclaimed: *mut u64,
    out_physical: *mut u64,
) -> c_int {
    let r = catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() {
            return Err(EngineError::new(ErrorCode::InvalidArgument, "null handle"));
        }
        let engine = unsafe { &*handle };
        let rep = engine.compact()?;
        if !out_reclaimed.is_null() {
            unsafe { ptr::write(out_reclaimed, rep.reclaimed_bytes) };
        }
        if !out_physical.is_null() {
            unsafe { ptr::write(out_physical, rep.physical_used_after_bytes) };
        }
        Ok(())
    }));
    match r {
        Ok(Ok(())) => {
            set_last_error("");
            ErrorCode::Ok.as_i32()
        }
        Ok(Err(e)) => {
            set_last_error(format!("entropyfs_compact: {}", e.message));
            e.code.as_i32()
        }
        Err(_) => {
            set_last_error("entropyfs_compact panicked at the FFI boundary (a bug)");
            eprintln!("entropyfs_compact: panic caught at the FFI boundary");
            ErrorCode::Internal.as_i32()
        }
    }
}

/// Fetch the engine metrics as a JSON string (callee-allocated; free
/// with [`entropyfs_free`]). The JSON is the versioned `EngineMetrics`
/// DTO (12E.6) — the same schema `entropyfs metrics --json` emits.
#[unsafe(no_mangle)]
pub extern "C" fn entropyfs_metrics_json(
    handle: *mut Engine,
    out_buf: *mut *mut u8,
    out_len: *mut usize,
) -> c_int {
    let r = catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() || out_buf.is_null() || out_len.is_null() {
            return Err(EngineError::new(ErrorCode::InvalidArgument, "null arg"));
        }
        let engine = unsafe { &*handle };
        let json = engine.metrics()?.to_json();
        let bytes = json.into_bytes();
        let n = bytes.len();
        let ptr = unsafe { alloc_output(n) };
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, n);
            ptr::write(out_buf, ptr);
            ptr::write(out_len, n);
        }
        Ok(())
    }));
    match r {
        Ok(Ok(())) => {
            set_last_error("");
            ErrorCode::Ok.as_i32()
        }
        Ok(Err(e)) => {
            set_last_error(format!("entropyfs_metrics_json: {}", e.message));
            e.code.as_i32()
        }
        Err(_) => {
            set_last_error("entropyfs_metrics_json panicked at the FFI boundary (a bug)");
            eprintln!("entropyfs_metrics_json: panic caught at the FFI boundary");
            ErrorCode::Internal.as_i32()
        }
    }
}

// Keep the C void type referenced (documentation of the free contract).
#[allow(unused_imports)]
use c_void as _Cvoid;
