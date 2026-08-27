# Golden stores (Phase 12E.4)

## Purpose

`testdata/golden/<era>/` holds **actual historical stores** — created by
historical EntropyFS binaries (v0.3.0, v0.5.2, v0.6.3), never regenerated
by the current encoder. They are the compatibility contract's executable
proof: the current build must open each one, agree with its feature
decision, fsck it clean, enumerate it, and materialize every file
byte-exact, or CI fails.

## Eras

| Era | Creator | Format | Signature |
|-----|---------|--------|-----------|
| `v0.3.0` | v0.3.0 (Phase 8, SequenceDict era) | v1 | incompat bits 1–6,10–12; transaction-path writes |
| `v0.5.2` | v0.5.2 (pre-epoch final form) | v1 | incompat bits 1–6,10–14; transaction-path writes |
| `v0.6.3` | v0.6.3 (Phase 10D epoch era) | v1 | incompat bit 15 (MUTATION_LOG); epoch-path writes, 6 log records |

The logical corpus is identical across eras (deterministic; `hello.txt`,
`structured.dat`, `random.bin`), so the manifest hashes are the same
everywhere — the encoders differ, the bytes must not.

## Policy

- **Fixtures are immutable.** `store_dir_hash` pins the store directory
  bytes; a change fails the golden court.
- **Regeneration is never done with the current encoder.** If a fixture
  is lost, recreate it with `tools/make-golden-fixtures.sh` (which checks
  out the era tag, builds the historical binary, and runs a historical
  test to write the store) and review the diff — inode timestamps embed
  wall-clock time, so a recreated fixture's bytes differ even though its
  structure and logical content should not.
- **A future release that cannot decode a supported fixture must fail
  CI** (`src/tests/golden_store.rs`).
- **Declared-unsupported eras** are documented here, never silently
  dropped: pre-store experimental versions (before the Phase-2 persistent
  store) never had a persistent format to decode; there is no v0.1/v0.2
  store fixture by design.

## Files

- `tools/make-golden-fixtures.sh` — the driver (worktree per era,
  historical build+test, seal).
- `tools/golden-fixture-test.rs` — the historical fixture test
  (transaction-path eras).
- `tools/golden-fixture-test-epoch.rs` — the historical fixture test
  (v0.6.3 epoch-path era; a runtime `if` cannot span eras because
  pre-epoch crates have no epoch API).
- `src/tests/golden_store.rs` — the compatibility court (the CI gate).
- `manifest.txt` per era — era metadata, logical fixture manifest
  (name/size/BLAKE3), creator revision, immutable store-dir hash.
