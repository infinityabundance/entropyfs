//! FUSE xattr adaptation.
//!
//! xattrs are stored per-inode in a persistent B-tree rooted at
//! `inode.xattr_root` (names are raw bytes; values bounded by
//! `XATTR_SIZE_MAX`). `user.*` and `security.*` namespaces are stored
//! verbatim; the `system.posix_acl_*` namespace is reported as unsupported
//! in v1 (ACLs arrive with a later phase).

#![forbid(unsafe_code)]

/// Whether an xattr name is storable in v1.
pub fn supported_name(name: &[u8]) -> bool {
    !name.starts_with(b"system.posix_acl_access")
        && !name.starts_with(b"system.posix_acl_default")
        && !name.starts_with(b"system.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_policy() {
        assert!(supported_name(b"user.comment"));
        assert!(supported_name(b"security.selinux"));
        assert!(!supported_name(b"system.posix_acl_access"));
    }
}
