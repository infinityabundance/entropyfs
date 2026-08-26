#!/usr/bin/env bash
# Phase-11E mount validation court: the fair worker pool END-TO-END over FUSE.
#
# The 11E probe (evidence/performance/worker-pool-probe-1787769464-8fdea62/)
# sealed pool-16 as KEPT with the 11C semaphore remaining the mount default
# "until the mounted-FUSE court validates the pool end-to-end". This court is
# that validation: it runs the same workload battery under semaphore /
# pool-8 / pool-16 at FUSE threads 1/4/8/16 and applies the adoption gate the
# 11E brief defined:
#
#   pool-16 becomes mount default only if:
#     - parallel throughput improves or stays neutral
#     - p95/p99 materially improve
#     - serial workloads do not materially regress
#     - CPU increase remains bounded
#     - crash/fsck/readback stay clean
#
# If pool-8 turns out better for real mounted workloads, the court overrules
# the synthetic probe (the brief's explicit clause).
#
# Workload battery (each writes DISTINCT content — the probe's
# per-write-distinct-content discipline, so no workload measures a dedup hit
# against another workload's bytes):
#
#   serial_cp       serial copies of a 24-file corpus            (regression control)
#   serial_dd       dd 128 MiB to one file                       (regression control)
#   parallel_write  T concurrent copies of a fresh corpus
#   parallel_read   T concurrent cmp of the written corpus
#   latency_write   T threads x 32 x 1 MiB pwrite, per-write p50/p95/p99
#   latency_read    T threads x 32 x 1 MiB pread,  per-read  p50/p95/p99
#   ns_ops          T threads x 30 mkdir/create/write/read/rename/unlink/rmdir
#                   cycles, per-cycle p50/p95/p99 (the 10D short-latency path)
#   tree_copy       T parallel cp -r of a generated source tree
#   untar           T parallel tar -xf of a generated tree archive
#   make_j          make -j T over a generated small C workload
#   cargo_build     the bindgen-workload cargo build (target on the mount)
#   mixed_rw        T/2 concurrent writers + T/2 concurrent readers
#   fsync_heavy     T threads x 24 small write+fsync, per-fsync p50/p95/p99
#
# Measurement rules (symmetric across schedulers):
#   - wall / throughput per workload (daemon CPU via /proc sampled per
#     workload, like court-threads-parallel.sh);
#   - external per-op latency percentiles measured IN the driver (the
#     write-through mount makes pwrite return only after the daemon acked);
#   - the daemon's cumulative FUSE op stats (--stats-file) archived per cell;
#   - byte-exact readback of every written file BEFORE unmount + fsck on the
#     store AFTER unmount, per cell (recorded in cleanliness.tsv; the gate
#     requires both clean).
#
# Usage: tools/court-worker-pool-mount.sh [WORKDIR] [OUTROOT]
#   Unprivileged (FUSE via fusermount3).
#   Env: COURT_SCHEDULERS (default "semaphore pool-8 pool-16")
#        COURT_FUSE_THREADS (default "1 4 8 16")
#        COURT_WORKLOADS (default: the full battery above; comma-separated)
#        COURT_TREE_FILES (default 200)  COURT_TAR_FILES (default 200)
#        COURT_C_FILES (default 8)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKDIR="${1:-$REPO_ROOT/target/court-worker-pool-mount-scratch}"
OUTROOT="${2:-$REPO_ROOT/evidence/performance}"
SCHEDULERS="${COURT_SCHEDULERS:-semaphore pool-8 pool-16}"
FUSE_THREADS="${COURT_FUSE_THREADS:-1 4 8 16}"
WORKLOADS="${COURT_WORKLOADS:-serial_cp,serial_dd,parallel_write,parallel_read,latency_write,latency_read,ns_ops,tree_copy,untar,make_j,cargo_build,mixed_rw,fsync_heavy}"
TREE_FILES="${COURT_TREE_FILES:-200}"
TAR_FILES="${COURT_TAR_FILES:-200}"
C_FILES="${COURT_C_FILES:-8}"
BIN="$REPO_ROOT/target/release/entropyfs"

if [[ ! -x "$BIN" ]]; then
    echo "error: $BIN missing (cargo build --release)" >&2
    exit 1
fi
command -v fusermount3 >/dev/null || { echo "error: fusermount3 missing" >&2; exit 1; }
command -v python3 >/dev/null || { echo "error: python3 required" >&2; exit 1; }
command -v tar >/dev/null || { echo "error: tar required" >&2; exit 1; }

