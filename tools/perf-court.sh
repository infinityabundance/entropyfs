#!/usr/bin/env bash
# FUSE perf court (§38, §41): mounts a fresh EntropyFS store and runs the
# Phase-6 FUSE-frontend workloads — 4K sync writes, 1M writes, sequential
# warm/cold reads, fsync latency, read-latency percentiles, and an optional
# bindgen build workload — with full context capture, archiving raw output
# and machine-readable results under evidence/performance/fuse-court-*.
#
# This is the evidence that admits (or re-labels as exploratory) the README
# Phase-6 frontend performance claims. Run with the same binary on the
# pre-Phase-6 revision to produce the "before" half of a before/after pair.
#
# Usage:
#   perf-court.sh STORE_DIR MOUNTPOINT [--size-mib N] [--label NAME]
#                [--keep-store] [--bindgen] [--no-drop-caches]
#
# STORE_DIR and MOUNTPOINT are created fresh and removed at exit unless
# --keep-store is given. The evidence archive is written relative to the
# repository root (the directory containing this script's repo).

set -euo pipefail

SIZE_MIB=64
LABEL=""
KEEP_STORE=0
DO_BINDGEN=0
DROP_CACHES=1
POSITIONAL=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --size-mib) SIZE_MIB="$2"; shift 2 ;;
        --label) LABEL="$2"; shift 2 ;;
        --keep-store) KEEP_STORE=1; shift ;;
        --bindgen) DO_BINDGEN=1; shift ;;
        --no-drop-caches) DROP_CACHES=0; shift ;;
        -h|--help) echo "usage: perf-court.sh STORE_DIR MOUNTPOINT [opts]"; exit 0 ;;
        *) POSITIONAL+=("$1"); shift ;;
    esac
done

