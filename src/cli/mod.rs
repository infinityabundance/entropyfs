//! CLI commands (ADR-0002): mkfs, mount, unmount, status, inspect,
//! explain, snapshot(s), fsck, scrub, gc, optimize, benchmark,
//! capabilities.

#![forbid(unsafe_code)]

pub mod benchmark;
pub mod capabilities;
pub mod explain;
pub mod fsck;
pub mod gc;
pub mod inspect;
pub mod mkfs;
pub mod mount;
pub mod optimize;
pub mod snapshot;
pub mod status;
pub mod ublk;
pub mod unmount;
