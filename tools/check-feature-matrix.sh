#!/usr/bin/env bash
# Phase 12E.2: feature-matrix compile gate.
#
# Every supported feature combination must compile (and the base library
# must build with NO frontend and the reference SyncIo transport only).
# No feature combination may silently change on-disk semantics — features
# gate CODE (frontends + transports), never the persistent format.
#
# Combinations (the 12E.2 minimum set):
#   default            the full daemon (fuse + ublk + uring)
#   no-fuse            default minus fuse
#   no-ublk            default minus ublk
#   no-uring           default minus uring (SyncIo only)
#   base               --no-default-features (the embeddable engine)
#   all-features       explicit union (== default today)
#
# Usage: tools/check-feature-matrix.sh [--check | --test-lib]

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:---check}"

run() {
    local label="$1"; shift
    echo "== $label =="
    if [[ "$MODE" == "--check" ]]; then
        cargo check --quiet --all-targets "$@"
    else
        cargo test --quiet --lib --no-run "$@"
    fi
}

run "default (fuse+ublk+uring)" 
run "no-fuse" --no-default-features --features ublk,uring
run "no-ublk" --no-default-features --features fuse,uring
run "no-uring (SyncIo only)" --no-default-features --features fuse,ublk
run "base (no default features)" --no-default-features
run "all-features (explicit)" --no-default-features --features fuse,ublk,uring

echo "feature matrix: ALL COMBINATIONS OK"