TS="$(date +%s)"
REV="$(git -C "$REPO_ROOT" rev-parse --short HEAD)"
OUT="$OUTROOT/worker-pool-mount-court-$TS-$REV"
mkdir -p "$OUT" "$WORKDIR"
SUMMARY="$OUT/summary.tsv"
CLEAN="$OUT/cleanliness.tsv"
: > "$OUT/op-stats.log"
: > "$OUT/errors.log"
printf "scheduler\tfuse_threads\tworkload\twall_s\tthroughput\tunit\tdaemon_cpu_s\top_p50_us\top_p95_us\top_p99_us\tfuse_max_concurrency\n" > "$SUMMARY"
printf "scheduler\tfuse_threads\treadback\tfsck\n" > "$CLEAN"

# ---------------------------------------------------------------------------
# Corpus generation (deterministic; per-workload distinct content).
# ---------------------------------------------------------------------------
# A 24-file corpus (8 compressible text, 8 incompressible shake, 8 zeros) —
# same classes as court-threads-parallel.sh, with a per-workload seed so no
# workload dedups against another workload's bytes.
gen_24_corpus() {
    local d="$1" seed="$2"
    mkdir -p "$d"
    python3 - "$d" "$seed" <<'PY'
import hashlib, os, sys
d, seed = sys.argv[1], sys.argv[2]
text = (b"the quick brown fox jumps over the lazy dog and the entropic "
        b"filesystem persists irreducible state. " * 16000)[:512*1024]
zero = b"\0" * (512*1024)
h = hashlib.shake_128(("entropyfs-worker-pool-mount-" + seed).encode())
rand = h.digest(512*1024)
for i in range(8):
    open(f"{d}/text-{i}.bin", "wb").write(text)
    open(f"{d}/rand-{i}.bin", "wb").write(rand)
    open(f"{d}/zero-{i}.bin", "wb").write(zero)
PY
}

# A source tree of TREE_FILES small text files (deterministic, structured —
# repeated headers + varying bodies, so the dictionary/model families engage).
gen_tree() {
    local d="$1" seed="$2" n="$3"
    mkdir -p "$d"
    python3 - "$d" "$seed" "$n" <<'PY'
import hashlib, os, sys
d, seed, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
h = hashlib.shake_128(("tree-" + seed).encode())
for i in range(n):
    body = hashlib.shake_128(f"treebody-{seed}-{i}".encode()).digest(256)
    with open(f"{d}/f{i:04d}.cfg", "wb") as f:
        f.write(b"# generated config\nhost = node-%d\nport = %d\nuser = svc\n"
                b"flags = %s\npayload = " % (i, 8000 + i % 200, b"ab" * (i % 5)))
        f.write(body.hex().encode())
        f.write(b"\n# end\n")
PY
}

# A small C workload for make -j T (deterministic; compiles in seconds).
# The seed is folded into a NUMERIC literal — the generated C must compile.
gen_c_workload() {
    local d="$1" seed="$2" n="$3"
    mkdir -p "$d"
    python3 - "$d" "$seed" "$n" <<'PY'
import sys
d, seed, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
seedval = int.from_bytes(seed.encode(), "little") % 100000
units = []
for i in range(n):
    src = f"{d}/unit{i}.c"
    with open(src, "w") as f:
        f.write(f"#include <stddef.h>\nunsigned long long unit{i}(unsigned long long x) {{\n"
                f"    x ^= 0x9E3779B97F4A7C15ULL + {i};\n"
                f"    x = (x ^ (x >> 30)) * 0xBF58476D1CE4E5B9ULL;\n"
                f"    return x + {seedval};\n}}\n")
    units.append(f"unit{i}.o")
with open(f"{d}/Makefile", "w") as f:
    f.write("CC = cc\nCFLAGS = -O2 -c\n")
    f.write("OBJS = " + " ".join(units) + "\n")
    f.write("all: prog\n")
    f.write("prog: main.o $(OBJS)\n\t$(CC) -o prog main.o $(OBJS)\n")
    f.write("main.o: main.c\n\t$(CC) $(CFLAGS) main.c\n")
    f.write("$(OBJS): %.o: %.c\n\t$(CC) $(CFLAGS) $<\n")
with open(f"{d}/main.c", "w") as f:
    f.write("#include <stdio.h>\n#include <stdlib.h>\n"
            "unsigned long long unit0(unsigned long long);\n")
    for i in range(1, n):
        f.write(f"unsigned long long unit{i}(unsigned long long);\n")
    f.write("int main(void) { unsigned long long x = 1;\n")
    for i in range(n):
        f.write(f"  x = unit{i}(x);\n")
    f.write('  printf("%llu\\n", x); return 0; }\n')
PY
}

