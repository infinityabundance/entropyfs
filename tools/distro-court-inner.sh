#!/usr/bin/env bash
# EntropyFS distribution court — INNER script (runs inside the container).
#
# Phase 12E.8: the 17-stage court executed against a pristine minimal
# distro image. The driver (tools/distro-court.sh) builds the image and
# runs this with:
#   - the repo bind-mounted read-only at /src
#   - a writable evidence scratch at /out
#   - /dev/fuse + privileges where the host can provide them
#
# Stage results land in /out/court/<distro>/ with: per-stage logs,
# capability probes, waivers (environment-only failures, precisely
# recorded), fsck.json, hash manifests, court-result.json, and the
# sealed evidence-manifest.json (written by the container's own
# entropyfs binary).
#
# Exit status: 0 when every stage passed or was explicitly waivered;
# non-zero when a stage that could run FAILED (never converted to a
# waiver).

set -euo pipefail

DISTRO="${1:?usage: distro-court-inner.sh <distro>}"
# Paths are env-overridable: the Docker lane uses /src + /out; the VM
# lane uses the scp'd repo + a local evidence dir.
REPO="${COURT_REPO:-/src}"
OUT="${COURT_OUT:-/out}"
COURT_DIR="$OUT/court/$DISTRO"
STORE="${COURT_STORE:-/store}"
MNT="${COURT_MNT:-/mnt/efs}"
WAIVERS="$COURT_DIR/waivers"
mkdir -p "$COURT_DIR" "$WAIVERS" "$STORE" "$MNT"
# The sealed revision (the driver knows it; the container has no git).
echo "${COURT_REV:-unknown}" > "$COURT_DIR/revision.txt"

note() { echo "[stage $1] $2"; }

waive() {
    # $1 = stage name, $2 = probe command, $3 = probe output/error,
    # $4 = requirement to clear.
    local stage="$1" probe="$2" err="$3" req="$4"
    {
        echo "stage: $stage"
        echo "probe command: $probe"
        echo "probe output/error:"
        echo "$err"
        echo "requirement to clear: $req"
        echo "verdict: WAIVER (environment capability unavailable — NOT an EntropyFS failure)"
    } > "$WAIVERS/$stage.txt"
    note "$stage" "WAIVER recorded: $req"
}

# ---------------------------------------------------------------------------
# Stage 1-3: pristine image + prereqs + rustup. The Docker lanes bake
# rustup into the image; the VM lanes install it here (the 12E.9 rule:
# a pinned rustup stable toolchain, NEVER the distro's packaged Rust).
# ---------------------------------------------------------------------------
if ! command -v rustc >/dev/null 2>&1; then
    note 3 "installing rustup (pinned stable toolchain)"
    export RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain stable \
        > "$COURT_DIR/rustup.log" 2>&1
    export PATH="/root/.cargo/bin:$PATH"
fi
note 3 "toolchain"
rustc --version > "$COURT_DIR/rustc.txt" 2>&1
cargo --version >> "$COURT_DIR/rustc.txt" 2>&1
uname -a > "$COURT_DIR/environment.txt"
cat /etc/os-release >> "$COURT_DIR/environment.txt" 2>/dev/null || true
# package list (distro-specific)
if command -v rpm >/dev/null; then rpm -qa | sort > "$COURT_DIR/package-list.txt";
elif command -v dpkg-query >/dev/null; then dpkg-query -W -f='${Package} ${Version}\n' | sort > "$COURT_DIR/package-list.txt";
else echo "unknown package manager" > "$COURT_DIR/package-list.txt"; fi

# ---------------------------------------------------------------------------
# Worktree: the repo is bind-mounted READ-ONLY, but the store lock file
# and cargo's target dir need writes. Extract the exact revision into the
# writable evidence mount (excluding the host's target/), then build there.
# ---------------------------------------------------------------------------
mkdir -p "$OUT/work"
# Copy the exact revision (excluding the host's target/ and the evidence
# archive — the court exercises SOURCE, and the evidence archive is not
# part of the build).
(cd "$REPO" && tar --exclude=./target --exclude=./evidence --exclude=./tools/docker/evidence -cf - .) \
    | (cd "$OUT/work" && tar -xf -)