if [[ ${#POSITIONAL[@]} -ne 2 ]]; then
    echo "error: STORE_DIR and MOUNTPOINT required" >&2
    exit 1
fi
STORE_DIR="${POSITIONAL[0]}"
MOUNTPOINT="${POSITIONAL[1]}"

# --- locate the binary and the repo ---------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENTROPYFS_BIN="${ENTROPYFS_BIN:-$REPO_ROOT/target/release/entropyfs}"
if [[ ! -x "$ENTROPYFS_BIN" ]]; then
    echo "error: $ENTROPYFS_BIN not found (build with: cargo build --release)" >&2
    exit 1
fi

# --- preflight ------------------------------------------------------------
[[ -e /dev/fuse ]] || { echo "error: /dev/fuse missing" >&2; exit 1; }
command -v fusermount3 >/dev/null || { echo "error: fusermount3 missing" >&2; exit 1; }
command -v python3 >/dev/null || { echo "error: python3 required" >&2; exit 1; }

REV="$("$ENTROPYFS_BIN" capabilities 2>/dev/null | grep -i revision | head -1 || true)"
if [[ -z "$REV" ]]; then
    REV="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo norev)"
fi
TS="$(date +%s)"
LABEL_SUFFIX=""
[[ -n "$LABEL" ]] && LABEL_SUFFIX="-$LABEL"
EVIDENCE_DIR="$REPO_ROOT/evidence/performance/fuse-court-${TS}-${REV}${LABEL_SUFFIX}"
mkdir -p "$EVIDENCE_DIR"
RAW_OUT="$EVIDENCE_DIR/raw-output.txt"
exec > >(tee "$RAW_OUT") 2>&1

echo "=== EntropyFS FUSE perf court ==="
echo "revision: $REV"
echo "binary: $ENTROPYFS_BIN"
echo "timestamp: $TS"
echo "store: $STORE_DIR"
echo "mountpoint: $MOUNTPOINT"
echo "size: ${SIZE_MIB} MiB"
echo "label: ${LABEL:-none}"
echo

# --- context capture ------------------------------------------------------
rm -rf "$STORE_DIR"
mkdir -p "$STORE_DIR" "$MOUNTPOINT"
KERNEL="$(cat /proc/sys/kernel/osrelease 2>/dev/null || echo unknown)"
GOVERNOR="$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo unknown)"
CPU_MODEL="$(grep -m1 'model name' /proc/cpuinfo | sed 's/.*: //' || echo unknown)"
DEVICE="$(stat -c '%d' "$STORE_DIR" 2>/dev/null || echo 0)"
MOUNT_DEV="$(findmnt -n -o SOURCE -T "$STORE_DIR" 2>/dev/null || echo unknown)"
FS_TYPE="$(findmnt -n -o FSTYPE -T "$STORE_DIR" 2>/dev/null || echo unknown)"
NPROC="$(nproc)"
MEM="$(grep -m1 MemTotal /proc/meminfo | awk '{print $2}')"
echo "kernel: $KERNEL"
echo "governor: $GOVERNOR"
echo "cpu: $CPU_MODEL ($NPROC threads)"
echo "memory: ${MEM} KiB"
echo "backing device: $MOUNT_DEV ($FS_TYPE, st_dev $DEVICE)"
echo

# --- diskstats sample -----------------------------------------------------
DEV_NAME="$(basename "$MOUNT_DEV")"
diskstats() {
    [[ -n "$DEV_NAME" ]] || { echo "0 0 0 0"; return; }
    awk -v n="$DEV_NAME" '$3==n {print $4, $8, $10}' /proc/diskstats 2>/dev/null || echo "0 0 0"
}
read -r -a DS_BEFORE <<< "$(diskstats)"

# --- fresh store + mount --------------------------------------------------
"$ENTROPYFS_BIN" mkfs "$STORE_DIR"
"$ENTROPYFS_BIN" mount "$STORE_DIR" "$MOUNTPOINT" &
DAEMON_PID=$!
trap 'fusermount3 -u "$MOUNTPOINT" 2>/dev/null || true; kill "$DAEMON_PID" 2>/dev/null || true' EXIT

for _ in $(seq 1 100); do
    mountpoint -q "$MOUNTPOINT" && break
    sleep 0.1
done
mountpoint -q "$MOUNTPOINT" || { echo "error: mount failed" >&2; exit 1; }
echo "mounted (daemon pid $DAEMON_PID)"
echo

# --- deterministic incompressible payload --------------------------------
PAYLOAD="$STORE_DIR/payload.bin"
python3 - "$PAYLOAD" "$SIZE_MIB" <<'PY'
import hashlib, sys
path, size_mib = sys.argv[1], int(sys.argv[2])
n = size_mib * 1024 * 1024
h = hashlib.shake_128(b"entropyfs-perf-court-payload-v1")
with open(path, "wb") as f:
    f.write(h.digest(n))
print(f"payload: {n} bytes shake_128 (deterministic, incompressible)")
PY

echo "--- 4K sync writes (write-through path, dsync) ---"
T0=$(date +%s%N)
dd if="$PAYLOAD" of="$MOUNTPOINT/bench-4k.bin" bs=4096 oflag=dsync conv=fsync status=none
T1=$(date +%s%N)
W4K_NS=$((T1 - T0))
W4K_MBPS=$(python3 -c "print(f'{$SIZE_MIB*1024*1024 / ($W4K_NS/1e9) / (1024*1024):.1f}')")
W4K_OPS=$((SIZE_MIB * 1024 * 1024 / 4096))
W4K_OP_US=$(python3 -c "print(f'{($W4K_NS/1e9)/$W4K_OPS*1e6:.0f}')")
echo "4K dsync writes: $W4K_MBPS MiB/s (${W4K_NS}ns wall, $W4K_OPS ops, ${W4K_OP_US} µs/op incl. per-op fsync)"

echo "--- 4K buffered writes (single trailing fsync) ---"
T0=$(date +%s%N)
dd if="$PAYLOAD" of="$MOUNTPOINT/bench-4k-buf.bin" bs=4096 conv=fsync status=none
T1=$(date +%s%N)
W4KB_NS=$((T1 - T0))
W4KB_MBPS=$(python3 -c "print(f'{$SIZE_MIB*1024*1024 / ($W4KB_NS/1e9) / (1024*1024):.1f}')")
echo "4K buffered writes: $W4KB_MBPS MiB/s (${W4KB_NS}ns wall + trailing fsync)"

echo "--- 1M writes ---"
T0=$(date +%s%N)
dd if="$PAYLOAD" of="$MOUNTPOINT/bench-1m.bin" bs=1M conv=fsync status=none
T1=$(date +%s%N)
W1M_NS=$((T1 - T0))
W1M_MBPS=$(python3 -c "print(f'{$SIZE_MIB*1024*1024 / ($W1M_NS/1e9) / (1024*1024):.1f}')")
echo "1M writes: $W1M_MBPS MiB/s (${W1M_NS}ns wall + fsync)"

echo "--- fsync latency (10 fsyncs of the 1M file) ---"
FSYNC_LATS="$STORE_DIR/fsync-lats.txt"
python3 - "$MOUNTPOINT/bench-1m.bin" "$FSYNC_LATS" <<'PY'
import os, sys, time
path, out = sys.argv[1], sys.argv[2]
fd = os.open(path, os.O_RDONLY)
lats = []
for _ in range(10):
    t0 = time.perf_counter_ns()
    os.fsync(fd)
    lats.append((time.perf_counter_ns() - t0) / 1e3)
os.close(fd)
with open(out, "w") as f:
    for l in lats:
        f.write(f"{l:.1f}\n")
PY
FSYNC_P50=$(python3 -c "
import statistics
l=[float(x) for x in open('$FSYNC_LATS')]
l.sort()
print(f'{l[int(len(l)*0.5)]:.0f} {l[int(len(l)*0.95)]:.0f} {l[int(len(l)*0.99)]:.0f}')
")
echo "fsync latency µs p50/p95/p99: $FSYNC_P50"

echo "--- sequential read (warm) ×3 ---"
BEST_READ_NS=0
for i in 1 2 3; do
    T0=$(date +%s%N)
    dd if="$MOUNTPOINT/bench-1m.bin" of=/dev/null bs=1M status=none
    T1=$(date +%s%N)
    NS=$((T1 - T0))
    [[ $NS -lt $BEST_READ_NS || $BEST_READ_NS -eq 0 ]] && BEST_READ_NS=$NS
done
READ_MBPS=$(python3 -c "print(f'{$SIZE_MIB*1024*1024 / ($BEST_READ_NS/1e9) / (1024*1024):.1f}')")
echo "warm sequential read: $READ_MBPS MiB/s (best of 3)"

CACHE_STATE="warm (retained page cache)"
echo "--- cold read ---"
if [[ $DROP_CACHES -eq 1 ]] && [[ "$(id -u)" -eq 0 ]] && [[ -w /proc/sys/vm/drop_caches ]]; then
    sync
    echo 3 > /proc/sys/vm/drop_caches
    T0=$(date +%s%N)
    dd if="$MOUNTPOINT/bench-1m.bin" of=/dev/null bs=1M status=none
    T1=$(date +%s%N)
    COLD_NS=$((T1 - T0))
    COLD_MBPS=$(python3 -c "print(f'{$SIZE_MIB*1024*1024 / ($COLD_NS/1e9) / (1024*1024):.1f}')")
    CACHE_STATE="cold (drop_caches before read)"
    echo "cold sequential read: $COLD_MBPS MiB/s"
else
    COLD_MBPS="n/a"
    echo "cold read skipped: not root or drop_caches unavailable (cache state: $CACHE_STATE)"
fi

echo "--- read-latency percentiles (64× 1M reads) ---"
READ_LATS="$STORE_DIR/read-lats.txt"
> "$READ_LATS"
for _ in $(seq 1 64); do
    T0=$(date +%s%N)
    dd if="$MOUNTPOINT/bench-1m.bin" of=/dev/null bs=1M count=1 status=none
    T1=$(date +%s%N)
    echo "$(( (T1 - T0) / 1000 ))" >> "$READ_LATS"
done
READ_PCTS=$(python3 -c "
l=[int(x) for x in open('$READ_LATS')]
l.sort()
n=len(l)
print(f'{l[int(n*0.5)-1]:.0f} {l[int(n*0.95)-1]:.0f} {l[int(n*0.99)-1]:.0f}')
")
echo "1M read latency µs p50/p95/p99: $READ_PCTS"

# --- bindgen build workload ----------------------------------------------
BINDGEN_WALL_S=""
WORK_HASH=""
BINDGEN_RESULT="not-run"
if [[ $DO_BINDGEN -eq 1 ]]; then
    echo "--- bindgen build workload (target on the mount) ---"
    WORK_SRC="$REPO_ROOT/tools/bindgen-workload"
    WORK_HASH="$(tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -C "$WORK_SRC" -cf - . | sha256sum | awk '{print $1}')"
    echo "workload source hash: $WORK_HASH"
    mkdir -p "$MOUNTPOINT/work"
    cp -r "$WORK_SRC"/. "$MOUNTPOINT/work/"
    BINDGEN_LOG="$STORE_DIR/bindgen-build.log"
    T0=$(date +%s%N)
    if CARGO_TARGET_DIR="$MOUNTPOINT/work-target" cargo build --release --manifest-path "$MOUNTPOINT/work/Cargo.toml" >"$BINDGEN_LOG" 2>&1; then
        T1=$(date +%s%N)
        BINDGEN_WALL_S=$(python3 -c "print(f'{($T1-$T0)/1e9:.2f}')")
        BINDGEN_RESULT="ok"
        echo "bindgen build wall: ${BINDGEN_WALL_S}s"
        tail -3 "$BINDGEN_LOG"
    else
        T1=$(date +%s%N)
        BINDGEN_WALL_S=""
        BINDGEN_RESULT="FAILED: $(grep -m1 -E 'error|SIGBUS|panicked' "$BINDGEN_LOG" || tail -1 "$BINDGEN_LOG")"
        echo "bindgen build FAILED after $(python3 -c "print(f'{($T1-$T0)/1e9:.1f}')")s: $BINDGEN_RESULT"
    fi
    cp "$BINDGEN_LOG" "$EVIDENCE_DIR/bindgen-build.log"
    echo "cargo.lock hash: $(sha256sum "$MOUNTPOINT/work/Cargo.lock" 2>/dev/null | awk '{print $1}')"
fi

# --- unmount + cleanup ----------------------------------------------------
fusermount3 -u "$MOUNTPOINT"
wait "$DAEMON_PID" 2>/dev/null || true
trap - EXIT
echo
echo "unmounted cleanly"

read -r -a DS_AFTER <<< "$(diskstats)"
DEV_WRITES=$(( ${DS_AFTER[2]:-0} - ${DS_BEFORE[2]:-0} ))
DEV_READS=$(( ${DS_AFTER[0]:-0} - ${DS_BEFORE[0]:-0} ))
echo "device delta: writes=${DEV_WRITES} sectors, reads=${DEV_READS} sectors (${DEV_NAME:-unknown})"

# --- evidence -------------------------------------------------------------
python3 - "$EVIDENCE_DIR" "$REV" "$KERNEL" "$GOVERNOR" "$CPU_MODEL" "$NPROC" "$MEM" "$MOUNT_DEV" "$FS_TYPE" "$CACHE_STATE" "$SIZE_MIB" "$W4K_MBPS" "$W4KB_MBPS" "$W1M_MBPS" "$READ_MBPS" "$COLD_MBPS" "$FSYNC_P50" "$READ_PCTS" "$BINDGEN_WALL_S" "$BINDGEN_RESULT" "$WORK_HASH" "$DEV_WRITES" "$DEV_READS" "$TS" <<'PY'
import json, sys
(d, rev, kernel, gov, cpu, nproc, mem, dev, fstype, cache, size,
 w4k, w4kb, w1m, read, cold, fsync, readpcts, bindgen, bindgen_result, workhash, dw, dr, ts) = sys.argv[1:]
fsync_p = fsync.split()
read_p = readpcts.split()
results = {
    "timestamp_unix": int(ts),
    "revision": rev,
    "kernel": kernel,
    "governor": gov,
    "cpu_model": cpu,
    "cpu_count": int(nproc),
    "memory_kib": int(mem),
    "backing_device": dev,
    "backing_fstype": fstype,
    "cache_state": cache,
    "payload_mib": int(size),
    "writes_4k_dsync_mbps": float(w4k),
    "writes_4k_buffered_mbps": float(w4kb),
    "writes_1m_mbps": float(w1m),
    "read_warm_mbps": float(read),
    "read_cold_mbps": None if cold == "n/a" else float(cold),
    "fsync_latency_us": {"p50": float(fsync_p[0]), "p95": float(fsync_p[1]), "p99": float(fsync_p[2])},
    "read_1m_latency_us": {"p50": float(read_p[0]), "p95": float(read_p[1]), "p99": float(read_p[2])},
    "bindgen_build_wall_s": None if not bindgen else float(bindgen),
    "bindgen_build_result": bindgen_result,
    "bindgen_workload_source_hash": workhash or None,
    "device_write_sectors_delta": int(dw),
    "device_read_sectors_delta": int(dr),
}
with open(f"{d}/results.json", "w") as f:
    json.dump(results, f, indent=2)
    f.write("\n")
print(f"evidence written: {d}/")
PY

echo "=== court complete ==="
