//! Engine API smoke (Phase 12E.8, stage 8): the minimal embeddable-engine
//! exercise the distribution court runs in every container.
//!
//! Creates a store, puts a blob, gets it back, verifies byte identity,
//! syncs, closes, reopens, verifies again. Prints one line per step and
//! `engine smoke: OK` at the end; exits non-zero on any failure.
//!
//! Usage: entropyfs-engine-smoke <store-dir> [--io-backend sync|uring]

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = args
        .get(1)
        .map(PathBuf::from)
        .expect("usage: engine_smoke <store-dir> [--io-backend sync|uring]");
    let io_backend = match args.iter().position(|a| a == "--io-backend") {
        Some(i) => args
            .get(i + 1)
            .map(|s| entropyfs::store::io::IoBackendKind::parse(s).expect("backend"))
            .unwrap_or(entropyfs::store::io::IoBackendKind::Sync),
        None => entropyfs::store::io::IoBackendKind::Sync,
    };

    let opts = entropyfs::engine::EngineOpenOptions {
        io_backend,
        ..Default::default()
    };
    let payload = b"distro-court engine smoke payload: 0123456789abcdef".to_vec();

    let engine = entropyfs::engine::Engine::create(&dir, &opts).expect("engine create");
    let id = engine.put_blob(&payload).expect("put");
    let got = engine.get_blob(id).expect("get");
    assert_eq!(got, payload, "byte identity after put/get");
    println!("put/get byte-exact: ok ({} bytes)", payload.len());
    engine.sync().expect("sync");
    let m = engine.metrics().expect("metrics");
    println!(
        "metrics: blob_count={} format={}.{} backend={}",
        m.accounting.blob_count, m.format.format_major, m.format.format_minor, m.format.io_backend
    );
    engine.close().expect("close");

    let engine = entropyfs::engine::Engine::open(&dir, &opts).expect("engine reopen");
    let got2 = engine.get_blob(id).expect("get after reopen");
    assert_eq!(got2, payload, "byte identity after reopen");
    engine.close().expect("close");

    println!("engine smoke: OK");
}
