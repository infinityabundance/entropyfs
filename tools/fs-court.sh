#!/usr/bin/env bash
# Competitive filesystem court (Phase 8H, methodology §3/§41, spec §42).
#
# Measures storage footprint and throughput for the same corpora across:
#   - ext4            (host directory — no privileges needed)
#   - XFS             (loop image; requires root + loop devices)
#   - Btrfs, no comp  (loop image; requires root + loop devices)
#   - Btrfs + zstd:1  (loop image; requires root + loop devices)
#   - SquashFS        (mksquashfs; image creation is unprivileged)
#   - EROFS           (mkfs.erofs; image creation is unprivileged)
#   - zstd standalone (zstd -1 / -3 / -19 on each corpus)
#   - EntropyFS       (mkfs + FUSE mount via fusermount3 — unprivileged)
#
# The court writes an evidence archive under
# evidence/performance/fs-court-<ts>-<rev>/ with results.json, report.md,
# raw-output.txt and environment.json, so the admission rules (§8) apply.
# Loop-mount filesystems that cannot run in this environment are recorded
# as EXPLICIT WAIVERS with the exact command a root-capable VM must run —
# the methodology permits waivers, but the goal (Phase 8H) is to remove
# them by running this script in a disposable root-capable VM.
#
# Usage: fs-court.sh [WORKDIR] [OUTDIR]
#   WORKDIR  scratch (default: <repo>/target/fs-court-scratch)
#   OUTDIR   evidence root (default: <repo>/evidence/performance)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENTROPYFS_BIN="${ENTROPYFS_BIN:-$REPO_ROOT/target/release/entropyfs}"
WORKDIR="${1:-$REPO_ROOT/target/fs-court-scratch}"
OUTROOT="${2:-$REPO_ROOT/evidence/performance}"

if [[ ! -x "$ENTROPYFS_BIN" ]]; then
    echo "error: $ENTROPYFS_BIN not found (build with: cargo build --release)" >&2
    exit 1
fi

REV="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || true)"
[[ -n "$REV" ]] || REV="norev"
TS="$(date +%s)"
OUT="$OUTROOT/fs-court-$TS-$REV"
mkdir -p "$OUT" "$WORKDIR/corpora"

LOG="$OUT/raw-output.txt"
: > "$LOG"
log() { echo "$*" | tee -a "$LOG"; }

cleanup() {
    # Never leave a loop mount behind.
    for m in "$WORKDIR"/mnt-*; do
        [[ -d "$m" ]] && umount "$m" 2>/dev/null || true
    done
    for f in "$WORKDIR"/img-*; do
        losetup -d "$f" 2>/dev/null || true
    done
    rm -rf "$WORKDIR"/mnt-* "$WORKDIR"/img-* "$WORKDIR"/efs-store "$WORKDIR"/mnt-efs
}
trap cleanup EXIT

# --- corpora ---------------------------------------------------------------
log "== corpus construction =="
cp -r "$REPO_ROOT/src" "$WORKDIR/corpora/src" 2>/dev/null || true
cp "$REPO_ROOT/Cargo.toml" "$REPO_ROOT/Cargo.lock" "$WORKDIR/corpora/" 2>/dev/null || true
[[ -d "$REPO_ROOT/docs" ]] && cp -r "$REPO_ROOT/docs" "$WORKDIR/corpora/docs" 2>/dev/null || true
dd if=/dev/urandom of="$WORKDIR/corpora/random.bin" bs=1M count=64 status=none
dd if=/dev/zero of="$WORKDIR/corpora/zeros.bin" bs=1M count=64 status=none
(cd "$WORKDIR/corpora" && tar czf compressed.tgz src docs 2>/dev/null || true)

CORPORA=(src random.bin zeros.bin compressed.tgz)

bytes_of() { stat -c %s "$1" 2>/dev/null || echo 0; }

results="$OUT/results.json"
python3 - "$results" <<'EOF'
import json, sys
json.dump({"fs": {}, "zstd": {}, "entropyfs": {}}, open(sys.argv[1], "w"))
EOF

record() {  # record <section> <key> <value>
    python3 - "$results" "$1" "$2" "$3" <<'EOF'
import json, sys
r = json.load(open(sys.argv[1]))
r[sys.argv[2]][sys.argv[3]] = json.loads(sys.argv[4]) if sys.argv[4].startswith(("{","[")) else sys.argv[4]
json.dump(r, open(sys.argv[1], "w"), indent=1)
EOF
}

