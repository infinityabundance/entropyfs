//! Linux-specific machinery (safe Rust only; see `docs/security/
//! unsafe-ledger.md` — the crate currently has zero unsafe code).
//!
//! Owns the narrowly isolated Linux integration surface: FUSE device and
//! fusermount detection, kernel feature probing, path-containment checks,
//! and statvfs wrappers.

#![forbid(unsafe_code)]

use std::path::Path;

/// Result of a FUSE availability preflight (§47).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseAvailability {
    /// `/dev/fuse` present and accessible.
    pub dev_fuse: bool,
    /// `fusermount3` found in PATH.
    pub fusermount3: bool,
    /// `fuse` filesystem registered in `/proc/filesystems`.
    pub kernel_fuse: bool,
}

impl FuseAvailability {
    /// Whether mounting is expected to work.
    pub fn ready(&self) -> bool {
        self.dev_fuse && self.fusermount3 && self.kernel_fuse
    }

    /// Human-readable diagnosis for the CLI (§47).
    pub fn diagnose(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.dev_fuse {
            out.push(
                "/dev/fuse is missing or not accessible — load the fuse module \
                 (`modprobe fuse`) and ensure /dev/fuse exists"
                    .into(),
            );
        }
        if !self.fusermount3 {
            out.push("`fusermount3` not found in PATH — install fuse3 (pacman -S fuse3)".into());
        }
        if !self.kernel_fuse {
            out.push("the kernel reports no `fuse` filesystem — load the fuse module".into());
        }
        out
    }
}

/// Preflight FUSE availability.
pub fn fuse_available() -> FuseAvailability {
    FuseAvailability {
        dev_fuse: Path::new("/dev/fuse").exists(),
        fusermount3: which("fusermount3"),
        kernel_fuse: kernel_has_fuse(),
    }
}

/// Whether the kernel has the `fuse` filesystem registered.
pub fn kernel_has_fuse() -> bool {
    std::fs::read_to_string("/proc/filesystems")
        .map(|s| s.lines().any(|l| l.contains("fuse")))
        .unwrap_or(false)
}

/// Whether the kernel was built with FUSE-over-io_uring support (informational).
pub fn kernel_fuse_io_uring() -> bool {
    std::fs::read_to_string("/sys/module/fuse/parameters/io_uring")
        .map(|s| s.trim() == "Y" || s.trim() == "1")
        .unwrap_or(false)
}

/// Look up a program in PATH.
pub fn which(prog: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join(prog))
                .any(|f| f.is_file())
        })
        .unwrap_or(false)
}

/// Whether `child` is `parent` itself or located underneath it
/// (canonicalized prefix check). Used to reject recursive backing
/// configurations (§47: never store the backing store under its own
/// mount).
pub fn path_contains(parent: &Path, child: &Path) -> bool {
    let parent = match std::fs::canonicalize(parent) {
        Ok(p) => p,
        Err(_) => parent.to_path_buf(),
    };
    let child = match std::fs::canonicalize(child) {
        Ok(c) => c,
        Err(_) => child.to_path_buf(),
    };
    child.starts_with(&parent)
}

/// statvfs summary of a path (blocks, frsize, bfree, bavail, files, ffree).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatvfsInfo {
    /// Total blocks.
    pub blocks: u64,
    /// Fragment size.
    pub frsize: u64,
    /// Free blocks.
    pub bfree: u64,
    /// Free blocks for unprivileged users.
    pub bavail: u64,
    /// Total inodes.
    pub files: u64,
    /// Free inodes.
    pub ffree: u64,
}

/// statvfs through rustix (safe).
pub fn statvfs(path: &Path) -> Option<StatvfsInfo> {
    use rustix::fs::statvfs as statvfs_call;
    match statvfs_call(path) {
        Ok(s) => Some(StatvfsInfo {
            blocks: s.f_blocks,
            frsize: s.f_frsize,
            bfree: s.f_bfree,
            bavail: s.f_bavail,
            files: s.f_files,
            ffree: s.f_ffree,
        }),
        Err(_) => None,
    }
}

/// Page size in bytes (safe sysconf wrapper).
pub fn page_size() -> u64 {
    rustix::param::page_size() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_containment() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = tmp.path();
        let nested = base.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(path_contains(base, &nested));
        assert!(path_contains(base, base));
        let other = tempfile::TempDir::new().unwrap();
        assert!(!path_contains(base, other.path()));
    }

    #[test]
    fn page_size_sane() {
        let ps = page_size();
        assert!(ps == 4096 || ps == 16384 || ps == 65536);
    }
}
