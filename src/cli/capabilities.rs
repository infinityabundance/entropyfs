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
        "representation families: ZERO FILL INLINE RAW RANS EXACT_REF BASE_RESIDUAL SPARSE PALETTE PERIODIC ENTROPY_REF PERMUTATION SEQUENCE_RANS"
    );
    println!("rANS backends: ryg-rans-rs 0.5.1 (byte, interleaved2; scalar authority)");
    println!("DSFB observer: dsfb 0.1.2 (zero decoding authority)");
    println!("unsafe code: none (#![forbid(unsafe_code)])");
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
    if !avail.ready() {
        for d in avail.diagnose() {
            println!("  note: {d}");
        }
    }
    Ok(())
}