HAVE_ROOT=0
[[ "$(id -u)" == "0" ]] && HAVE_ROOT=1
command -v losetup >/dev/null || HAVE_ROOT=0

# --- 1. ext4 host directory -------------------------------------------------
log "== ext4 (host directory) =="
EXT4_OK=1
for c in "${CORPORA[@]}"; do
    mkdir -p "$WORKDIR/mnt-ext4"
    t0=$(date +%s%N)
    cp -r "$WORKDIR/corpora/$c" "$WORKDIR/mnt-ext4/$c"
    t1=$(date +%s%N)
    apparent=$(du -sb "$WORKDIR/mnt-ext4/$c" | cut -f1)
    allocated=$(du -sB1 "$WORKDIR/mnt-ext4/$c" | cut -f1)
    wall_ns=$((t1 - t0))
    wmbps=$(python3 -c "print(f'{$apparent/1048576/($wall_ns/1e9):.1f}')")
    log "  $c: apparent $apparent allocated $allocated write ${wmbps} MiB/s"
    record fs "ext4/$c" "{\"apparent\": $apparent, \"allocated\": $allocated, \"write_mbps\": $wmbps}"
done

# --- 2/3/4. XFS / Btrfs (±zstd) loop images ---------------------------------
run_loop_fs() {  # run_loop_fs <name> "<mkfs args>" "<mount opts>"
    local name="$1" mkfs_args="$2" mnt_opts="$3"
    log "== $name (loop image) =="
    if [[ "$HAVE_ROOT" != "1" ]]; then
        log "  WAIVER: $name requires root + loop devices; run in a root-capable VM:"
        log "    $mkfs_args <image> && mount -o loop${mnt_opts:+,$mnt_opts} <image> <mnt>"
        record fs "$name" "{\"waived\": \"requires root + loop\", \"command\": \"$mkfs_args <image> && mount -o loop${mnt_opts:+,$mnt_opts} <image> <mnt>\"}"
        return
    fi
    local img="$WORKDIR/img-$name"
    truncate -s 256M "$img"
    # shellcheck disable=SC2086
    $mkfs_args "$img" >/dev/null 2>&1 || { log "  WAIVER: $mkfs_args failed"; record fs "$name" "{\"waived\": \"mkfs failed\"}"; return; }
    local mnt="$WORKDIR/mnt-$name"
    mkdir -p "$mnt"
    if [[ -n "$mnt_opts" ]]; then
        mount -o "$mnt_opts,loop" "$img" "$mnt" || { log "  WAIVER: mount failed"; record fs "$name" "{\"waived\": \"mount failed\"}"; return; }
    else
        mount -o loop "$img" "$mnt" || { log "  WAIVER: mount failed"; record fs "$name" "{\"waived\": \"mount failed\"}"; return; }
    fi
    for c in "${CORPORA[@]}"; do
        t0=$(date +%s%N)
        cp -r "$WORKDIR/corpora/$c" "$mnt/$c"
        t1=$(date +%s%N)
        apparent=$(du -sb "$mnt/$c" | cut -f1)
        allocated=$(du -sB1 "$mnt/$c" | cut -f1)
        wall_ns=$((t1 - t0))
        wmbps=$(python3 -c "print(f'{$apparent/1048576/($wall_ns/1e9):.1f}')")
        log "  $c: apparent $apparent allocated $allocated write ${wmbps} MiB/s"
        record fs "$name/$c" "{\"apparent\": $apparent, \"allocated\": $allocated, \"write_mbps\": $wmbps}"
    done
    umount "$mnt"
}

if command -v mkfs.xfs >/dev/null; then
    run_loop_fs xfs "mkfs.xfs -f" ""
else
    log "== xfs: WAIVER (mkfs.xfs not installed) =="
    record fs "xfs" "{\"waived\": \"mkfs.xfs not installed\"}"
fi

if command -v mkfs.btrfs >/dev/null; then
    run_loop_fs btrfs "mkfs.btrfs -f" ""
    run_loop_fs btrfs-zstd "mkfs.btrfs -f" "compress=zstd:1"
else
    log "== btrfs: WAIVER (mkfs.btrfs not installed) =="
    record fs "btrfs" "{\"waived\": \"mkfs.btrfs not installed\"}"
    record fs "btrfs-zstd" "{\"waived\": \"mkfs.btrfs not installed\"}"
fi

