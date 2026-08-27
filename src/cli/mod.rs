//! CLI commands (ADR-0002): mkfs, mount, unmount, status, inspect,
//! explain, snapshot(s), fsck, scrub, gc, optimize, benchmark,
//! capabilities.

#![forbid(unsafe_code)]

pub mod benchmark;
pub mod capabilities;
pub mod evidence;
pub mod explain;
pub mod fsck;
pub mod gc;
pub mod inspect;
pub mod mkfs;
#[cfg(feature = "fuse")]
pub mod mount;
pub mod optimize;
pub mod snapshot;
pub mod status;
#[cfg(feature = "ublk")]
pub mod ublk;
#[cfg(feature = "fuse")]
pub mod unmount;
