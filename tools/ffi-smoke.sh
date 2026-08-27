#!/usr/bin/env bash
# Phase 12E.14 — the C ABI smoke test (driver).
#
# # PURPOSE
#
# Compile the C smoke program (`tools/ffi-smoke/smoke.c`) against
# `include/entropyfs.h` and link it against the crate's cdylib
# (`libentropyfs.so`), then run it. This is the C-side proof of the
# opaque-handle facade: the smoke exercises the ABI version query, the
# create/open lifecycle, put/get/range byte-exactness, dedup identity,
# contains, sync, compact, metrics JSON, the classified error path, the
# last-error detail, and the free contract.
#
# # BOUNDARY
#
# KNOWS: how to build the cdylib and run the smoke. NEVER KNOWS: the
# store or engine internals. Requires: cc, cargo.
#
# # USAGE
#
#     tools/ffi-smoke.sh
#
# Exits nonzero on any smoke failure.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "== phase-12E.14 C ABI smoke =="

(cd "$REPO_ROOT" && cargo build --release --locked)

SO="$REPO_ROOT/target/release/libentropyfs.so"
[[ -f "$SO" ]] || { echo "error: $SO not built (cdylib missing)" >&2; exit 1; }

echo "-- exported symbols --"
nm -D --defined-only "$SO" | grep " T entropyfs_" | awk '{print $3}' | sort

echo "-- compile --"
cc -std=c11 -Wall -Wextra -O2 -I "$REPO_ROOT/include" \
    "$SCRIPT_DIR/ffi-smoke/smoke.c" -L "$REPO_ROOT/target/release" -lentropyfs \
    -o "$SCRIPT_DIR/ffi-smoke/smoke"
echo "-- run --"
LD_LIBRARY_PATH="$REPO_ROOT/target/release" "$SCRIPT_DIR/ffi-smoke/smoke"
