//! Phase-10G regression: the FUSE write-request shape (offset-split
//! epoch writes + post-write setattr(size) checkpoint flushes) must never
//! corrupt the committed tree under parallel identical-content writes.
//!
//! `cp` delivers each 512 KiB file as FOUR 128 KiB FUSE write requests
//! (the fuser default max_write) at offsets 0/128K/256K/384K, followed by
//! the kernel's setattr(size). Concurrent `cp` instances (threads=4 on
//! the mounted court) run these sequences for byte-identical files
//! (dedup-heavy: EXACT_REF aliasing) while every setattr(size) flushes
//! the epoch — overlapping checkpoints from different files' flushes.
//!
//! Before the fix, two overlapping checkpoints could merge a STALE
//! snapshot: a checkpoint that snapshotted BEFORE a later write's stage
//! but COMMITTED after another checkpoint published that write merged its
//! stale inode (older size) on top of the NEWER committed tree (whose
//! tail extents stayed), so the file's committed size regressed while its
//! tail extents remained — fsck's "extent ends beyond file size" error,
//! short (393216-byte) reads on the mount, and EIO-class corruption.
//!
//! The fix snapshots the overlay UNDER the commit lock: a checkpoint
//! that waited behind another checkpoint merges the CURRENT overlay,
//! never a pre-wait snapshot.
//!
//! This test reproduces the exact court shape on BOTH io backends:
//! 24 files in byte-identical groups of 8, 4 concurrent writers, each
//! file as four offset-split epoch writes + setattr(size), 8 rounds;
//! every file must read back byte-exact (overlay + remount) and fsck
//! must stay clean.

#![forbid(unsafe_code)]

use crate::optimizer::foreground::ForegroundPolicy;
use crate::optimizer::policy::OptimizeOptions;
use crate::store::io::IoBackendKind;
use crate::store::transaction::CrashHooks;
use crate::store::{AttrUpdate, NewEntry, Store, StoreConfig};

fn cfg(kind: IoBackendKind) -> StoreConfig {
    StoreConfig {
        io_backend: kind,
        ..Default::default()
    }
}

/// Deterministic byte-uniform noise (SplitMix64).
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

/// Compressible periodic text (the court's text corpus family).
fn text512() -> Vec<u8> {
    (b"the quick brown fox jumps over the lazy dog and the entropic ".repeat(16000))[..512 * 1024]
        .to_vec()
}

/// The court's per-file sequence: four 128 KiB writes (the fuser default
/// max_write request size), then the kernel's post-write setattr(size)
/// (a checkpoint flush), then a full overlay read-back.
fn fuse_file_sequence(
    store: &Store,
    ino: u64,
    content: &[u8],
    tag: &str,
    errors: &std::sync::Mutex<Vec<String>>,
) {
    let req = 128 * 1024;
    for (i, off) in (0..content.len()).step_by(req).enumerate() {
        let end = (off + req).min(content.len());
        if let Err(e) = store.epoch_write(
            ino,
            off as u64,
            &content[off..end],
            OptimizeOptions::default(),
            ForegroundPolicy::full(),
            &CrashHooks::none(),
        ) {
            errors
                .lock()
                .unwrap()
                .push(format!("{tag} write#{i}: {e:?}"));
            return;
        }
    }
    if let Err(e) = store.epoch_setattr(
        ino,
        &AttrUpdate {
            size: Some(content.len() as u64),
            ..Default::default()
        },
        &CrashHooks::none(),
    ) {
        errors.lock().unwrap().push(format!("{tag} setattr: {e:?}"));
        return;
    }
    let ep = store.epoch();
    match store.read_file_epoch(&ep, ino, 0, content.len() as u64) {
        Ok(b) if b != content => {
            let sz = store
                .get_inode_epoch(&ep, ino)
                .ok()
                .flatten()
                .map(|i| i.size)
                .unwrap_or(0);
            let csz = store
                .get_inode(ino)
                .ok()
                .flatten()
                .map(|i| i.size)
                .unwrap_or(0);
            let pend_ext: Vec<(u64, usize)> = ep
                .pending_extents
                .iter()
                .filter(|((f, _), _)| *f == ino)
                .map(|((_, off), b)| (*off, b.len()))
                .collect();
            errors.lock().unwrap().push(format!(
                "{tag}: READ MISMATCH (len {}, expected {}, overlay size {sz}, committed size {csz}, pending extents {pend_ext:?})",
                b.len(),
                content.len()
            ));
        }
        Ok(_) => {}
        Err(e) => errors.lock().unwrap().push(format!("{tag}: read {e:?}")),
    }
}

#[test]
fn parallel_offset_split_writes_with_setattr_flushes_stay_byte_exact() {
    for kind in IoBackendKind::ALL {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::create(dir.path(), &cfg(kind), [0xAB; 16]).unwrap();
        let root = store.current_root().root_dir_ino;
        let text = text512();
        let rand = noise(512 * 1024, 0xC0FFEE);
        let zero = vec![0u8; 512 * 1024];
        // 24 files in three byte-identical groups of 8 (the court corpus
        // shape: dedup-heavy, EXACT_REF aliasing against committed
        // terminals).
        let groups: Vec<(String, Vec<u8>)> = (0..24)
            .map(|i| {
                let g = i / 8;
                let c = match g {
                    0 => text.clone(),
                    1 => rand.clone(),
                    _ => zero.clone(),
                };
                (format!("f{i}"), c)
            })
            .collect();

        let errors: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let mut last_round_inos: Vec<u64> = Vec::new();
        for round in 0..8 {
            let mut inos: Vec<u64> = Vec::new();
            for (name, _) in &groups {
                let ino = store
                    .create_entry(
                        root,
                        format!("r{round}-{name}").as_bytes(),
                        NewEntry::file(0o644, 1000, 1000),
                        &CrashHooks::none(),
                    )
                    .unwrap();
                inos.push(ino);
            }
            last_round_inos = inos.clone();
            std::thread::scope(|s| {
                let mut hs = Vec::new();
                for (i, ((name, content), ino)) in groups.iter().zip(&inos).enumerate() {
                    let store = &store;
                    let content = content.clone();
                    let ino = *ino;
                    let tag = format!("round {round} {name} ({i})");
                    let errors = &errors;
                    hs.push(s.spawn(move || {
                        fuse_file_sequence(store, ino, &content, &tag, errors);
                    }));
                }
                for h in hs {
                    h.join().unwrap();
                }
            });
            let errs = errors.lock().unwrap().clone();
            if !errs.is_empty() {
                panic!("{kind:?}: round {round} failures:\n{}", errs.join("\n"));
            }
        }

        // Remount + committed read of every file + fsck (the LAST round's
        // inos; earlier rounds' files are still committed and readable).
        drop(store);
        let store = Store::open(dir.path(), &cfg(kind)).unwrap();
        for ((name, content), ino) in groups.iter().zip(&last_round_inos) {
            let back = store.read_file(*ino, 0, content.len() as u64).unwrap();
            if back != *content {
                let first = back
                    .iter()
                    .zip(content.iter())
                    .position(|(a, b)| a != b)
                    .unwrap_or(usize::MAX);
                panic!(
                    "{kind:?} remount: {name} (ino {ino}) mismatch: len {} expected {}, first diff at byte {first}",
                    back.len(),
                    content.len()
                );
            }
        }
        let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
        assert!(report.is_clean(), "{kind:?} fsck:\n{}", report.render());
    }
}