# Daemon CPU seconds (utime+stime) from /proc.
cpu_secs() {
    awk '{print ($14+$15)/100.0}' "/proc/$1/stat" 2>/dev/null || echo 0
}

# One row's latency percentiles printed by the python drivers:
#   p50_us p95_us p99_us  (space-separated, one line)

# ---------------------------------------------------------------------------
# One cell: one scheduler config at one FUSE thread count, full battery.
# ---------------------------------------------------------------------------
run_cell() {
    local sched="$1" t="$2" RUN="$3"
    echo "== cell scheduler=$sched fuse_threads=$t =="
    rm -rf "$RUN"
    mkdir -p "$RUN/mnt"
    "$BIN" mkfs "$RUN/store" >/dev/null

    local MOUNT_ARGS=("--threads" "$t" "--no-background-optimize" "--stats-file" "$RUN/stats.txt")
    if [[ "$sched" == pool-* ]]; then
        MOUNT_ARGS+=("--worker-pool" "${sched#pool-}")
    fi
    "$BIN" mount "${MOUNT_ARGS[@]}" "$RUN/store" "$RUN/mnt" &
    local DAEMON=$!
    for _ in $(seq 1 200); do
        mountpoint -q "$RUN/mnt" && break
        sleep 0.1
    done
    mountpoint -q "$RUN/mnt" || { echo "error: mount failed ($sched threads=$t)" >&2; exit 1; }

    cleanup() { fusermount3 -u "$RUN/mnt" 2>/dev/null || true; }
    trap cleanup EXIT

    local manifest="$RUN/manifest.txt"
    : > "$manifest"
    local READBACK=OK
    local CPUS0 CPUS1 T0 T1 WALL MBPS

    # --- serial_cp: regression control (serial copies, 24-file corpus) ---
    if [[ "$WORKLOADS" == *serial_cp* ]]; then
        gen_24_corpus "$RUN/corpus-a" "a"
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        for f in "$RUN"/corpus-a/*.bin; do cp "$f" "$RUN/mnt/"; echo "$f" >> "$manifest"; done
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        MBPS=$(python3 -c "print(f'{24*512*1024/(($T1-$T0)/1e9)/1048576:.1f}')")
        echo -e "$sched\t$t\tserial_cp\t$WALL\t$MBPS\tMB/s\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t-\t-\t-\t-" >> "$SUMMARY"
    fi

    # --- serial_dd: regression control ---
    if [[ "$WORKLOADS" == *serial_dd* ]]; then
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        dd if=/dev/zero of="$RUN/mnt/dd.bin" bs=1M count=128 oflag=direct 2>/dev/null || \
        dd if=/dev/zero of="$RUN/mnt/dd.bin" bs=1M count=128 2>/dev/null
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        MBPS=$(python3 -c "print(f'{128/(($T1-$T0)/1e9):.1f}')")
        echo -e "$sched\t$t\tserial_dd\t$WALL\t$MBPS\tMB/s\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t-\t-\t-\t-" >> "$SUMMARY"
    fi

    # --- parallel_write: T concurrent copies of a FRESH corpus ---
    if [[ "$WORKLOADS" == *parallel_write* ]]; then
        gen_24_corpus "$RUN/corpus-b" "b"
        mkdir -p "$RUN/mnt/data-b"
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        find "$RUN/corpus-b" -maxdepth 1 -name '*.bin' -print | sort \
            | xargs -P "$t" -I{} cp {} "$RUN/mnt/data-b/"
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        for f in "$RUN"/corpus-b/*.bin; do echo "$f" >> "$manifest"; done
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        MBPS=$(python3 -c "print(f'{24*512*1024/(($T1-$T0)/1e9)/1048576:.1f}')")
        echo -e "$sched\t$t\tparallel_write\t$WALL\t$MBPS\tMB/s\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t-\t-\t-\t-" >> "$SUMMARY"
    fi

    # --- parallel_read: T concurrent cmp of the written corpus ---
    if [[ "$WORKLOADS" == *parallel_read* ]]; then
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        find "$RUN/corpus-b" -maxdepth 1 -name '*.bin' -print | sort \
            | xargs -P "$t" -I{} sh -c 'cmp "$1" "$2/$(basename "$1")"' _ {} "$RUN/mnt/data-b"
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        MBPS=$(python3 -c "print(f'{24*512*1024/(($T1-$T0)/1e9)/1048576:.1f}')")
        echo -e "$sched\t$t\tparallel_read\t$WALL\t$MBPS\tMB/s\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t-\t-\t-\t-" >> "$SUMMARY"
    fi

    # --- latency_write: T threads x 32 x 1 MiB pwrite, per-write p50/p95/p99 ---
    if [[ "$WORKLOADS" == *latency_write* ]]; then
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        read -r P50 P95 P99 < <(python3 - "$RUN/mnt" "$t" <<'PY'
import hashlib, os, sys, threading, time
mnt, t = sys.argv[1], int(sys.argv[2])
lats = []
lock = threading.Lock()
barrier = threading.Barrier(t)
def worker(w):
    buf = hashlib.shake_128(f"lw-{w}".encode()).digest(1024*1024)
    name = f"lw-{w}.bin"
    with open(f"{mnt}/{name}", "wb") as f:
        barrier.wait()
        for i in range(32):
            t0 = time.perf_counter_ns()
            f.write(buf)
            f.flush()
            with lock:
                lats.append(time.perf_counter_ns() - t0)
    # Byte-exact self-check (the write-through mount made every write
    # visible; the content is deterministic, so a read-back compare is
    # exact). This is the probe's read-back discipline applied in-mount.
    with open(f"{mnt}/{name}", "rb") as f:
        expect = buf * 32
        got = f.read()
        assert got == expect, f"lw-{w}.bin read-back mismatch ({len(got)} != {len(expect)})"
threads = [threading.Thread(target=worker, args=(w,)) for w in range(t)]
for th in threads: th.start()
for th in threads: th.join()
lats.sort()
def pct(q):
    return lats[min(len(lats)-1, int((len(lats)-1)*q))]
print(f"{pct(0.50)/1e3:.1f} {pct(0.95)/1e3:.1f} {pct(0.99)/1e3:.1f}")
PY
)
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        echo -e "$sched\t$t\tlatency_write\t$WALL\t-\tus\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t$P50\t$P95\t$P99\t-" >> "$SUMMARY"
    fi

    # --- latency_read: T threads x 32 x 1 MiB pread, per-read p50/p95/p99 ---
    if [[ "$WORKLOADS" == *latency_read* ]]; then
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        read -r P50 P95 P99 < <(python3 - "$RUN/mnt/data-b" "$t" <<'PY'
import os, sys, threading, time
data, t = sys.argv[1], int(sys.argv[2])
files = [f"{data}/{n}" for n in sorted(os.listdir(data))]
lats = []
lock = threading.Lock()
barrier = threading.Barrier(t)
def worker(w):
    with open(files[w % len(files)], "rb") as f:
        barrier.wait()
        for i in range(32):
            f.seek(0)
            t0 = time.perf_counter_ns()
            while True:
                chunk = f.read(1024*1024)
                if not chunk:
                    break
            with lock:
                lats.append(time.perf_counter_ns() - t0)
threads = [threading.Thread(target=worker, args=(w,)) for w in range(t)]
for th in threads: th.start()
for th in threads: th.join()
lats.sort()
def pct(q):
    return lats[min(len(lats)-1, int((len(lats)-1)*q))]
print(f"{pct(0.50)/1e3:.1f} {pct(0.95)/1e3:.1f} {pct(0.99)/1e3:.1f}")
PY
)
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        echo -e "$sched\t$t\tlatency_read\t$WALL\t-\tus\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t$P50\t$P95\t$P99\t-" >> "$SUMMARY"
    fi

    # --- ns_ops: T threads x 30 mkdir/create/write/read/rename/unlink/rmdir ---
    if [[ "$WORKLOADS" == *ns_ops* ]]; then
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        read -r P50 P95 P99 < <(python3 - "$RUN/mnt" "$t" <<'PY'
import os, sys, threading, time
mnt, t = sys.argv[1], int(sys.argv[2])
lats = []
lock = threading.Lock()
barrier = threading.Barrier(t)
def worker(w):
    barrier.wait()
    for i in range(30):
        d = f"{mnt}/w{w}-d{i}"
        t0 = time.perf_counter_ns()
        os.mkdir(d)
        p = f"{d}/f{i}"
        fd = os.open(p, os.O_CREAT | os.O_WRONLY, 0o644)
        os.write(fd, b"x" * 4096)
        os.close(fd)
        os.rename(p, f"{d}/g{i}")
        with open(f"{d}/g{i}", "rb") as f:
            assert f.read(4096) == b"x" * 4096
        os.unlink(f"{d}/g{i}")
        os.rmdir(d)
        with lock:
            lats.append(time.perf_counter_ns() - t0)
threads = [threading.Thread(target=worker, args=(w,)) for w in range(t)]
for th in threads: th.start()
for th in threads: th.join()
lats.sort()
def pct(q):
    return lats[min(len(lats)-1, int((len(lats)-1)*q))]
print(f"{pct(0.50)/1e3:.1f} {pct(0.95)/1e3:.1f} {pct(0.99)/1e3:.1f}")
PY
)
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        echo -e "$sched\t$t\tns_ops\t$WALL\t-\tcyc/s\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t$P50\t$P95\t$P99\t-" >> "$SUMMARY"
    fi

    # --- tree_copy: T parallel cp -r of a generated source tree ---
    if [[ "$WORKLOADS" == *tree_copy* ]]; then
        gen_tree "$RUN/tree" "tc" "$TREE_FILES"
        mkdir -p "$RUN/mnt/trees"
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        seq 1 "$t" | xargs -P "$t" -I{} sh -c 'cp -r "$1/tree" "$1/mnt/trees/tree-{}"' _ "$RUN"
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        find "$RUN/tree" -type f | sort >> "$manifest"
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        echo -e "$sched\t$t\ttree_copy\t$WALL\t-\ttrees\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t-\t-\t-\t-" >> "$SUMMARY"
    fi

    # --- untar: T parallel tar -xf of a generated tree archive ---
    if [[ "$WORKLOADS" == *untar* ]]; then
        gen_tree "$RUN/tarsrc" "ut" "$TAR_FILES"
        tar -C "$RUN" -cf "$RUN/tree.tar" tarsrc
        mkdir -p "$RUN/mnt/untars"
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        seq 1 "$t" | xargs -P "$t" -I{} sh -c 'mkdir -p "$1/mnt/untars/ut-{}" && tar -C "$1/mnt/untars/ut-{}" -xf "$1/tree.tar"' _ "$RUN"
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        find "$RUN/tarsrc" -type f | sort >> "$manifest"
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        echo -e "$sched\t$t\tuntar\t$WALL\t-\ttars\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t-\t-\t-\t-" >> "$SUMMARY"
    fi

    # --- make_j: make -j T over the C workload ---
    if [[ "$WORKLOADS" == *make_j* ]]; then
        gen_c_workload "$RUN/cwork" "mk" "$C_FILES"
        cp -r "$RUN/cwork" "$RUN/mnt/cwork"
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        make -C "$RUN/mnt/cwork" -j "$t" >/dev/null 2>&1 || echo "make FAILED ($sched threads=$t)" >> "$OUT/errors.log"
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        echo -e "$sched\t$t\tmake_j\t$WALL\t-\t-\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t-\t-\t-\t-" >> "$SUMMARY"
    fi

    # --- cargo_build: the bindgen workload build (target on the mount) ---
    if [[ "$WORKLOADS" == *cargo_build* ]]; then
        cp -r "$REPO_ROOT/tools/bindgen-workload" "$RUN/mnt/bindgen-workload"
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        CARGO_TARGET_DIR="$RUN/mnt/work-target" cargo build --release \
            --manifest-path "$RUN/mnt/bindgen-workload/Cargo.toml" >"$RUN/cargo.log" 2>&1 \
            || echo "bindgen build FAILED ($sched threads=$t)" >> "$OUT/errors.log"
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        echo -e "$sched\t$t\tcargo_build\t$WALL\t-\t-\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t-\t-\t-\t-" >> "$SUMMARY"
    fi

    # --- mixed_rw: T/2 writers + T/2 readers concurrently ---
    if [[ "$WORKLOADS" == *mixed_rw* ]]; then
        gen_24_corpus "$RUN/corpus-c" "c"
        mkdir -p "$RUN/mnt/data-c"
        local RW=$((t / 2)); [[ "$RW" -lt 1 ]] && RW=1
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        (
            find "$RUN/corpus-c" -maxdepth 1 -name '*.bin' -print | sort \
                | xargs -P "$RW" -I{} cp {} "$RUN/mnt/data-c/"
        ) &
        local WPID=$!
        find "$RUN/corpus-b" -maxdepth 1 -name '*.bin' -print | sort \
            | xargs -P "$RW" -I{} sh -c 'cmp "$1" "$2/data-b/$(basename "$1")"' _ {} "$RUN/mnt"
        wait "$WPID"
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        for f in "$RUN"/corpus-c/*.bin; do echo "$f" >> "$manifest"; done
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        echo -e "$sched\t$t\tmixed_rw\t$WALL\t-\t-\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t-\t-\t-\t-" >> "$SUMMARY"
    fi

    # --- fsync_heavy: T threads x 24 small write+fsync, per-fsync p50/p95/p99 ---
    if [[ "$WORKLOADS" == *fsync_heavy* ]]; then
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        read -r P50 P95 P99 < <(python3 - "$RUN/mnt" "$t" <<'PY'
import os, sys, threading, time
mnt, t = sys.argv[1], int(sys.argv[2])
lats = []
lock = threading.Lock()
barrier = threading.Barrier(t)
def worker(w):
    barrier.wait()
    for i in range(24):
        p = f"{mnt}/fsh-{w}-{i}.bin"
        fd = os.open(p, os.O_CREAT | os.O_WRONLY, 0o644)
        os.write(fd, b"y" * 65536)
        t0 = time.perf_counter_ns()
        os.fsync(fd)
        os.close(fd)
        # Byte-exact self-check (write-through: the fsync made it visible).
        with open(p, "rb") as f:
            assert f.read(65536) == b"y" * 65536
        with lock:
            lats.append(time.perf_counter_ns() - t0)
threads = [threading.Thread(target=worker, args=(w,)) for w in range(t)]
for th in threads: th.start()
for th in threads: th.join()
lats.sort()
def pct(q):
    return lats[min(len(lats)-1, int((len(lats)-1)*q))]
print(f"{pct(0.50)/1e3:.1f} {pct(0.95)/1e3:.1f} {pct(0.99)/1e3:.1f}")
PY
)
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        echo -e "$sched\t$t\tfsync_heavy\t$WALL\t-\tus\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t$P50\t$P95\t$P99\t-" >> "$SUMMARY"
    fi

    # --- Readback: byte-exact cmp of every written file (before unmount) ---
    while IFS= read -r src; do
        local dest=""
        case "$src" in
            *corpus-a*) dest="$RUN/mnt/$(basename "$src")" ;;
            *corpus-b*) dest="$RUN/mnt/data-b/$(basename "$src")" ;;
            *corpus-c*) dest="$RUN/mnt/data-c/$(basename "$src")" ;;
            */tree/*)   dest="$RUN/mnt/trees/tree-1/$(basename "$src")" ;;
            *tarsrc*)   dest="$RUN/mnt/untars/ut-1/tarsrc/$(basename "$src")" ;;
            *lw-*)      dest="$RUN/mnt/$(basename "$src")" ;;
        esac
        if [[ -n "$dest" ]] && ! cmp -s "$src" "$dest"; then
            echo "READBACK MISMATCH: $src vs $dest" >> "$OUT/errors.log"
            READBACK=FAILED
        fi
    done < "$manifest"

    # --- Unmount + fsck ---
    fusermount3 -u "$RUN/mnt"
    wait "$DAEMON" 2>/dev/null || true
    trap - EXIT
    local MAXC FSCK_RESULT
    MAXC=$(grep -m1 "max concurrency" "$RUN/stats.txt" | awk '{print $NF}' || echo "?")
    "$BIN" fsck "$RUN/store" > "$OUT/fsck-$sched-t$t.log" 2>&1 && FSCK_RESULT=OK || FSCK_RESULT=FAILED
    echo "fuse max concurrency: $MAXC ; readback: $READBACK ; fsck: $FSCK_RESULT"
    echo -e "$sched\t$t\t$READBACK\t$FSCK_RESULT" >> "$CLEAN"

    # Attach the cell's cumulative op percentiles + max concurrency to the
    # summary rows (the daemon's FUSE-op stats are cumulative over the cell;
    # the per-workload latency drivers above are the per-workload rows).
    local OP OPP50 OPP95 OPP99
    for OP in write read fsync lookup create rename; do
        local ROW
        ROW=$(grep -m1 "  $OP " "$RUN/stats.txt" || true)
        if [[ -n "$ROW" ]]; then
            OPP50=$(echo "$ROW" | sed -n 's/.*p50= *\([0-9.]*\).*/\1/p')
            OPP95=$(echo "$ROW" | sed -n 's/.*p95= *\([0-9.]*\).*/\1/p')
            OPP99=$(echo "$ROW" | sed -n 's/.*p99= *\([0-9.]*\).*/\1/p')
            echo "op_$OP $sched t$t: n=$(echo "$ROW" | sed -n 's/.*n= *\([0-9]*\).*/\1/p') p50=$OPP50 p95=$OPP95 p99=$OPP99 us" >> "$OUT/op-stats.log"
        fi
    done
    cp "$RUN/stats.txt" "$OUT/stats-$sched-t$t.txt"
    # The cell's rows carry the daemon's max request concurrency (parsed
    # from the stats dump AFTER the daemon exited — the original
    # court-threads-parallel.sh discipline).
    if [[ "$MAXC" != "?" ]]; then
        sed -i -e "/^$sched\t$t\t/ s/\t-$/\t$MAXC/" "$SUMMARY"
    fi
}

