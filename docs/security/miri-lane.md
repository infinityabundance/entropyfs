# The Miri lane (Phase 12E.18)

Driver: `tools/court-miri.sh`. Evidence:
`evidence/security/miri-lane-*`. Sealed run: the 9-test bounded subset
passed under `cargo +nightly miri` (Miri 0.1.0, nightly 2026-07-24),
~39 minutes.

## Purpose

Miri is a Rust undefined-behavior detector: it interprets the code and
its dependencies' `unsafe`, flagging UB that the compiler will not. The
hostile-media court (`src/tests/hostile_media/`) establishes the
SEMANTIC guarantees (no panic, no OOM, bounded work on adversarial
persistent bytes); this lane adds the UB check over the same machinery.
The 11A guarantees and the `unsafe_files_match_ledger` invariant remain
mandatory and unchanged.

## What this lane covers (exactly)

Filesystem-free, deterministic tests over the safe persistent-data
machinery, with the `--no-default-features` build (no fuse/ublk/uring in
the Miri tree):

| Test | Machinery exercised |
| --- | --- |
| `descriptor_court::truncation_at_every_boundary_of_every_seed` | descriptor decode at every truncation boundary |
| `descriptor_court::descriptor_cap_boundary` | descriptor capacity-bound enforcement |
| `descriptor_court::descriptor_exhibits_pass` | the hostile descriptor exhibit corpus |
| `descriptor_court::exhibits_never_panic` | no-panic property on the exhibit corpus |
| `descriptor_court::seeds_are_canonical_and_valid` | seed canonicality |
| `descriptor_court::seeds_bounded_under_tight_limits` | resource bounds under tight limits |
| `graph_court::graph_seeds_materialize_to_pinned_content` | materialization + residual application on hostile graphs |
| `graph_court::graph_seeds_bounded_under_tight_limits` | bounded hostile graph cases |
| `graph_court::graph_exhibits_pass` | the hostile graph exhibit corpus |

## What this lane does NOT cover (explicitly)

- The proptest `*_oracle` tests (256 generated cases each — prohibitively
  slow under the interpreter; they run in the native suite).
- The store courts (real file I/O; Miri's filesystem shim is not the
  target — the store is covered by the native crash/hostile/parity
  courts).
- The FUSE/ublk frontends and the io_uring transport (kernel/device
  surfaces; covered by their own courts).
- The C ABI boundary (`src/ffi`) — covered by the C smoke test and the
  Rust FFI court (its unsafe is ledger-reviewed and precondition-tested,
  not Miri-run).

**This lane does NOT claim "Miri verifies EntropyFS".** It claims exactly
the bounded subset above: UB-freedom of the persistent-data parsers
under the hostile corpus, to the extent the interpreter exercises them.
