//! FUSE mounting orchestration (§24, §47).
//!
//! Preflight checks (FUSE availability, mountpoint validity, recursive
//! backing detection), then spawn the fuser session. `spawn_mount` runs
//! the event loop on a background thread and returns a session handle;
//! dropping the handle unmounts. The CLI keeps the daemon alive and
//! handles signals.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use fuser::{Config, MountOption, SessionACL, spawn_mount};

use crate::platform::linux::{fuse_available, path_contains};
use crate::store::Store;

use super::filesystem::EntropyFs;

/// Mount errors (user-actionable, §47).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountError {
    /// Preflight failed.
    Preflight(String),
    /// Backing store would be inside the mountpoint (or vice versa).
    RecursiveBacking(String),
    /// The store could not be opened.
    Store(String),
    /// The mount syscall failed.
    Mount(String),
}

impl std::fmt::Display for MountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MountError::Preflight(m) => write!(f, "preflight: {m}"),
            MountError::RecursiveBacking(m) => write!(f, "recursive backing: {m}"),
            MountError::Store(m) => write!(f, "store: {m}"),
            MountError::Mount(m) => write!(f, "mount: {m}"),
        }
    }
}

impl std::error::Error for MountError {}

/// Mount parameters.
#[derive(Debug, Clone)]
pub struct MountParams {
    /// Store directory.
    pub store_dir: PathBuf,
    /// Mountpoint.
    pub mountpoint: PathBuf,
    /// Read-only mount.
    pub read_only: bool,
    /// Allow other users (session ACL All; requires fuse config).
    pub allow_other: bool,
    /// Event loop threads (v1 defaults to 1; concurrency lands after
    /// correctness is sealed).
    pub threads: usize,
    /// FUSE filesystem name shown in mtab.
    pub fs_name: String,
}

/// Preflight: validate the environment and the mount configuration (§47).
pub fn preflight(params: &MountParams) -> Result<(), MountError> {
    let avail = fuse_available();
    if !avail.ready() {
        return Err(MountError::Preflight(avail.diagnose().join("; ")));
    }
    if !params.mountpoint.exists() {
        return Err(MountError::Preflight(format!(
            "mountpoint {} does not exist",
            params.mountpoint.display()
        )));
    }
    if !params.mountpoint.is_dir() {
        return Err(MountError::Preflight(format!(
            "mountpoint {} is not a directory",
            params.mountpoint.display()
        )));
    }
    // Recursive backing detection (§47): the store must never live under
    // the mountpoint and the mountpoint must never live under the store.
    if path_contains(&params.mountpoint, &params.store_dir) {
        return Err(MountError::RecursiveBacking(format!(
            "the backing store {} is inside the mountpoint {}",
            params.store_dir.display(),
            params.mountpoint.display()
        )));
    }
    if path_contains(&params.store_dir, &params.mountpoint) {
        return Err(MountError::RecursiveBacking(format!(
            "the mountpoint {} is inside the backing store {}",
            params.mountpoint.display(),
            params.store_dir.display()
        )));
    }
    Ok(())
}

/// Mount the store and return a background session (drop to unmount).
pub fn mount(params: &MountParams, store: Store) -> Result<fuser::BackgroundSession, MountError> {
    preflight(params)?;
    let fs = EntropyFs::new(store);
    mount_fs(fs, params)
}

/// Mount an already-built filesystem (used by tests and the daemon).
pub fn mount_fs(
    fs: EntropyFs,
    params: &MountParams,
) -> Result<fuser::BackgroundSession, MountError> {
    let mut mount_options = vec![
        MountOption::FSName(params.fs_name.clone()),
        MountOption::Subtype("entropyfs".into()),
        MountOption::DefaultPermissions,
        MountOption::NoAtime,
    ];
    if params.read_only {
        mount_options.push(MountOption::RO);
    }
    let acl = if params.allow_other {
        SessionACL::All
    } else {
        SessionACL::Owner
    };
    // `Config` is #[non_exhaustive] upstream; mutate the default. clippy's
    // field_reassign_with_default does not apply here (no builder exists).
    #[allow(clippy::field_reassign_with_default)]
    let mut config = Config::default();
    config.mount_options = mount_options;
    config.acl = acl;
    config.n_threads = Some(params.threads.max(1));
    // Seed the kernel-cache invalidation handle once the session exists
    // (§24): the filesystem needs it to drop stale dentries after
    // unlink/rmdir/rename.
    let notifier_slot = fs.notifier_slot();
    let session = spawn_mount(fs, &params.mountpoint, &config)
        .map_err(|e| MountError::Mount(e.to_string()))?;
    if let Ok(mut slot) = notifier_slot.lock() {
        *slot = Some(session.notifier());
    }
    Ok(session)
}

/// Unmount via fusermount3 (the CLI unmount command).
pub fn unmount(mountpoint: &Path) -> Result<(), String> {
    let status = std::process::Command::new("fusermount3")
        .arg("-u")
        .arg(mountpoint)
        .status()
        .map_err(|e| format!("fusermount3: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("fusermount3 -u failed with {status}"))
    }
}
