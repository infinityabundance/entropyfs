//! `entropyfs capabilities`: print compiled-in capabilities and the local
//! Linux/FUSE environment (§47 diagnostics).

#![forbid(unsafe_code)]

use crate::core::limits::CHUNK_CLASSES;
use crate::platform::linux::{fuse_available, kernel_fuse_io_uring};

/// Run capabilities.
pub fn run() -> Result<(), String> {
    let avail = fuse_available();
    println!("entropyfs version: {}", env!("CARGO_PKG_VERSION"));
    println!(
        "format: v{}.{} (fuse frontend via fuser 0.18)",
        crate::format::version::FORMAT_MAJOR,
        crate::format::version::FORMAT_MINOR,
    );
    println!("chunk classes: {:?}", CHUNK_CLASSES);
    println!(
        "default chunk class: {}",
        crate::core::limits::DEFAULT_CHUNK_CLASS
    );
    println!(
        "max reference depth: {}",
        crate::core::limits::DEFAULT_MAX_REFERENCE_DEPTH
    );
    println!(
        "representation families: ZERO FILL INLINE RAW RANS EXACT_REF BASE_RESIDUAL SPARSE PALETTE PERIODIC ENTROPY_REF PERMUTATION SEQUENCE_RANS SPARSE_BLOCK64"
    );
    println!("rANS backends: ryg-rans-rs 0.5.1 (byte, interleaved2; scalar authority)");
    println!("DSFB observer: dsfb 0.1.2 (zero decoding authority)");
    println!(
        "storage transport (Phase 10F): SyncIo (reference path, default) | UringIo (io_uring)"
    );
    println!(
        "safety ledger: 1 confined file (platform/io_uring.rs; see docs/security/unsafe-ledger.md)"
    );
    println!();
    println!(
        "/dev/fuse: {}",
        if avail.dev_fuse { "present" } else { "MISSING" }
    );
    println!(
        "fusermount3: {}",
        if avail.fusermount3 {
            "present"
        } else {
            "MISSING"
        }
    );
    println!(
        "kernel fuse: {}",
        if avail.kernel_fuse {
            "registered"
        } else {
            "NOT registered"
        }
    );
    println!(
        "kernel fuse io_uring: {}",
        if kernel_fuse_io_uring() {
            "enabled"
        } else {
            "disabled (informational)"
        }
    );
    // Phase 10F: probe the io_uring transport (the UringIo backend).
    match crate::platform::io_uring::Uring::new(8) {
        Ok(_) => println!("io_uring transport: available"),
        Err(e) => println!("io_uring transport: UNAVAILABLE ({e})"),
    }
    println!("io_uring ops: READ/WRITE (kernel 5.6+), FSYNC (5.1+), UNLINKAT (5.11+)");
    if !avail.ready() {
        for d in avail.diagnose() {
            println!("  note: {d}");
        }
    }
    Ok(())
}
