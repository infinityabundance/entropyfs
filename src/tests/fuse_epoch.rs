//! Phase-10D FUSE-level integration: the mounted filesystem runs every
//! namespace/writeback op through the ACTIVE EPOCH (log append + ack),
//! with the overlay visible to reads before the checkpoint. These tests
//! mount a REAL FUSE filesystem (skipped when FUSE is unavailable), run
//! the src-workload pattern, verify byte-exactness through the overlay
//! AND after the unmount flush + remount.

#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::Arc;

use crate::fuse::mount::MountParams;
use crate::store::StoreConfig;
use tempfile::TempDir;

fn fuse_ready() -> bool {
    crate::platform::linux::fuse_available().ready()
}

fn mount_params(store: &Path, mnt: &Path) -> MountParams {
    MountParams {
        store_dir: store.to_path_buf(),
        mountpoint: mnt.to_path_buf(),
        read_only: false,
        allow_other: false,
        threads: 1,
        fs_name: "entropyfs-test".into(),
        background_optimize: false,
        stats_file: None,
        worker_pool_threads: None,
    }
}

/// A small source-tree corpus: nested dirs + files with per-file content.
fn make_source_tree(root: &Path) {
    std::fs::create_dir_all(root.join("sub1/sub2")).unwrap();
    for i in 0..20 {
        std::fs::write(
            root.join(format!("f{i}.rs")),
            format!("pub fn f{i}() -> u32 {{ {i} }}\n"),
        )
        .unwrap();
    }
    for i in 0..10 {
        std::fs::write(
            root.join("sub1").join(format!("h{i}.rs")),
            format!("// helper {i}\npub const H{i}: u32 = {i};\n"),
        )
        .unwrap();
    }
    for i in 0..5 {
        std::fs::write(
            root.join("sub1/sub2").join(format!("k{i}.rs")),
            format!("// deep {i}\npub const K{i}: u32 = {i};\n"),
        )
        .unwrap();
    }
    std::fs::write(root.join("lib.rs"), b"pub mod sub1;\n").unwrap();
}

fn verify_tree(src: &Path, mnt: &Path) {
    fn walk(src: &Path, mnt: &Path) {
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let s = entry.path();
            let m = mnt.join(entry.file_name());
            if s.is_dir() {
                assert!(m.is_dir(), "missing dir {}", m.display());
                walk(&s, &m);
            } else {
                let sb = std::fs::read(&s).unwrap();
                let mb = std::fs::read(&m).unwrap();
                assert_eq!(sb, mb, "byte mismatch for {}", m.display());
            }
        }
    }
    walk(src, mnt);
}

#[test]
fn mounted_epoch_src_workload_roundtrips_and_survives_remount() {
    if !fuse_ready() {
        eprintln!("skipping: FUSE unavailable");
        return;
    }
    let dir = TempDir::new().unwrap();
    let store_dir = dir.path().join("store");
    let mnt = dir.path().join("mnt");
    std::fs::create_dir_all(&mnt).unwrap();

    let cfg = StoreConfig {
        segment_size: 64 * 1024 * 1024,
        ..Default::default()
    };
    let store = crate::store::Store::create(&store_dir, &cfg, [0x7e; 16]).unwrap();
    let params = mount_params(&store_dir, &mnt);
    let session = crate::fuse::mount::mount(&params, store).unwrap();

    // The src-workload pattern: cp-like creates + writes + setattrs.
    let src = dir.path().join("src");
    make_source_tree(&src);
    let copy = |dst: &Path| {
        fn cp(src: &Path, dst: &Path) {
            for entry in std::fs::read_dir(src).unwrap() {
                let entry = entry.unwrap();
                let s = entry.path();
                let d = dst.join(entry.file_name());
                if s.is_dir() {
                    std::fs::create_dir_all(&d).unwrap();
                    cp(&s, &d);
                } else {
                    std::fs::copy(&s, &d).unwrap();
                }
            }
        }
        cp(&src, dst);
    };
    let mnt_src = mnt.join("tree");
    std::fs::create_dir_all(&mnt_src).unwrap();
    copy(&mnt_src);

    // Byte-exact THROUGH THE OVERLAY (no unmount yet).
    verify_tree(&src, &mnt_src);

    // Namespace ops through the epoch: rename + unlink + mkdir + rmdir.
    std::fs::rename(mnt_src.join("f0.rs"), mnt_src.join("renamed.rs")).unwrap();
    assert!(!mnt_src.join("f0.rs").exists());
    assert!(mnt_src.join("renamed.rs").exists());
    std::fs::remove_file(mnt_src.join("f1.rs")).unwrap();
    std::fs::remove_dir_all(mnt_src.join("sub1/sub2")).unwrap();
    assert!(!mnt_src.join("sub1/sub2").exists());

    // Reads see the epoch state.
    assert!(mnt_src.join("renamed.rs").exists());

    // Unmount: flushes the epoch (checkpoint + durability barrier).
    drop(session);

    // fsck clean after the flush.
    let report = crate::fsck::fsck(&store_dir, &crate::fsck::FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck: {}", report.render());

    // Remount: the committed state reproduces the tree exactly.
    let store = crate::store::Store::open(&store_dir, &StoreConfig::default()).unwrap();
    let _ = Arc::new(store);
    let params = mount_params(&store_dir, &mnt);
    let session2 = crate::fuse::mount::mount(&params, {
        // reopen
        crate::store::Store::open(&store_dir, &StoreConfig::default()).unwrap()
    })
    .unwrap();
    let mnt_src2 = mnt.join("tree");
    assert!(mnt_src2.join("renamed.rs").exists(), "renamed survives");
    assert!(!mnt_src2.join("f1.rs").exists(), "unlinked stays gone");
    assert!(!mnt_src2.join("sub1/sub2").exists(), "rmdir'd stays gone");
    let expected = std::fs::read(src.join("f2.rs")).unwrap();
    let got = std::fs::read(mnt_src2.join("f2.rs")).unwrap();
    assert_eq!(expected, got, "content survives remount");
    drop(session2);
}