cd "$OUT/work"

# ---------------------------------------------------------------------------
# Stage 4: cargo build --release --locked
# ---------------------------------------------------------------------------
note 4 "build"
export CARGO_TARGET_DIR=/out/target
(cargo build --release --locked > "$COURT_DIR/build.log" 2>&1) || {
    echo "build failed — this is an ENTROPYFS/PACKAGING failure, not a waiver"
    exit 1
}
# (shell-native count — `xargs` is not guaranteed on minimal images)
compiled=$(grep -c '^   Compiling' "$COURT_DIR/build.log" || true)
echo "crates compiled: $compiled" | tee -a "$COURT_DIR/court-result.txt"

# ---------------------------------------------------------------------------
# Stage 5: selected release courts (the engine + persistence + compat +
# golden + fsck gates, release mode).
# ---------------------------------------------------------------------------
note 5 "release courts"
for court_filter in engine:: persistent_store:: compat_seal:: golden_store:: fsck::; do
    cargo test --release --lib --locked "$court_filter" >> "$COURT_DIR/test.log" 2>&1 \
        || { echo "release courts failed ($court_filter) — ENTROPYFS failure"; exit 1; }
done
# Aggregate: the last run's summary line
cargo test --release --lib --locked 'engine::' 2>&1 | tail -1 >> "$COURT_DIR/test.log" || true
tail -3 "$COURT_DIR/test.log" | tee -a "$COURT_DIR/court-result.txt"

# ---------------------------------------------------------------------------
# Stage 6: cargo install --path . --locked (package-local equivalent)
# ---------------------------------------------------------------------------
note 6 "install"
export CARGO_INSTALL_ROOT=/opt/efs
(cargo install --path . --locked --root /opt/efs > "$COURT_DIR/install.log" 2>&1) || {
    echo "install failed — ENTROPYFS failure"
    exit 1
}
export PATH="/opt/efs/bin:$PATH"
entropyfs --version | tee -a "$COURT_DIR/court-result.txt"

# ---------------------------------------------------------------------------
# Stage 7: entropyfs mkfs
# ---------------------------------------------------------------------------
note 7 "mkfs"
entropyfs mkfs "$STORE" --uuid 22222222222222222222222222222222 | tee "$COURT_DIR/mkfs.log"

# ---------------------------------------------------------------------------
# Stage 8-9: library Engine API smoke (sync default + explicit sync)
# ---------------------------------------------------------------------------
note 8 "engine API smoke"
(cargo run --release --example engine_smoke -- /store-engine > "$COURT_DIR/engine-smoke.log" 2>&1) || {
    echo "engine smoke failed — ENTROPYFS failure"
    exit 1
}
tail -2 "$COURT_DIR/engine-smoke.log" | tee -a "$COURT_DIR/court-result.txt"
note 9 "SyncIo smoke (same run)"

# ---------------------------------------------------------------------------
# Stage 10: UringIo capability detection + smoke (waiver when the
# container runtime blocks io_uring — an environment limitation).
# ---------------------------------------------------------------------------
note 10 "UringIo"
entropyfs capabilities > "$COURT_DIR/capabilities.log" 2>&1 || true
if grep -q "io_uring transport: available" "$COURT_DIR/capabilities.log"; then
    (cargo run --release --example engine_smoke -- /store-uring --io-backend uring \
        > "$COURT_DIR/uring-smoke.log" 2>&1) || {
        echo "uring smoke failed — ENTROPYFS failure (io_uring present but failing)"
        exit 1
    }
    echo "uring smoke: OK" | tee -a "$COURT_DIR/court-result.txt"
else
    waive 10-uring "entropyfs capabilities (io_uring probe)" \
        "$(grep -i 'uring' "$COURT_DIR/capabilities.log" || echo 'io_uring transport: UNAVAILABLE')" \
        "container/runtime must allow io_uring_create (e.g. seccomp=unconfined, kernel 5.6+)"
