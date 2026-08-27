//! Unsafe-ledger enforcement (ADR-0021, `docs/security/unsafe-ledger.md`):
//! the set of files under `src/` containing `unsafe` must equal the
//! ledger's designated file list — currently exactly one file,
//! `platform/io_uring.rs` (the io_uring SQE submission boundary).

#![forbid(unsafe_code)]

/// The ledger's designated unsafe files, relative to the crate root
/// (`src/`). Adding a file here WITHOUT a ledger entry is a policy
/// violation; the ledger doc describes the required preconditions.
///
/// Phase 12E.14: `ffi/mod.rs` is the SECOND designated file — the C ABI
/// boundary (raw pointers across `extern "C"`). Its exact preconditions
/// live in the ledger doc; the C smoke test (`tools/ffi-smoke.sh`) and
/// the Rust FFI court (`src/tests/ffi_cabi.rs`) exercise them.
const LEDGER_UNSAFE_FILES: &[&str] = &["platform/io_uring.rs", "ffi/mod.rs"];

#[test]
fn unsafe_files_match_ledger() {
    let mut found: Vec<String> = Vec::new();
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).expect("read src dir");
        for entry in entries {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let rel = path
                .strip_prefix(&src)
                .expect("under src")
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(&path).expect("read file");
            // The `unsafe` KEYWORD, excluding doc/comment lines. The
            // `\bunsafe\b`-style check below never matches `unsafe_code`
            // (lint attrs), `unsafe_ledger` (docs), or `unsafe_files_*`
            // (this test), because `_` is a word character.
            let has_unsafe = text.lines().any(|l| {
                let t = l.trim_start();
                if t.starts_with("//") {
                    return false;
                }
                // The `unsafe` KEYWORD is always a standalone whitespace-
                // delimited token, optionally followed by `{` (`unsafe {`).
                // Hyphenated prose (`unsafe-ledger`) and identifiers
                // (`unsafe_code`, `unsafe_ledger`, `unsafe_files_*`) never
                // match.
                t.split_whitespace().any(|w| {
                    w.strip_suffix('{').unwrap_or(w) == "unsafe"
                        || w.strip_suffix(";").unwrap_or(w) == "unsafe"
                })
            });
            if has_unsafe {
                found.push(rel);
            }
        }
    }
    found.sort();
    let mut expected: Vec<&str> = LEDGER_UNSAFE_FILES.to_vec();
    expected.sort();
    assert_eq!(
        found, expected,
        "ledger mismatch: got {found:?}, ledger {expected:?} (add a ledger entry \
         when the safety boundary expands)"
    );
}