# --- 5/6. SquashFS / EROFS images (unprivileged creation) ------------------
if command -v mksquashfs >/dev/null; then
    log "== squashfs (zstd image) =="
    for c in "${CORPORA[@]}"; do
        img="$WORKDIR/img-squash-$c.sqfs"
        mksquashfs "$WORKDIR/corpora/$c" "$img" -comp zstd >/dev/null 2>&1
        size=$(bytes_of "$img")
        apparent=$(du -sb "$WORKDIR/corpora/$c" | cut -f1)
        ratio=$(python3 -c "print(f'{$apparent/max($size,1):.3f}')")
        log "  $c: apparent $apparent image $size ratio ${ratio}x"
        record fs "squashfs-zstd/$c" "{\"apparent\": $apparent, \"image\": $size, \"ratio\": $ratio}"
    done
else
    log "== squashfs: WAIVER (mksquashfs not installed) =="
    record fs "squashfs-zstd" "{\"waived\": \"mksquashfs not installed\"}"
fi

if command -v mkfs.erofs >/dev/null; then
    log "== erofs (lz4hc image) =="
    for c in "${CORPORA[@]}"; do
        img="$WORKDIR/img-erofs-$c.erofs"
        mkfs.erofs -zlz4hc "$img" "$WORKDIR/corpora/$c" >/dev/null 2>&1
        size=$(bytes_of "$img")
        apparent=$(du -sb "$WORKDIR/corpora/$c" | cut -f1)
        ratio=$(python3 -c "print(f'{$apparent/max($size,1):.3f}')")
        log "  $c: apparent $apparent image $size ratio ${ratio}x"
        record fs "erofs-lz4hc/$c" "{\"apparent\": $apparent, \"image\": $size, \"ratio\": $ratio}"
    done
else
    log "== erofs: WAIVER (mkfs.erofs not installed) =="
    record fs "erofs-lz4hc" "{\"waived\": \"mkfs.erofs not installed\"}"
fi

# --- 7. zstd standalone ------------------------------------------------------
if command -v zstd >/dev/null; then
    log "== zstd standalone =="
    for level in 1 3 19; do
        for c in "${CORPORA[@]}"; do
            src="$WORKDIR/corpora/$c"
            if [[ -d "$src" ]]; then
                t0=$(date +%s%N)
                (cd "$WORKDIR/corpora" && tar c "$c" | zstd -q -"$level" -o "$WORKDIR/zstd-$c-$level.zst")
                t1=$(date +%s%N)
            else
                t0=$(date +%s%N)
                zstd -q -"$level" -f "$src" -o "$WORKDIR/zstd-$c-$level.zst"
                t1=$(date +%s%N)
            fi
            size=$(bytes_of "$WORKDIR/zstd-$c-$level.zst")
            apparent=$(du -sb "$src" | cut -f1)
            ratio=$(python3 -c "print(f'{$apparent/max($size,1):.3f}')")
            wall_ns=$((t1 - t0))
            wmbps=$(python3 -c "print(f'{$apparent/1048576/($wall_ns/1e9):.1f}')")
            log "  zstd -$level $c: apparent $apparent image $size ratio ${ratio}x ${wmbps} MiB/s"
            record zstd "-$level/$c" "{\"apparent\": $apparent, \"image\": $size, \"ratio\": $ratio, \"write_mbps\": $wmbps}"
        done
    done
else
    log "== zstd: WAIVER (zstd not installed) =="
    record zstd "waived" "zstd not installed"
fi