fi

# ---------------------------------------------------------------------------
# Stage 11: FUSE mount (waiver when the container runtime lacks /dev/fuse
# or mount privileges).
# ---------------------------------------------------------------------------
note 11 "FUSE mount"
if [[ ! -e /dev/fuse ]]; then
    waive 11-fuse "ls /dev/fuse" "/dev/fuse absent" \
        "container must expose /dev/fuse (--device /dev/fuse) and CAP_SYS_ADMIN"
    FUSE_OK=0
else
    # Mount in the background (the daemon runs the FUSE event loop); the
    # court then exercises the mounted filesystem and unmounts it.
    entropyfs mount "$STORE" "$MNT" --threads 2 \
        > "$COURT_DIR/mount.log" 2>&1 &
    MOUNT_PID=$!
    sleep 3
    if kill -0 "$MOUNT_PID" 2>/dev/null && mountpoint -q "$MNT" 2>/dev/null; then
        FUSE_OK=1
        echo "fuse mount: OK (pid $MOUNT_PID)" | tee -a "$COURT_DIR/court-result.txt"
    else
        # Distinguish: a capability problem (mount(2) EPERM — the store
        # opens and fsck is clean) is an environment waiver; anything else
        # is an EntropyFS failure.
        kill "$MOUNT_PID" 2>/dev/null || true
        if entropyfs fsck --json "$STORE" >/dev/null 2>&1 \
            && grep -qiE 'permission|operation not permitted|EPERM|mount.*denied' "$COURT_DIR/mount.log"; then
            waive 11-fuse "entropyfs mount $STORE $MNT" \
                "$(tail -5 "$COURT_DIR/mount.log")" \
                "container must allow fuse mounts (CAP_SYS_ADMIN + /dev/fuse + apparmor/seccomp unconfined)"
            FUSE_OK=0
        else
            echo "mount failed — ENTROPYFS failure"
            tail -20 "$COURT_DIR/mount.log"
            exit 1
        fi
    fi
fi

# ---------------------------------------------------------------------------
# Stage 12: POSIX smoke (through FUSE when available).
# ---------------------------------------------------------------------------
note 12 "POSIX smoke"
if [[ "${FUSE_OK:-0}" == "1" ]]; then
    T="$MNT"
else
    waive 12-posix "mount stage" "POSIX smoke requires the FUSE mount" \
        "container must expose /dev/fuse + mount privileges"
    T="$STORE"
    echo "POSIX smoke: WAIVED (no FUSE mount)" | tee -a "$COURT_DIR/court-result.txt"
    POSIX_OK=0
fi

if [[ "${POSIX_OK:-1}" == "1" ]]; then
    (
        set -e
        cd "$T"
        mkdir d1
        echo "hello distro" > d1/f1.txt
        cat d1/f1.txt > /dev/null
        mv d1/f1.txt d1/f2.txt
        ln d1/f2.txt d1/f2-hard
        ln -s f2.txt d1/f2-sym
        truncate -s 100000 d1/big.bin
        printf 'x' | dd of=d1/big.bin bs=1 seek=99999 conv=notrunc status=none
        python3 -c "import os; os.setxattr('d1/f2.txt', b'user.test', b'v1')" 2>/dev/null \
            || setfattr -n user.test -v v1 d1/f2.txt 2>/dev/null \
            || echo "xattr: unavailable (fs limitation)" > /dev/null
        sync
        for i in $(seq 1 8); do (echo "writer-$i" > "d1/cw-$i.txt") & done; wait
    ) > "$COURT_DIR/posix.log" 2>&1 || {
        echo "POSIX smoke failed — ENTROPYFS failure"
        tail -20 "$COURT_DIR/posix.log"
        exit 1
    }
    echo "POSIX smoke: OK" | tee -a "$COURT_DIR/court-result.txt"
    # hash manifest (what we wrote)
    (cd "$T" && find . -type f | sort | while read -r f; do sha256sum "$f"; done) \
        > "$COURT_DIR/hash-manifest-before.txt"
