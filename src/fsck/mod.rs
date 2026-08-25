//! Independent validation and repair (`docs/recovery/fsck.md`). fsck does
//! not merely call the happy-path mounted APIs; it independently walks and
//! validates persistent structures.

#![forbid(unsafe_code)]

// (module populated by the fsck implementation step)
