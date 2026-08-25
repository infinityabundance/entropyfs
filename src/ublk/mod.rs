//! Experimental ublk frontend (ADR-0020, §36 Phase 7).
//!
//! Exposes the *same* entropy storage engine as a Linux block device so
//! filesystems (ext4/XFS) can be layered above it experimentally. The
//! device is a hidden file in the store; block I/O runs through the normal
//! representation engine (dedup, base+residual, rANS, configurational all
//! apply). The FUSE frontend and this block frontend share one store —
//! nothing is duplicated.
//!
//! The kernel control-plane binding (the `ublk_drv` module) is provided by
//! `target.rs` via the `libublk` crate; running it requires root and
//! `CONFIG_BLK_DEV_UBLK` (present on CachyOS as a module). The
//! `BlockStore` adapter itself is fully testable without the kernel.

#![forbid(unsafe_code)]

pub mod block;
pub mod target;