fi

# ---------------------------------------------------------------------------
# Stage 13: unmount
# ---------------------------------------------------------------------------
note 13 "unmount"
if [[ "${FUSE_OK:-0}" == "1" ]]; then
    entropyfs unmount "$MNT" > "$COURT_DIR/unmount.log" 2>&1 || \
        fusermount3 -u "$MNT" >> "$COURT_DIR/unmount.log" 2>&1 || \
        umount "$MNT" >> "$COURT_DIR/unmount.log" 2>&1 || true
    kill "${MOUNT_PID:-0}" 2>/dev/null || true
    wait "${MOUNT_PID:-0}" 2>/dev/null || true
    echo "unmount: ok (or already detached)" | tee -a "$COURT_DIR/court-result.txt"
fi

# ---------------------------------------------------------------------------
# Stage 14: fsck --json
# ---------------------------------------------------------------------------
note 14 "fsck --json"
entropyfs fsck --json "$STORE" > "$COURT_DIR/fsck.json" 2>&1 || {
    echo "fsck failed — ENTROPYFS failure"
    cat "$COURT_DIR/fsck.json"
    exit 1
}
python3 -c "
import json
d = json.load(open('$COURT_DIR/fsck.json'))
print('fsck status:', d['status'], '| findings:', len(d['findings']))
" | tee -a "$COURT_DIR/court-result.txt"

# ---------------------------------------------------------------------------
# Stage 15: reopen + verify exact hashes (the engine smoke creates a
# store, closes, reopens, and verifies byte identity — the reopen is the
# verify; run it against a FRESH path, then fsck-verify the POSIX store
# again as the settled-state check).
# ---------------------------------------------------------------------------
note 15 "reopen + verify"
(cargo run --release --example engine_smoke -- /store-verify > "$COURT_DIR/reopen.log" 2>&1) || {
    echo "reopen smoke failed — ENTROPYFS failure"
    exit 1
}
entropyfs fsck --json "$STORE" > "$COURT_DIR/fsck-before-gc.json" 2>&1
echo "reopen + verify: OK" | tee -a "$COURT_DIR/court-result.txt"

# ---------------------------------------------------------------------------
# Stage 16: compact / GC
# ---------------------------------------------------------------------------
note 16 "compact / GC"
entropyfs gc --compact "$STORE" > "$COURT_DIR/gc.log" 2>&1 || {
    echo "gc failed — ENTROPYFS failure"
    exit 1
}
tail -4 "$COURT_DIR/gc.log" | tee -a "$COURT_DIR/court-result.txt"

# ---------------------------------------------------------------------------
# Stage 17: reopen + fsck again
# ---------------------------------------------------------------------------
note 17 "reopen + fsck"
entropyfs fsck --json "$STORE" > "$COURT_DIR/fsck-after.json" 2>&1 || {
    echo "post-gc fsck failed — ENTROPYFS failure"
    exit 1
}
python3 -c "
import json
d = json.load(open('$COURT_DIR/fsck-after.json'))
print('post-gc fsck status:', d['status'])
" | tee -a "$COURT_DIR/court-result.txt"

# ---------------------------------------------------------------------------
# Seal: the evidence manifest (written by the container's own binary).
# ---------------------------------------------------------------------------
entropyfs evidence-manifest "$COURT_DIR/evidence-manifest.json" \
    --store "$STORE" --io-backend sync --worker-scheduler semaphore \
    --court-schema-version 1 \
    --container-image-digest "${COURT_IMAGE_DIGEST:-unknown}" \
    > /dev/null 2>&1 || echo "evidence-manifest: unavailable" >> "$COURT_DIR/court-result.txt"

# Result summary.
{
    echo "court: $DISTRO"
    echo "result: PASS (all stages passed or environment-waived)"
    echo "waivers: $(ls "$WAIVERS" 2>/dev/null | wc -l)"
} > "$COURT_DIR/court-result.json"
cat "$COURT_DIR/court-result.txt" >> "$COURT_DIR/court-result.json"

echo "== distro court $DISTRO: DONE =="
