#!/usr/bin/env bash
# Phase-10F follow-up: the FUSE thread sweep RE-RUN with PARALLEL workloads.
#
# The Phase-10A sweep (serial `cp`) found a maximum FUSE request concurrency
# of ~1, so extra worker threads were useless THEN — but that measurement
# predates the 10D namespace-latency collapse, the 10E range reads, and the
# 10F batched read path. This court drives the mount with genuinely parallel
# workloads and asks whether the thread count matters NOW:
#
#   - parallel file writes   (xargs -P T concurrent copies)
#   - parallel file reads    (xargs -P T concurrent cmp)
#   - parallel namespace ops (T python threads × mkdir/create/write/read/
#                             unlink loops)
#   - make -j T              (the bindgen workload, target on the mount)
#
# Every run captures the daemon's FUSE max-request-concurrency from
# --stats-file: a workload that does not actually raise concurrency above 1
# is not a valid parallel measurement, and the summary reports it.
#
# Usage: tools/court-threads-parallel.sh [WORKDIR] [OUTROOT]
#   Unprivileged (FUSE via fusermount3). Thread counts: COURT_THREADS env,
#   default "1 2 4 8 16".

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKDIR="${1:-$REPO_ROOT/target/court-threads-parallel-scratch}"
OUTROOT="${2:-$REPO_ROOT/evidence/performance}"
THREADS="${COURT_THREADS:-1 2 4 8 16}"
BIN="$REPO_ROOT/target/release/entropyfs"

if [[ ! -x "$BIN" ]]; then
    echo "error: $BIN missing (cargo build --release)" >&2
    exit 1
fi
command -v fusermount3 >/dev/null || { echo "error: fusermount3 missing" >&2; exit 1; }
command -v python3 >/dev/null || { echo "error: python3 required" >&2; exit 1; }

TS="$(date +%s)"
OUT="$OUTROOT/court-threads-parallel-$TS"
mkdir -p "$OUT" "$WORKDIR"
SUMMARY="$OUT/summary.tsv"
echo -e "threads\tworkload\twall_s\tmbps_or_ops\tdaemon_cpu_s\tfuse_max_concurrency" > "$SUMMARY"

# Deterministic corpus: 24 files x 512 KiB (8 compressible text, 8 shake
# incompressible, 8 zeros) + the bindgen source tree.
gen_corpus() {
    local d="$1"
    rm -rf "$d"
    mkdir -p "$d"
    python3 - "$d" <<'PY'
import hashlib, os, sys
d = sys.argv[1]
text = (b"the quick brown fox jumps over the lazy dog and the entropic "
        b"filesystem persists irreducible state. " * 16000)[:512*1024]
zero = b"\0" * (512*1024)
h = hashlib.shake_128(b"entropyfs-thread-sweep-corpus")
rand = h.digest(512*1024)
for i in range(8):
    open(f"{d}/text-{i}.bin", "wb").write(text)
    open(f"{d}/rand-{i}.bin", "wb").write(rand)
    open(f"{d}/zero-{i}.bin", "wb").write(zero)
PY
    cp -r "$REPO_ROOT/tools/bindgen-workload" "$d/bindgen-workload"
}

# Daemon CPU seconds (utime+stime) from /proc.
cpu_secs() {
    awk '{print ($14+$15)/100.0}' "/proc/$1/stat" 2>/dev/null || echo 0
}

