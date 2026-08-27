#!/usr/bin/env bash
# Phase 12E.18 — the Miri lane (driver).
#
# # PURPOSE
#
# Runs the bounded Miri subset over the safe persistent-data machinery:
# descriptor decode, representation validation, materialization, residual
# application, and the bounded hostile graph cases — the exact list the
# 12E.18 brief names. Miri is a UB detector for the whole stack (this
# crate's safe code + its dependencies' unsafe), complementing the
# hostile-media court's semantic checks with undefined-behavior checks.
#
# # COVERAGE — exactly what this lane does and does NOT verify
#
# COVERED (deterministic, filesystem-free tests):
#   - descriptor_court: truncation at every boundary of every seed,
#     the descriptor capacity boundary, the hostile exhibits pass, the
#     never-panic property, seed canonicality and tight-limit bounds;
#   - graph_court: hostile graph seeds materializing to pinned content,
#     tight-limit bounds, the exhibits pass.
# NOT COVERED (documented boundaries):
#   - the proptest *oracle tests (256 cases each — prohibitively slow
#     under Miri; they run in the normal suite instead);
#   - the store courts (real file I/O — Miri's fs shim is not the
#     target; the store is covered by the native crash/hostile courts);
#   - the FUSE/ublk/io_uring frontends (kernel/device surfaces);
#   - the FFI boundary (src/ffi — covered by the C smoke + Rust FFI
#     court instead).
# This lane therefore does NOT claim "Miri verifies EntropyFS"; it
# claims exactly the bounded subset above.
#
# # USAGE
#
#     tools/court-miri.sh [OUTROOT]
#
# Requires: a nightly toolchain with Miri (rustup component add miri).
# Runtime: ~40 minutes (the Miri interpreter is ~100x slower than
# native). Archives under evidence/security/miri-lane-<ts>-<rev>/.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTROOT="${1:-$REPO_ROOT/evidence/security}"
BUILD_DIR="${COURT_WORKTREE:-$REPO_ROOT}"

TS="$(date +%s)"
REV="$(git -C "$BUILD_DIR" rev-parse --short HEAD)"
OUT="$OUTROOT/miri-lane-$TS-$REV"
mkdir -p "$OUT"

echo "== phase-12E.18 Miri lane: rev=$REV (bounded deterministic subset) =="
echo "runtime: ~40 min (Miri interpreter)" | tee "$OUT/runtime-note.txt"

(cd "$BUILD_DIR" && cargo +nightly miri test --no-default-features --lib -- \
    hostile_media::descriptor_court::descriptor_cap_boundary \
    hostile_media::descriptor_court::descriptor_exhibits_pass \
    hostile_media::descriptor_court::exhibits_never_panic \
    hostile_media::descriptor_court::seeds_are_canonical_and_valid \
    hostile_media::descriptor_court::seeds_bounded_under_tight_limits \
    hostile_media::descriptor_court::truncation_at_every_boundary_of_every_seed \
    hostile_media::graph_court::graph_exhibits_pass \
    hostile_media::graph_court::graph_seeds_bounded_under_tight_limits \
    hostile_media::graph_court::graph_seeds_materialize_to_pinned_content \
    > "$OUT/run.log" 2>&1) || {
    echo "error: Miri lane FAILED (UB or test failure); evidence at $OUT/run.log" >&2
    tail -30 "$OUT/run.log"
    exit 1
}

grep -E "^test |test result" "$OUT/run.log" | tee "$OUT/results.txt"
grep "test result: ok" "$OUT/run.log" >/dev/null || {
    echo "error: Miri lane did not pass" >&2
    exit 1
}

BIN="$REPO_ROOT/target/release/entropyfs"
if [[ -x "$BIN" ]]; then
    "$BIN" evidence-manifest "$OUT/evidence-manifest.json" \
        --store "$OUT" --io-backend sync --worker-scheduler pool \
        --court-schema-version 2 >/dev/null 2>&1 \
        && echo "manifest: $OUT/evidence-manifest.json" || echo "manifest skipped"
fi

echo "== Miri lane PASS: $OUT =="
