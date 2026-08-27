#!/usr/bin/env bash
# Phase 12E.15 — the Go binding build+test driver.
#
# # PURPOSE
#
# Builds the cdylib, then runs the Go binding's courts with the correct
# link environment: `go vet`, `go test` (correctness + hostile-input),
# `go test -race` (the mandatory race gate), and the FFI-overhead
# benchmarks. The native dependency story is explicit: the binding links
# `libentropyfs.so` through the stable C ABI and nothing else.
#
# # BOUNDARY
#
# KNOWS: how to build the cdylib and invoke the Go toolchain. NEVER
# KNOWS: the store or engine internals. Requires: cargo, cc, go >= 1.24.
#
# # USAGE
#
#     tools/go-test.sh [fast]        # tests + race; benchmarks only with 'bench'
#     tools/go-test.sh bench         # also run the FFI-overhead benchmarks
#
# Exits nonzero on any failure.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
GO_DIR="$REPO_ROOT/go"
MODE="${1:-test}"

echo "== phase-12E.15 Go binding court =="

(cd "$REPO_ROOT" && cargo build --release --locked)

SO="$REPO_ROOT/target/release/libentropyfs.so"
[[ -f "$SO" ]] || { echo "error: $SO missing (cdylib not built)" >&2; exit 1; }

export CGO_LDFLAGS="-L$REPO_ROOT/target/release -lentropyfs"
export LD_LIBRARY_PATH="$REPO_ROOT/target/release${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

echo "-- go vet --"
(cd "$GO_DIR" && go vet ./...)

echo "-- go test (correctness + hostile-input) --"
(cd "$GO_DIR" && go test -count=1 ./...)

echo "-- go test -race (mandatory binding gate) --"
(cd "$GO_DIR" && go test -race -count=1 ./...)

if [[ "$MODE" == "bench" ]]; then
    echo "-- FFI-overhead benchmarks --"
    (cd "$GO_DIR" && go test -run '^$' -bench . -benchmem ./entropyfs)
fi

echo "== Go binding court PASS =="