for t in $THREADS; do
    echo "== court at threads=$t =="
    RUN="$WORKDIR/t$t"
    rm -rf "$RUN"
    mkdir -p "$RUN/mnt" "$RUN/corpus"
    gen_corpus "$RUN/corpus"

    "$BIN" mkfs "$RUN/store" >/dev/null
    "$BIN" mount --threads "$t" --no-background-optimize --stats-file "$RUN/stats.txt" \
        "$RUN/store" "$RUN/mnt" &
    DAEMON=$!
    for _ in $(seq 1 100); do
        mountpoint -q "$RUN/mnt" && break
        sleep 0.1
    done
    mountpoint -q "$RUN/mnt" || { echo "error: mount failed (threads=$t)" >&2; exit 1; }
    trap "fusermount3 -u '$RUN/mnt' 2>/dev/null || true" EXIT

    mkdir -p "$RUN/mnt/data"

    # 1. Parallel write: T concurrent copies of the 24-file corpus.
    CPUS0=$(cpu_secs "$DAEMON")
    T0=$(date +%s%N)
    find "$RUN/corpus" -maxdepth 1 -name '*.bin' -print | sort \
        | xargs -P "$t" -I{} cp {} "$RUN/mnt/data/"
    T1=$(date +%s%N)
    CPUS1=$(cpu_secs "$DAEMON")
    WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
    MBPS=$(python3 -c "print(f'{24*512*1024/(($T1-$T0)/1e9)/1048576:.1f}')")
    echo -e "$t\tparallel_write\t$WALL\t$MBPS\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t" >> "$SUMMARY"

    # 2. Parallel read: T concurrent cmp of the 24 files.
    CPUS0=$(cpu_secs "$DAEMON")
    T0=$(date +%s%N)
    find "$RUN/corpus" -maxdepth 1 -name '*.bin' -print | sort \
        | xargs -P "$t" -I{} sh -c 'cmp "$1" "'"$RUN"'/mnt/data/$(basename "$1")"' _ {}
    T1=$(date +%s%N)
    CPUS1=$(cpu_secs "$DAEMON")
    WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
    MBPS=$(python3 -c "print(f'{24*512*1024/(($T1-$T0)/1e9)/1048576:.1f}')")
    echo -e "$t\tparallel_read\t$WALL\t$MBPS\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t" >> "$SUMMARY"

    # 3. Parallel namespace ops: t python threads, each 40
    #    mkdir/create/write/read/unlink cycles (the 10D short-latency path).
    CPUS0=$(cpu_secs "$DAEMON")
    T0=$(date +%s%N)
    python3 - "$RUN/mnt" "$t" <<'PY'
import os, sys, threading, time
mnt, t = sys.argv[1], int(sys.argv[2])
barrier = threading.Barrier(t)
errs = []
def worker(w):
    try:
        barrier.wait()
        for i in range(40):
            d = f"{mnt}/w{w}-d{i}"
            os.mkdir(d)
            p = f"{d}/f{i}"
            fd = os.open(p, os.O_CREAT | os.O_WRONLY, 0o644)
            os.write(fd, b"x" * 4096)
            os.close(fd)
            with open(p, "rb") as f:
                assert f.read(4096) == b"x" * 4096
            os.unlink(p)
            os.rmdir(d)
    except Exception as e:
        errs.append(e)
threads = [threading.Thread(target=worker, args=(w,)) for w in range(t)]
for th in threads: th.start()
for th in threads: th.join()
if errs:
    raise SystemExit(f"namespace worker errors: {errs[:3]}")
PY
    T1=$(date +%s%N)
    CPUS1=$(cpu_secs "$DAEMON")
    WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
    OPS=$(python3 -c "print(f'{$t*40*5:.0f}')")  # 5 ops per cycle
    echo -e "$t\tparallel_ns\t$WALL\t$OPS/s\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t" >> "$SUMMARY"

    # 4. make -j t: the bindgen workload with the target on the mount.
    cp -r "$RUN/corpus/bindgen-workload" "$RUN/mnt/bindgen-workload"
    CPUS0=$(cpu_secs "$DAEMON")
    T0=$(date +%s%N)
    CARGO_TARGET_DIR="$RUN/mnt/work-target" cargo build --release \
        --manifest-path "$RUN/mnt/bindgen-workload/Cargo.toml" >"$RUN/make-t$t.log" 2>&1 \
        || echo "bindgen build FAILED at threads=$t (see $RUN/make-t$t.log)" >> "$OUT/errors.log"
    T1=$(date +%s%N)
    CPUS1=$(cpu_secs "$DAEMON")
    WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
    echo -e "$t\tmake_j\t$WALL\t-\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t" >> "$SUMMARY"

    # FUSE max request concurrency: the stats file is dumped when the
    # daemon drops (after the unmount), so read it AFTER the daemon exits.
    fusermount3 -u "$RUN/mnt"
    wait "$DAEMON" 2>/dev/null || true
    trap - EXIT
    MAXC=$(grep -m1 "max concurrency" "$RUN/stats.txt" | awk '{print $NF}' || echo "?")
    sed -i -e "s/\t$/\t$MAXC/" "$SUMMARY"
    echo "  done (fuse max concurrency: $MAXC)"
done

echo
echo "== Phase-10F parallel-workload thread sweep =="
column -t -s$'\t' "$SUMMARY"
echo
echo "archive: $OUT"
