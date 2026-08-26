//! Phase-10G regression: partial-window overlay reads (offset != 0 — the
//! kernel's page-granular read requests) of files with checkpoint-merged
//! writes. The read's scan window must extend to the PENDING predecessor
//! (the committed tree may lack the not-yet-merged writes, so the covering
//! pending extent starts BELOW the committed covering — the mounted court
//! hit holes at chunk boundaries exactly this way).

#![forbid(unsafe_code)]

use crate::optimizer::foreground::ForegroundPolicy;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::io::IoBackendKind;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};

#[test]
fn partial_window_reads_after_checkpoint_interleave() {
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
        let text = (b"the quick brown fox jumps over the lazy dog and the entropic ".repeat(16000))
            [..512 * 1024]
            .to_vec();
        let mut inos = Vec::new();
        for f in 0..4usize {
            let ino = store
                .create_entry(
                    root,
                    format!("f{f}").as_bytes(),
                    NewEntry::file(0o644, 1000, 1000),
                    &CrashHooks::none(),
                )
                .unwrap();
            inos.push(ino);
            // Four offset-split writes.
            for (i, off) in (0..text.len()).step_by(128 * 1024).enumerate() {
                let end = (off + 128 * 1024).min(text.len());
                store
                    .epoch_write(
                        ino,
                        off as u64,
                        &text[off..end],
                        OptimizeOptions::default(),
                        ForegroundPolicy::full(),
                        &CrashHooks::none(),
                    )
                    .unwrap_or_else(|e| panic!("w f{f} #{i}: {e:?}"));
            }
            store.epoch_checkpoint(&CrashHooks::none()).unwrap();
        }
        // Partial-window reads (the kernel's page-granular read requests).
        let ep = store.epoch();
        for ino in inos {
            for off in [0u64, 16384, 49152, 262144, 393216] {
                let got = store
                    .read_file_epoch(&ep, ino, off, 32768)
                    .unwrap_or_else(|e| panic!("read ino {ino} at {off}: {e:?}"));
                let want = &text[off as usize..(off + 32768) as usize];
                if got != want {
                    let first = got
                        .iter()
                        .zip(want.iter())
                        .position(|(a, b)| a != b)
                        .unwrap_or(usize::MAX);
                    panic!(
                        "{kind:?} read ino {ino} at {off}: mismatch (len {} want {}, first diff {first})",
                        got.len(),
                        want.len()
                    );
                }
            }
        }
        let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
        assert!(report.is_clean(), "{kind:?} fsck:\n{}", report.render());
    }
}
