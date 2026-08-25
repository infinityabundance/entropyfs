#!/usr/bin/env bash
# Run the competitive filesystem court in a disposable root-capable docker
# container (Phase 8H/9A): loop-mounted XFS, Btrfs raw + zstd:1, EROFS,
# SquashFS, FUSE EntropyFS, and standalone zstd — all under the same
# corpus and the symmetric buffered/durable/warm/cold measurement rules.
#
# Requires: docker (privileged + /dev/loop-control access), a release
# build of the entropyfs binary (cargo build --release), and
# tools/docker/Dockerfile.court.
#
# Usage: tools/run-court-docker.sh [WORKDIR] [OUTDIR]
#   WORKDIR  container scratch (default /scratch)
#   OUTDIR   evidence root (default <repo>/evidence/performance; must be
#            bind-mount writable)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKDIR="${1:-/scratch}"
OUTROOT="${2:-$REPO_ROOT/evidence/performance}"

if [[ ! -x "$REPO_ROOT/target/release/entropyfs" ]]; then
    echo "error: target/release/entropyfs missing (cargo build --release)" >&2
    exit 1
fi

mkdir -p "$REPO_ROOT/evidence/performance"

echo "== building court image =="
docker build -q -t entropyfs-court -f "$REPO_ROOT/tools/docker/Dockerfile.court" . >/dev/null

REV="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || true)"
echo "== running court (revision ${REV:-norev}) in privileged container =="
exec docker run --rm --privileged --device /dev/loop-control \
    -e "COURT_REV=$REV" \
    -e "COURT_FUSE_THREADS=${COURT_FUSE_THREADS:-1}" \
    -e "COURT_FOREGROUND=${COURT_FOREGROUND:-full}" \
    -v "$REPO_ROOT:/repo:ro" \
    -v "$OUTROOT:/repo/evidence/performance:rw" \
    entropyfs-court "$WORKDIR" /repo/evidence/performance
