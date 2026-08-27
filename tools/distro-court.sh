#!/usr/bin/env bash
# EntropyFS distribution court — DRIVER (Phase 12E.8).
#
# Runs the 17-stage portability court as DOCKER containers against the
# mandatory enterprise matrix, using the DOCKER HUB vendor images
# (preferred per the operator):
#
#   almalinux-10.2-minimal   almalinux:10.2
#   ubuntu-26.04-minimal     ubuntu:26.04
#   leap-16.0-minimal        opensuse/leap:16.0  (the SUSE-family lane,
#                            the vendor's legally distributable base)
#
# The images extracted from the provided vendor OS artifacts by
# tools/docker/build-distro-images.sh remain available as the offline /
# vendor-artifact lane (documented alternative).
#
# Usage:
#   tools/distro-court.sh [almalinux-10.2-minimal|ubuntu-26.04-minimal|leap-16.0-minimal|all]
#
# Evidence: every run archives under evidence/portability/
# distro-court-<distro>-<timestamp>-<revision>/ with the immutable
# base-image digest, per-stage logs, capability probes, waivers, fsck
# JSON, hash manifests, court-result.json, and the sealed
# evidence-manifest.json.
#
# A distribution does not pass merely because Rust compiles: the gate is
# the full stage list (build, release courts, install, mkfs, engine
# smoke, SyncIo, UringIo where the runtime permits, FUSE mount when
# /dev/fuse + privileges exist, POSIX smoke, unmount, fsck --json,
# reopen hash verification, compact, reopen+fsck). Environment
# capability limitations become explicit waivers (never EntropyFS
# failures); anything that CAN run must PASS.

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

COURT="${1:-all}"
REV="$(git rev-parse --short HEAD 2>/dev/null || echo norev)"
EVIDENCE_ROOT="evidence/portability"

# OOM limiter (the operator's hard requirement): every court container runs
# under a hard memory cap so a pathological build/court cannot exhaust the
# host (docker's storage here is a 26 GiB RAM tmpfs — an unconstrained
# process could OOM the machine). Env-overridable:
#   COURT_MEM         memory limit (default 4g)
#   COURT_MEM_SWAP    memory+swap limit (default 6g)
COURT_MEM="${COURT_MEM:-4g}"
COURT_MEM_SWAP="${COURT_MEM_SWAP:-6g}"

mkdir -p "$EVIDENCE_ROOT"

run_lane() {
    local lane="$1" image="$2" dockerfile="$3"
    local ts out_dir
    ts="$(date +%s)"
    out_dir="$REPO_ROOT/$EVIDENCE_ROOT/distro-court-$lane-$ts-$REV"
    mkdir -p "$out_dir"
    echo "== lane $lane ($image) =="

    # Pull the immutable Docker Hub image (mutable tags are insufficient
    # evidence; the resolved digest is recorded).
    docker pull "$image" > "$out_dir/pull.log" 2>&1 || {
        echo "image pull failed: $image"
        return 1
    }
    local digest
    digest="$(docker inspect --format '{{index .RepoDigests 0}}' "$image" 2>/dev/null || docker inspect --format '{{.Id}}' "$image")"
    echo "$digest" > "$out_dir/base-image-digest.txt"
    echo "docker-hub:$image" > "$out_dir/base-image-source.txt"
    echo "base image digest: $digest"

    # Build the court image (prereqs + rustup only; repo bind-mounted).
    docker build -q -t "entropyfs-court-image-$lane" -f "$dockerfile" . \
        > "$out_dir/court-image-build.log" 2>&1 || {
        echo "court image build failed — see $out_dir/court-image-build.log"
        tail -20 "$out_dir/court-image-build.log"
        return 1
    }

    # Run the 17-stage court. Privileged + /dev/fuse: the capability
    # probes (FUSE mount, io_uring) need them; the court records waivers
    # where the RUNTIME still cannot provide them. Hard memory cap
    # (OOM limiter): a container that exceeds the cap is killed by the
    # kernel, never allowed to exhaust the host.
    docker run --rm \
        --privileged \
        --device /dev/fuse \
        --memory "$COURT_MEM" \
        --memory-swap "$COURT_MEM_SWAP" \
        --oom-kill-disable=false \
        -e "COURT_IMAGE_DIGEST=$digest" \
        -e "COURT_REV=$REV" \
        -v "$REPO_ROOT:/src:ro" \
        -v "$out_dir:/out:rw" \
        "entropyfs-court-image-$lane" \
        bash /src/tools/distro-court-inner.sh "$lane" \
        > "$out_dir/court-driver.log" 2>&1 || {
        echo "lane $lane: FAILED (a runnable stage failed — not a waiver)"
        tail -30 "$out_dir/court-driver.log"
        return 1
    }
    # Court containers run under a hard OOM cap (the user's requirement):
    # a pathological build must be killed, never allowed to exhaust the
    # host (docker storage is a 26 GiB RAM tmpfs here).
    echo "oom limiter: memory=$COURT_MEM swap=$COURT_MEM_SWAP" > "$out_dir/oom-limiter.txt"
    # The container has no git; record the sealed revision + patch it into
    # the evidence manifest (the manifest's own git capture is best-effort).
    echo "$REV" > "$out_dir/revision.txt"
    python3 - "$out_dir" "$REV" <<'EOF'
import json, re, sys
out, rev = sys.argv[1], sys.argv[2]
name = out.rsplit("/", 1)[-1]
lane = re.sub(r"-\d+-[0-9a-f]+$", "", name.split("distro-court-", 1)[1])
p = f"{out}/court/{lane}/evidence-manifest.json"
try:
    m = json.load(open(p))
    m["git_revision"] = rev
    json.dump(m, open(p, "w"), indent=2)
except Exception:
    pass
EOF
    docker image rm -f "entropyfs-court-image-$lane" > /dev/null 2>&1 || true
    # The court's scratch (worktree + cargo target) lives on the /out
    # mount and is NOT evidence. The inner script's trap removes it as
    # root; this is the safety net for runs that died before the trap
    # (best-effort — root-owned leftovers require the container lane).
    rm -rf "$out_dir/work" "$out_dir/target" 2>/dev/null || true
    echo "lane $lane: sealed at $out_dir"
}

case "$COURT" in
almalinux-10.2-minimal)
    run_lane almalinux-10.2-minimal almalinux:10.2 \
        tools/docker/Dockerfile.distro-almalinux
    ;;
ubuntu-26.04-minimal)
    run_lane ubuntu-26.04-minimal ubuntu:26.04 \
        tools/docker/Dockerfile.distro-ubuntu
    ;;
leap-16.0-minimal)
    run_lane leap-16.0-minimal opensuse/leap:16.0 \
        tools/docker/Dockerfile.distro-leap
    ;;
all)
    bash "$0" almalinux-10.2-minimal
    bash "$0" ubuntu-26.04-minimal
    bash "$0" leap-16.0-minimal
    ;;
*)
    echo "unknown lane: $COURT (almalinux-10.2-minimal|ubuntu-26.04-minimal|leap-16.0-minimal|all)" >&2
    exit 1
    ;;
esac
echo "distro court: done"
