//! Phase-10G regression: the court's parallel namespace pattern
//! (mkdir/create/write/read/unlink/rmdir loops) at the store level —
//! concurrent epoch ops on disjoint subtrees must stay byte-exact and
//! fsck-clean. The mounted version of this court exposed the stale
//! committed-dir_root bug (an overlay inode's dir_root goes stale once a
//! checkpoint publishes a newer directory tree, so lookups missed
//! epoch-merged entries with "no such entry"); this store-level stress
//! keeps the pattern pinned on both io backends.

#![forbid(unsafe_code)]

use crate::optimizer::foreground::ForegroundPolicy;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::io::IoBackendKind;
use crate::store::transaction::CrashHooks;
use crate::store::{AttrUpdate, NewEntry, Store, StoreConfig};

#[test]
fn namespace_pattern_repro() {
    for kind in IoBackendKind::ALL {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::create(
            dir.path(),
            &StoreConfig {
                io_backend: kind,
                ..Default::default()
            },
            [0xAB; 16],
        )
        .unwrap();
        let root = store.current_root().root_dir_ino;
        let errors: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for w in 0..2usize {
                let store = &store;
                let errors = &errors;
                s.spawn(move || {
                    for i in 0..200usize {
                        let dname = format!("w{w}-d{i}");
                        let ino_d = store
                            .epoch_create(
                                root,
                                dname.as_bytes(),
                                NewEntry::dir(0o755, 1000, 1000),
                                &CrashHooks::none(),
                            )
                            .unwrap_or_else(|e| {
                                panic!("w{w} i{i} mkdir: {e:?}");
                            });
                        let fname = format!("f{i}");
                        let ino_f = store
                            .epoch_create(
                                ino_d,
                                fname.as_bytes(),
                                NewEntry::file(0o644, 1000, 1000),
                                &CrashHooks::none(),
                            )
                            .unwrap_or_else(|e| {
                                panic!("w{w} i{i} create: {e:?}");
                            });
                        let data = vec![b'x'; 4096];
                        store
                            .epoch_write(
                                ino_f,
                                0,
                                &data,
                                OptimizeOptions::default(),
                                ForegroundPolicy::full(),
                                &CrashHooks::none(),
                            )
                            .unwrap_or_else(|e| panic!("w{w} i{i} write: {e:?}"));
                        store
                            .epoch_setattr(
                                ino_f,
                                &AttrUpdate {
                                    size: Some(data.len() as u64),
                                    ..Default::default()
                                },
                                &CrashHooks::none(),
                            )
                            .unwrap_or_else(|e| panic!("w{w} i{i} setattr: {e:?}"));
                        let ep = store.epoch();
                        let got = store
                            .read_file_epoch(&ep, ino_f, 0, data.len() as u64)
                            .unwrap_or_else(|e| panic!("w{w} i{i} read: {e:?}"));
                        drop(ep);
                        if got != data {
                            panic!("w{w} i{i} read mismatch");
                        }
                        store
                            .epoch_unlink(ino_d, fname.as_bytes(), false, &CrashHooks::none())
                            .unwrap_or_else(|e| panic!("w{w} i{i} unlink: {e:?}"));
                        match store.epoch_unlink(root, dname.as_bytes(), true, &CrashHooks::none())
                        {
                            Ok(_) => {}
                            Err(e) => {
                                let mut errs = errors.lock().unwrap();
                                errs.push(format!("w{w} i{i} rmdir: {e:?}"));
                            }
                        }
                    }
                });
            }
        });
        let errs = errors.lock().unwrap().clone();
        assert!(errs.is_empty(), "{kind:?}:\n{}", errs.join("\n"));
        let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
        assert!(report.is_clean(), "{kind:?} fsck:\n{}", report.render());
    }
}
