//! Phase-10G regression: parallel identical-content writes must not
//! self-alias the chunk index.
//!
//! The FUSE write path emits `EXACT_REF{target: cid}` for a chunk whose
//! content id is already committed (the dedup alias). The epoch write path
//! used to register that self-referencing descriptor in the pending chunk
//! map, so the next checkpoint's `apply_sorted_batch` UPSERT replaced the
//! retained terminal descriptor with a self-loop — `materialize(cid)` then
//! recursed into itself until `DepthExceeded` (depth 5 > 4), surfacing as
//! EIO on concurrent `cp` of duplicated files (`xargs -P`), and on the
//! post-10D FUSE pattern write + setattr(size) + read. The transactional
//! path already guarded this in `put_chunk_in_tx`; the epoch path now
//! mirrors it.

#![forbid(unsafe_code)]

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

/// Deterministic 512 KiB corpus of 8 identical 64 KiB chunks (a periodic
/// pattern: PERIODIC encoding, no dictionaries).
fn corpus() -> Vec<u8> {
    (b"the quick brown fox jumps over the lazy dog and the entropic filesystem persists irreducible state. "
        .repeat(16000))[..512 * 1024]
        .to_vec()
}

/// The exact FUSE pattern per file: one full-file epoch write, then the
/// kernel's post-write setattr carrying the SIZE (which checkpoints the
/// epoch, flushing other files' pending writes), then an overlay read-back.
fn fuse_pattern(
    store: &Store,
    ino: u64,
    text: &[u8],
    errors: &std::sync::Mutex<Vec<String>>,
    tag: &str,
) {
    if let Err(e) = store.epoch_write(
        ino,
        0,
        text,
        OptimizeOptions::default(),
        crate::optimizer::foreground::ForegroundPolicy::full(),
        &CrashHooks::none(),
    ) {
        errors.lock().unwrap().push(format!("{tag} write: {e:?}"));
        return;
    }
    if let Err(e) = store.epoch_setattr(
        ino,
        &AttrUpdate {
            size: Some(text.len() as u64),
            ..Default::default()
        },
        &CrashHooks::none(),
    ) {
        errors.lock().unwrap().push(format!("{tag} setattr: {e:?}"));
        return;
    }
    let ep = store.epoch();
    match store.read_file_epoch(&ep, ino, 0, text.len() as u64) {
        Ok(b) if b != text => {
            errors.lock().unwrap().push(format!(
                "{tag}: READ MISMATCH (len {}, expected {})",
                b.len(),
                text.len()
            ));
        }
        Ok(_) => {}
        Err(e) => errors
            .lock()
            .unwrap()
            .push(format!("{tag}: read error {e:?}")),
    }
}

#[test]
fn parallel_identical_writes_never_self_alias_chunk_index() {
    for kind in IoBackendKind::ALL {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::create(dir.path(), &cfg(kind), [0xAB; 16]).unwrap();
        let text = corpus();
        let errors: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let errors_ref = &errors;
        // Warm the chunk index with the content (committed terminal
        // descriptors), then run the concurrent pattern in rounds so the
        // dedup alias fires against committed entries.
        let warm = store
            .create_entry(
                1,
                b"warm",
                NewEntry::file(0o644, 1000, 1000),
                &CrashHooks::none(),
            )
            .unwrap();
        fuse_pattern(&store, warm, &text, errors_ref, "warm");
        assert!(
            errors.lock().unwrap().is_empty(),
            "warm failures: {:?}",
            errors.lock().unwrap()
        );
        for round in 0..4u64 {
            let mut inos = Vec::new();
            for i in 0..4u64 {
                let ino = store
                    .create_entry(
                        1,
                        format!("r{round}f{i}").as_bytes(),
                        NewEntry::file(0o644, 1000, 1000),
                        &CrashHooks::none(),
                    )
                    .unwrap();
                inos.push(ino);
            }
            std::thread::scope(|s| {
                for (i, ino) in inos.iter().enumerate() {
                    let store = &store;
                    let text = &text;
                    let tag = format!("round {round} file {i}");
                    s.spawn(move || fuse_pattern(store, *ino, text, errors_ref, &tag));
                }
            });
        }
        let errors = errors.into_inner().unwrap();
        assert!(
            errors.is_empty(),
            "{kind:?}: parallel identical-content writes self-aliased the chunk index:\n{}",
            errors.join("\n")
        );
        // Remount + full read + fsck must stay clean.
        drop(store);
        let store = Store::open(dir.path(), &cfg(kind)).unwrap();
        {
            let ino = warm;
            let got = store.read_file(ino, 0, text.len() as u64).unwrap();
            assert_eq!(got, text, "{kind:?} remount read mismatch");
        }
        let report = crate::fsck::fsck(dir.path(), &crate::fsck::FsckOptions::default()).unwrap();
        assert!(
            report.is_clean(),
            "{kind:?} fsck after the regression:\n{}",
            report.render()
        );
    }
}