# --- 8. EntropyFS (FUSE, unprivileged) --------------------------------------
log "== entropyfs (FUSE store) =="
if [[ -e /dev/fuse ]] && command -v fusermount3 >/dev/null; then
    mkdir -p "$WORKDIR/efs-store" "$WORKDIR/mnt-efs"
    "$ENTROPYFS_BIN" mkfs "$WORKDIR/efs-store" >/dev/null
    "$ENTROPYFS_BIN" mount "$WORKDIR/efs-store" "$WORKDIR/mnt-efs" &
    EFS_PID=$!
    for _ in $(seq 1 50); do
        mountpoint -q "$WORKDIR/mnt-efs" && break
        sleep 0.1
    done
    mountpoint -q "$WORKDIR/mnt-efs" || { log "  WAIVER: entropyfs mount failed"; record entropyfs "waived" "mount failed"; }
    if mountpoint -q "$WORKDIR/mnt-efs"; then
        for c in "${CORPORA[@]}"; do
            t0=$(date +%s%N)
            cp -r "$WORKDIR/corpora/$c" "$WORKDIR/mnt-efs/$c"
            t1=$(date +%s%N)
            apparent=$(du -sb "$WORKDIR/mnt-efs/$c" | cut -f1)
            wall_ns=$((t1 - t0))
            wmbps=$(python3 -c "print(f'{$apparent/1048576/($wall_ns/1e9):.1f}')")
            # Read-back throughput through the FUSE mount.
            rt0=$(date +%s%N)
            dd if="$WORKDIR/mnt-efs/$c" of=/dev/null bs=1M status=none 2>/dev/null || true
            rt1=$(date +%s%N)
            rmbps=$(python3 -c "print(f'{$apparent/1048576/(($rt1-$rt0)/1e9):.1f}')")
            log "  $c: apparent $apparent write ${wmbps} MiB/s read ${rmbps} MiB/s"
            record entropyfs "$c" "{\"apparent\": $apparent, \"write_mbps\": $wmbps, \"read_mbps\": $rmbps}"
        done
        # Density: store physical usage after GC vs total apparent bytes.
        "$ENTROPYFS_BIN" unmount "$WORKDIR/mnt-efs" || fusermount3 -u "$WORKDIR/mnt-efs" || true
        wait "$EFS_PID" 2>/dev/null || true
        "$ENTROPYFS_BIN" fsck "$WORKDIR/efs-store" >/dev/null 2>&1 && log "  fsck: clean"
        "$ENTROPYFS_BIN" gc "$WORKDIR/efs-store" >/dev/null 2>&1 || true
        used=$("$ENTROPYFS_BIN" status "$WORKDIR/efs-store" 2>/dev/null | grep -o 'physical:[^,]*, [0-9]* used' | grep -o '[0-9]* used' | grep -o '[0-9]*' || echo 0)
        total_apparent=$(python3 -c "
import json
r = json.load(open('$results'))
a = sum(v['apparent'] for v in r['entropyfs'].values() if isinstance(v, dict) and 'apparent' in v)
print(a)")
        log "  store physical used (post-GC): $used bytes; total apparent: $total_apparent"
        if [[ "$used" -gt 0 && "$total_apparent" -gt 0 ]]; then
            ratio=$(python3 -c "print(f'{$total_apparent/$used:.3f}')")
            log "  entropyfs effective density: ${ratio}x (apparent / store physical)"
            record entropyfs "density" "{\"apparent\": $total_apparent, \"store_physical\": $used, \"ratio\": $ratio}"
        fi
    fi
else
    log "== entropyfs: WAIVER (/dev/fuse or fusermount3 missing) =="
    record entropyfs "waived" "/dev/fuse or fusermount3 missing"
fi

# --- environment + report ----------------------------------------------------
python3 - "$OUT" "$REV" "$TS" <<'EOF'
import json, platform, os, sys
out, rev, ts = sys.argv[1], sys.argv[2], sys.argv[3]
env = {
    "revision": rev,
    "created_unix": int(ts),
    "kernel": platform.release(),
    "cpu": platform.processor() or platform.machine(),
    "store_device": "/dev/nvme1n1p1",
    "uname": os.uname().nodename,
}
json.dump(env, open(f"{out}/environment.json", "w"), indent=1)
EOF

python3 - "$results" "$LOG" "$OUT" <<'EOF'
import json, sys
results, log, out = sys.argv[1], sys.argv[2], sys.argv[3]
r = json.load(open(results))
with open(f"{out}/report.md", "w") as f:
    f.write("# Filesystem court\n\n")
    f.write(f"Archive: {out}\n\n")
    f.write("Per-corpus apparent bytes / allocated-or-image bytes / ratio.\n")
    f.write("Corpus artifact: 4 unique chunks per pattern — the structured corpus\n")
    f.write("artifact is a corpus property, not a claim (methodology §8).\n\n")
    for section in ("fs", "zstd", "entropyfs"):
        f.write(f"## {section}\n\n")
        for k, v in sorted(r[section].items()):
            f.write(f"- {k}: {json.dumps(v)}\n")
        f.write("\n")
    f.write("## Waivers\n\n")
    for section in ("fs", "entropyfs"):
        for k, v in r[section].items():
            if isinstance(v, dict) and "waived" in v:
                f.write(f"- {section}/{k}: {v['waived']}\n")
    f.write("\nRun this court in a root-capable VM to clear the loop-mount waivers.\n")
print(f"court evidence written to {out}")
EOF

echo "done: $OUT"
