//! Phase 12E.10: classified CLI failure messages.
//!
//! # PURPOSE
//!
//! The trial path must tell an operator what is MISSING, never print an
//! opaque `EIO`. This module classifies the store's typed errors and the
//! environment probes into actionable messages:
//!
//! - unknown on-disk INCOMPAT bit → what the store is, what to do
//! - unknown RO_COMPAT for RW access → mount/read-only remediation
//! - store locked → who holds it
//! - io_uring unavailable → use `--io-backend sync` (or why it failed)
//! - /dev/fuse or mount capability missing → what the kernel/container
//!   must provide
//! - store directory problems → mkfs vs open confusion
//!
//! The library errors remain structured (`StoreError`,
//! `CompatibilityError`); this is presentation only — programs must
//! never parse CLI prose.

#![forbid(unsafe_code)]

use crate::store::StoreError;

/// Classify a `Store::open` failure into an operator message.
pub fn open(dir: &std::path::Path, e: &StoreError) -> String {
    match e {
        StoreError::IncompatibleFormat(c) => {
            let what = if c.unknown_incompat != 0 {
                format!(
                    "unknown incompat feature bits 0x{:016x}",
                    c.unknown_incompat
                )
            } else if c.unknown_ro_compat != 0 {
                format!(
                    "unknown ro_compat feature bits 0x{:016x}",
                    c.unknown_ro_compat
                )
            } else {
                format!("format {}.{}", c.format_major, c.format_minor)
            };
            format!(
                "cannot open {}: {}; {} access refused — {}",
                dir.display(),
                what,
                c.access.name(),
                c.remediation
            )
        }
        StoreError::Locked => format!(
            "cannot open {}: the store is mounted or otherwise in use \
             (the mount lock is held); unmount it first",
            dir.display()
        ),
        StoreError::Superblock(m) => format!(
            "cannot open {}: no readable superblock/root ({m}); is this an \
             entropyfs store? (run `entropyfs mkfs {}` to create one)",
            dir.display(),
            dir.display()
        ),
        StoreError::Io(m) => {
            if m.contains("No such file or directory") {
                format!(
                    "cannot open {}: no such store (run `entropyfs mkfs {}` first)",
                    dir.display(),
                    dir.display()
                )
            } else {
                format!("cannot open {}: I/O error ({m})", dir.display())
            }
        }
        other => format!("cannot open {}: {other}", dir.display()),
    }
}

/// Classify a transport-build failure (mkfs/mount with an io backend).
pub fn transport(kind: crate::store::io::IoBackendKind, e: &crate::store::StoreError) -> String {
    match (kind, e) {
        (crate::store::io::IoBackendKind::Uring, StoreError::Io(m)) => format!(
            "io_uring transport unavailable: {m}; the kernel or container runtime \
             blocks io_uring_create — use `--io-backend sync` (the reference \
             path; the on-disk format is identical)"
        ),
        _ => e.to_string(),
    }
}

/// Classify a FUSE mount failure: /dev/fuse presence and the mount(2)
/// errno categories.
pub fn fuse_mount(mountpoint: &std::path::Path, e: &str) -> String {
    let lower = e.to_lowercase();
    if !std::path::Path::new("/dev/fuse").exists() {
        return format!(
            "cannot mount {}: /dev/fuse is unavailable — the kernel fuse module \
             must be loaded and the container/runtime must expose /dev/fuse",
            mountpoint.display()
        );
    }
    if lower.contains("permission") || lower.contains("operation not permitted") {
        return format!(
            "cannot mount {}: permission denied — mounting FUSE requires \
             CAP_SYS_ADMIN (or a setuid fusermount3 helper)",
            mountpoint.display()
        );
    }
    if lower.contains("no such file") {
        return format!(
            "cannot mount {}: mountpoint does not exist — create the directory first",
            mountpoint.display()
        );
    }
    format!("cannot mount {}: {e}", mountpoint.display())
}
