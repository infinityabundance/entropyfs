//! Linux POSIX/VFS adapter (FUSE) (ADR-0002).
//!
//! Converts FUSE operations into storage-engine transactions. Contains no
//! entropy algorithms: every representation decision is made by the
//! engine and validated byte-exact before commit.
//!
//! Layering: `filesystem` (the `fuser::Filesystem` impl) → `file` /
//! `directory` / `xattr` / `inode` (adapters) → `Store`. The store never
//! depends on this module.

#![forbid(unsafe_code)]

pub mod directory;
pub mod file;
pub mod filesystem;
pub mod inode;
pub mod locking;
pub mod mount;
pub mod xattr;