# ---------------------------------------------------------------------------
# Gate computation + decision (python): parses summary.tsv + cleanliness.tsv,
# applies the adoption gate at the contention thread count (max of
# FUSE_THREADS), writes results.json + the decision into $OUT.
# ---------------------------------------------------------------------------
write_decision() {
    python3 - "$SUMMARY" "$CLEAN" "$OUT" <<'PY'
import json, os, sys
summary, clean, out = sys.argv[1], sys.argv[2], sys.argv[3]
rows = []
with open(summary) as f:
    header = f.readline().strip().split("\t")
    for line in f:
        parts = line.strip().split("\t")
        if len(parts) == len(header):
            rows.append(dict(zip(header, parts)))
cells = {}
with open(clean) as f:
    next(f)
    for line in f:
        parts = line.strip().split("\t")
        if len(parts) >= 4:
            cells[f"{parts[0]}@{parts[1]}"] = {"readback": parts[2], "fsck": parts[3]}

def cell_of(sched, t):
    return cells.get(f"{sched}@{t}")

def get(sched, t, wl):
    for r in rows:
        if r["scheduler"] == sched and r["fuse_threads"] == t and r["workload"] == wl:
            return r
    return None

def f(x, default=0.0):
    try:
        return float(x)
    except (ValueError, TypeError):
        return default

threads = sorted({int(r["fuse_threads"]) for r in rows})
tmax = threads[-1]  # the contention thread count where the gate is decided
machine = ""
try:
    cpu = open("/proc/cpuinfo").read()
    machine = cpu.split("model name")[1].split("\n")[0].strip() if "model name" in cpu else "?"
except OSError:
    machine = "?"
results = {
    "court": "phase-11e-mount-validation",
    "rev": os.path.basename(out).rsplit("-", 1)[-1],
    "machine": machine,
    "threads": threads,
    "decided_at_fuse_threads": tmax,
    "cleanliness": cells,
    "gates": {},
    "decision": "undecided",
}

def battery_cpu(sched, t):
    return sum(f(r["daemon_cpu_s"]) for r in rows if r["scheduler"] == sched and r["fuse_threads"] == t)

sem = get("semaphore", str(tmax), "parallel_write")
gates = {}
for sched in ["pool-8", "pool-16"]:
    pw = get(sched, str(tmax), "parallel_write")
    pr = get(sched, str(tmax), "parallel_read")
    lw = get(sched, str(tmax), "latency_write")
    sc = get(sched, str(tmax), "serial_cp")
    sd = get(sched, str(tmax), "serial_dd")
    sem_pr = get("semaphore", str(tmax), "parallel_read")
    sem_lw = get("semaphore", str(tmax), "latency_write")
    sem_sc = get("semaphore", str(tmax), "serial_cp")
    sem_sd = get("semaphore", str(tmax), "serial_dd")
    g = {}
    g["parallel_write_tp_ratio"] = round(f(pw["throughput"]) / f(sem["throughput"]), 3) if pw and sem else None
    g["parallel_read_tp_ratio"] = round(f(pr["throughput"]) / f(sem_pr["throughput"]), 3) if pr and sem_pr else None
    g["serial_cp_tp_ratio"] = round(f(sc["throughput"]) / f(sem_sc["throughput"]), 3) if sc and sem_sc else None
    g["serial_dd_tp_ratio"] = round(f(sd["throughput"]) / f(sem_sd["throughput"]), 3) if sd and sem_sd else None
    if lw and sem_lw:
        g["latency_write_p95_ratio"] = round(f(lw["op_p95_us"]) / f(sem_lw["op_p95_us"]), 3) if f(sem_lw["op_p95_us"]) else None
        g["latency_write_p99_ratio"] = round(f(lw["op_p99_us"]) / f(sem_lw["op_p99_us"]), 3) if f(sem_lw["op_p99_us"]) else None
    g["battery_daemon_cpu_delta_pct"] = round(
        (battery_cpu(sched, str(tmax)) - battery_cpu("semaphore", str(tmax)))
        / max(battery_cpu("semaphore", str(tmax)), 0.001) * 100, 1)
    cell = cell_of(sched, str(tmax))
    sem_cell = cell_of("semaphore", str(tmax))
    g["clean"] = bool(cell and sem_cell and cell["readback"] == "OK" and cell["fsck"] == "OK"
                      and sem_cell["readback"] == "OK" and sem_cell["fsck"] == "OK")
    # The brief's criteria (ratios >= 1 = improvement). "p95/p99 materially
    # improve" is the brief's phrase — NOT a rigid both-under-0.60 bar
    # (the 11E brief explicitly warned against too-rigid gates: a fair
    # scheduler may legitimately trade a little median for a huge tail
    # cut). The probe's own hard gate was p99 <= 0.60x; the mounted court
    # applies p99 <= 0.60x AND p95 <= 0.70x (a ~30%+ tail cut is
    # material), with the measured ratios always reported.
    g["parallel_neutral_or_better"] = (g["parallel_write_tp_ratio"] or 0) >= 0.95 and (g["parallel_read_tp_ratio"] or 0) >= 0.95
    g["tail_improved"] = (g.get("latency_write_p99_ratio") or 1.0) <= 0.60 and (g.get("latency_write_p95_ratio") or 1.0) <= 0.70
    g["serial_no_regression"] = (g["serial_cp_tp_ratio"] or 1.0) >= 0.90 and (g["serial_dd_tp_ratio"] or 1.0) >= 0.90
    g["cpu_bounded"] = (g["battery_daemon_cpu_delta_pct"] if g["battery_daemon_cpu_delta_pct"] is not None else 0) <= 7.0
    g["pass"] = bool(g["parallel_neutral_or_better"] and g["tail_improved"]
                     and g["serial_no_regression"] and g["cpu_bounded"] and g["clean"])
    gates[sched] = g
results["gates"] = gates

passed = [s for s, g in gates.items() if g["pass"]]
# Prefer pool-16 (the probe's adopted config) when both pass: it holds the
# latency and parallel-write advantage; pool-8's CPU win is already
# "bounded" for pool-16, and the pool-8 row is the documented
# lower-power alternative either way.
if "pool-16" in passed:
    results["decision"] = "POOL-16 BECOMES THE MOUNT DEFAULT"
elif "pool-8" in passed:
    results["decision"] = "POOL-8 OVERRULES THE SYNTHETIC PROBE (mount default)"
elif passed:
    results["decision"] = f"MOUNT DEFAULT: {passed[0]}"
else:
    results["decision"] = "KEEP THE 11C SEMAPHORE AS MOUNT DEFAULT (gates not met)"
with open(os.path.join(out, "results.json"), "w") as f:
    json.dump(results, f, indent=2)
print("decision:", results["decision"])
print(json.dumps(results["gates"], indent=2))
PY
}

# ---------------------------------------------------------------------------
for sched in $SCHEDULERS; do
    for t in $FUSE_THREADS; do
        run_cell "$sched" "$t" "$WORKDIR/$sched-t$t"
    done
done

echo
echo "== Phase-11E mounted-FUSE validation court =="
column -t -s$'\t' "$SUMMARY"
echo
echo "== cleanliness =="
column -t -s$'\t' "$CLEAN"
echo
write_decision
echo
echo "archive: $OUT"
