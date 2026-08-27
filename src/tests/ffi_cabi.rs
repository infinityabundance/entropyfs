//! Phase 12E.14: the C ABI court (Rust side of the boundary).
//!
//! # PURPOSE
//!
//! Exercises the `extern "C"` facade (`src/ffi`) exactly as a C or Go
//! caller would: opaque handles, caller-owned inputs, callee-allocated
//! outputs freed by [`crate::ffi::entropyfs_free`], the stable error
//! classes, the thread-local last-error detail, and the open/close
//! lifecycle. The C-side proof is the smoke test (`tools/ffi-smoke.sh` +
//! `tools/ffi-smoke/smoke.c`); this court is the ledger-required Rust
//! test for the designated unsafe file.
//!
//! # BOUNDARY
//!
//! KNOWS: the ABI surface only. NEVER KNOWS: the store or the engine
//! internals — everything here goes through the raw `extern "C"` entry
//! points, exactly like the adoption customer.
//!
//! # INVARIANTS UNDER TEST
//!
//! - open/create + close lifecycle (create -> close -> open -> close);
//! - put/get/range byte-exactness through the ABI;
//! - dedup identity: equal bytes -> equal 32-byte ids;
//! - contains true/false;
//! - sync + compact (reclaimed reported);
//! - metrics JSON (the versioned DTO);
//! - the error contract: missing blob -> `EFS_NOT_FOUND`, null args ->
//!   `EFS_INVALID_ARGUMENT`, and last_error carrying detail;
//! - the free contract (every callee-allocated buffer released exactly
//!   once via `entropyfs_free`).

#![forbid(unsafe_code)]

use std::ffi::CString;
use std::ptr;

use tempfile::TempDir;

use crate::engine::Engine;
use crate::ffi::{
    EFS_ABI_VERSION, EFS_ENGINE_CREATE, EFS_ENGINE_OPEN, EFS_INVALID_ARGUMENT, EFS_NOT_FOUND,
    EFS_OK,
};

type Handle = *mut Engine;

fn open_create(dir: &std::path::Path) -> Handle {
    let c = CString::new(dir.to_str().expect("utf8 path")).expect("cstring");
    let mut h: Handle = ptr::null_mut();
    let rc = crate::ffi::entropyfs_engine_open(c.as_ptr(), EFS_ENGINE_CREATE, &mut h);
    assert_eq!(rc, EFS_OK, "create failed: {}", last_error());
    assert!(!h.is_null());
    h
}

fn open_existing(dir: &std::path::Path) -> Handle {
    let c = CString::new(dir.to_str().expect("utf8 path")).expect("cstring");
    let mut h: Handle = ptr::null_mut();
    let rc = crate::ffi::entropyfs_engine_open(c.as_ptr(), EFS_ENGINE_OPEN, &mut h);
    assert_eq!(rc, EFS_OK, "open failed: {}", last_error());
    assert!(!h.is_null());
    h
}

fn close(h: Handle) {
    let rc = crate::ffi::entropyfs_engine_close(h);
    assert_eq!(rc, EFS_OK, "close failed: {}", last_error());
}

