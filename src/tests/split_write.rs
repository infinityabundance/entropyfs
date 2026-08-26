//! Phase-10G regression: the kernel's write-back delivers a file's writes
//! as SPLIT requests (e.g. 0..446464 + 446464..524288 — the fuser
//! max_write boundary), possibly out of order (the tail write first). The
//! epoch write path must compose overlapping partial chunks (the RMW via
//! the overlay prefill) so the file's final content is byte-exact
//! regardless of the arrival order, and the committed state must fsck
//! clean. The mounted court hit this shape at threads=4 on the
//! identical-content corpus.

#![forbid(unsafe_code)]

use crate::optimizer::foreground::ForegroundPolicy;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::io::IoBackendKind;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};

fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let b = z.to_le_bytes();
        let take = (n - out.len()).min(8);
        out.extend_from_slice(&b[..take]);
    }
    out
}

#[test]
fn out_of_order_split_writes_compose_byte_exact() {
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
        let rand = noise(512 * 1024, 0xC0FFEE);
        let zero = vec![0u8; 512 * 1024];
        let groups: Vec<Vec<u8>> = (0..24)
            .map(|i| match i / 8 {
                0 => text.clone(),
                1 => rand.clone(),
                _ => zero.clone(),
            })
            .collect();
        let mut inos = Vec::new();
        for i in 0..24 {
            let ino = store
                .create_entry(
                    root,
                    format!("f{i}").as_bytes(),
                    NewEntry::file(0o644, 1000, 1000),
                    &CrashHooks::none(),
                )
                .unwrap();
            inos.push(ino);
        }
        // Each file: split writes (0..446464 + 446464..), submitted
        // concurrently (the kernel's out-of-order write-back order).
        for (i, ino) in inos.iter().enumerate() {
            let store_ref = &store;
            let content = groups[i].clone();
            let ino = *ino;
            std::thread::scope(|s2| {
                let w1 = content[..446464].to_vec();
                let w2 = content[446464..].to_vec();
                let h1 = s2.spawn(move || {
                    store_ref.epoch_write(
                        ino,
                        446464,
                        &w2,
                        OptimizeOptions::default(),
                        ForegroundPolicy::full(),
                        &CrashHooks::none(),
                    )
                });
                let h2 = s2.spawn(move || {
                    store_ref.epoch_write(
                        ino,
                        0,
                        &w1,
                        OptimizeOptions::default(),
                        ForegroundPolicy::full(),
                        &CrashHooks::none(),
                    )
                });
                for h in [h1, h2] {
                    h.join().unwrap().unwrap_or_else(|e| panic!("f{i}: {e:?}"));
                }
            });
            // A reader SERIALIZED with the writes (the FUSE read handler's
            // inode lock) must always see the full staged state.
            let _lock = store.inode_lock(ino);
            let ep = store.epoch();
            let got = store
                .read_file_epoch(&ep, ino, 0, content.len() as u64)
                .unwrap();
            drop(ep);
            drop(_lock);
            if got != content {
                let first = got
                    .iter()
                    .zip(content.iter())
                    .position(|(a, b)| a != b)
                    .unwrap_or(usize::MAX);
                panic!(
                    "{kind:?} f{i}: mismatch len {} first diff {first}",
                    got.len()
                );
            }
            store.epoch_checkpoint(&CrashHooks::none()).unwrap();
        }
        let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
        assert!(report.is_clean(), "{kind:?} fsck:\n{}", report.render());
    }
}