fn last_error() -> String {
    let mut buf = [0u8; 512];
    crate::ffi::entropyfs_last_error(buf.as_mut_ptr() as *mut std::ffi::c_char, buf.len());
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

fn put(h: Handle, data: &[u8]) -> [u8; 32] {
    let mut id = [0u8; 32];
    let rc = crate::ffi::entropyfs_blob_put(
        h,
        if data.is_empty() {
            ptr::null()
        } else {
            data.as_ptr()
        },
        data.len(),
        id.as_mut_ptr(),
    );
    assert_eq!(rc, EFS_OK, "put failed: {}", last_error());
    id
}

fn get(h: Handle, id: &[u8; 32]) -> Vec<u8> {
    let mut buf: *mut u8 = ptr::null_mut();
    let mut len = 0usize;
    let rc = crate::ffi::entropyfs_blob_get(h, id.as_ptr(), &mut buf, &mut len);
    assert_eq!(rc, EFS_OK, "get failed: {}", last_error());
    crate::ffi::take_output(buf, len)
}

#[test]
fn abi_version_is_current() {
    assert_eq!(crate::ffi::entropyfs_abi_version(), EFS_ABI_VERSION);
}

#[test]
fn create_put_get_range_sync_compact_metrics() {
    let tmp = TempDir::new().expect("tmp");
    let dir = tmp.path().join("store");
    let h = open_create(&dir);

    // Three distinct blobs + one duplicate (dedup identity).
    let a = b"entropyfs c-abi blob one: the quick brown fox jumps over the lazy dog".to_vec();
    let b = vec![0xABu8; 4096];
    let c = b"{\"schema\":\"config\",\"value\":42,\"nested\":{\"a\":[1,2,3]}}".to_vec();
    let id_a = put(h, &a);
    let id_b = put(h, &b);
    let id_c = put(h, &c);
    let id_a2 = put(h, &a);
    assert_eq!(id_a, id_a2, "equal bytes must dedup to the same id");
    assert_ne!(id_a, id_b);
    assert_ne!(id_b, id_c);

    // Byte-exact get through the ABI.
    assert_eq!(get(h, &id_a), a);
    assert_eq!(get(h, &id_b), b);
    assert_eq!(get(h, &id_c), c);

    // Range read: a 64-byte window at offset 100 of blob b.
    let mut buf: *mut u8 = ptr::null_mut();
    let mut len = 0usize;
    let rc = crate::ffi::entropyfs_blob_read_range(h, id_b.as_ptr(), 100, 64, &mut buf, &mut len);
    assert_eq!(rc, EFS_OK, "range failed: {}", last_error());
    assert_eq!(len, 64);
    let want = &b[100..164];
    let got = crate::ffi::take_output(buf, len);
    assert_eq!(got, want, "range bytes must match");

    // contains: true for stored, false for an arbitrary id.
    let mut present = 0;
    let rc = crate::ffi::entropyfs_contains(h, id_a.as_ptr(), &mut present);
    assert_eq!(rc, EFS_OK);
    assert_eq!(present, 1);
    let mut absent = 1;
    let rc = crate::ffi::entropyfs_contains(h, [0xEE; 32].as_ptr(), &mut absent);
    assert_eq!(rc, EFS_OK);
    assert_eq!(absent, 0);

    // Durability + maintenance.
    let rc = crate::ffi::entropyfs_sync(h);
    assert_eq!(rc, EFS_OK, "sync failed: {}", last_error());
    let mut reclaimed = u64::MAX;
    let mut physical = u64::MAX;
    let rc = crate::ffi::entropyfs_compact(h, &mut reclaimed, &mut physical);
    assert_eq!(rc, EFS_OK, "compact failed: {}", last_error());
    assert!(physical > 0, "a store with 3 blobs has physical bytes");

    // Metrics JSON (the versioned DTO).
    let mut jbuf: *mut u8 = ptr::null_mut();
    let mut jlen = 0usize;
    let rc = crate::ffi::entropyfs_metrics_json(h, &mut jbuf, &mut jlen);
    assert_eq!(rc, EFS_OK, "metrics failed: {}", last_error());
    let json: serde_json::Value =
        serde_json::from_slice(&crate::ffi::take_output(jbuf, jlen)).expect("metrics JSON parses");
    assert_eq!(json["schema_version"], 1);
    assert!(json["accounting"]["physical_used_bytes"].as_u64().unwrap() > 0);

    close(h);
}

#[test]
fn error_paths_are_classified() {
    let tmp = TempDir::new().expect("tmp");
    let dir = tmp.path().join("store");
    let h = open_create(&dir);

    // Missing blob -> NOT_FOUND, with last_error detail.
    let mut buf: *mut u8 = ptr::null_mut();
    let mut len = 0usize;
    let rc = crate::ffi::entropyfs_blob_get(h, [0x11; 32].as_ptr(), &mut buf, &mut len);
    assert_eq!(rc, EFS_NOT_FOUND, "missing blob must be NOT_FOUND");
    assert!(!last_error().is_empty(), "last_error carries detail");
    assert!(buf.is_null(), "no buffer on error");

    // Null handle -> INVALID_ARGUMENT.
    let rc =
        crate::ffi::entropyfs_blob_get(ptr::null_mut(), [0u8; 32].as_ptr(), &mut buf, &mut len);
    assert_eq!(rc, EFS_INVALID_ARGUMENT);

    // Null data with nonzero len -> INVALID_ARGUMENT.
    let mut id = [0u8; 32];
    let rc = crate::ffi::entropyfs_blob_put(h, ptr::null(), 16, id.as_mut_ptr());
    assert_eq!(rc, EFS_INVALID_ARGUMENT);

    // Open a nonexistent store -> NOT OK (classified, not a panic).
    let ghost = CString::new(tmp.path().join("ghost").to_str().unwrap()).unwrap();
    let mut h2: Handle = ptr::null_mut();
    let rc = crate::ffi::entropyfs_engine_open(ghost.as_ptr(), EFS_ENGINE_OPEN, &mut h2);
    assert_ne!(rc, EFS_OK);
    assert!(h2.is_null());

    // Create over an existing store -> NOT OK.
    let c = CString::new(dir.to_str().unwrap()).unwrap();
    let mut h3: Handle = ptr::null_mut();
    let rc = crate::ffi::entropyfs_engine_open(c.as_ptr(), EFS_ENGINE_CREATE, &mut h3);
    assert_ne!(rc, EFS_OK, "creating over an existing store must fail");

    close(h);
}

#[test]
fn lifecycle_open_close_cycles() {
    let tmp = TempDir::new().expect("tmp");
    let dir = tmp.path().join("store");
    // create -> close -> open -> close -> open -> close (three cycles).
    for i in 0..3 {
        let mode = if i == 0 {
            EFS_ENGINE_CREATE
        } else {
            EFS_ENGINE_OPEN
        };
        let c = CString::new(dir.to_str().unwrap()).unwrap();
        let mut h: Handle = ptr::null_mut();
        let rc = crate::ffi::entropyfs_engine_open(c.as_ptr(), mode, &mut h);
        assert_eq!(rc, EFS_OK, "cycle {i} open: {}", last_error());
        assert!(!h.is_null());
        close(h);
    }
}

#[test]
fn dedup_across_handles() {
    let tmp = TempDir::new().expect("tmp");
    let dir = tmp.path().join("store");
    let h1 = open_create(&dir);
    let id = put(h1, b"shared immutable bytes across handles".as_slice());
    close(h1);
    let h2 = open_existing(&dir);
    let id2 = put(h2, b"shared immutable bytes across handles".as_slice());
    assert_eq!(id, id2, "content identity is stable across opens");
    assert_eq!(
        get(h2, &id),
        b"shared immutable bytes across handles".to_vec()
    );
    close(h2);
}
