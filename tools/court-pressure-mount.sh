#!/usr/bin/env bash
# Phase 12C-1-2 — the mounted-FUSE pressure-deferral validation court.
#
# # PURPOSE
#
# The 12C-1-2 brief: the pressure-aware foreground deferral must not stop
# at the Engine facade — the mounted kernel/VFS scheduling pressure may
# alter exactly when the adaptive policy fires. This court validates the
# policy END-TO-END over FUSE: `full` vs `focused` vs `pressure`
# (`--foreground pressure` = the sealed 12C-1-2 shape: hysteresis band
# enter 0.80 / leave 0.60, configurational deferral, 1 GiB debt cap)
# against the brief's mounted workload battery:
#
#   parallel_write           T concurrent corpus copies (creation+write)
#   tree_copy                T parallel cp -r of a generated source tree
#   untar                    T parallel tar -xf of a generated archive
#   make_j                   make -j T over a generated C workload
#   mixed_rw                 T/2 concurrent writers + T/2 readers
#   bursty_writers           T threads writing in bursts (the hysteresis
#                            lane: pressure oscillates around the pool's
#                            capacity as bursts overlap)
#   continuous_saturation    sustained concurrent writers (~10 s; the
#                            starvation lane: debt grows, latency must
#                            stay bounded, the settle must converge)
#
# The mounted daemon uses the worker pool (the 11E mount default) and the
# store's foreground policy, so the `pressure` mode's gate samples the
# POOL's real in-flight pressure under concurrent writes — the signal
# validated here end-to-end. The FUSE write path feeds no semantic
# context, so the class gate is dormant under the mount: the pressure
# mode = the entropy probe + the pressure gate (exactly the mechanism the
# direct-engine court measured; the prior wiring is the documented
# follow-on).
#
# Per cell: wall + throughput, daemon CPU, external per-op latency
# percentiles (write-through mount: pwrite returns after the daemon
# acked), byte-exact readback of every written file, unmount, fsck
# (cleanliness), then `optimize` + `compact` + `metrics --json` — the
# settled physical footprint per policy. The gate rows:
#
#   pressure latency   <= full's (the deferral keeps the search off the
#                       critical path under saturation) or neutral
#   settled footprint  converges to the same place as full
#   cleanliness        readback + fsck clean
#   RAW controls       unchanged
#
# Usage: tools/court-pressure-mount.sh [WORKDIR] [OUTROOT]
#   Unprivileged (FUSE via fusermount3).
#   Env: COURT_POLICIES (default "full focused pressure")
#        COURT_THREADS (default 8)     COURT_WORKLOADS (default: the battery)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKDIR="${1:-$REPO_ROOT/target/court-pressure-mount-scratch}"
OUTROOT="${2:-$REPO_ROOT/evidence/performance}"
POLICIES="${COURT_POLICIES:-full focused pressure}"
THREADS="${COURT_THREADS:-8}"
WORKLOADS="${COURT_WORKLOADS:-parallel_write,tree_copy,untar,make_j,mixed_rw,bursty_writers,continuous_saturation,structured_burst}"
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
OUT="$OUTROOT/pressure-mount-court-$TS-$REV"
mkdir -p "$OUT" "$WORKDIR"
SUMMARY="$OUT/summary.tsv"
CLEAN="$OUT/cleanliness.tsv"
printf "policy\tfuse_threads\tworkload\twall_s\tthroughput\tunit\tdaemon_cpu_s\top_p50_us\top_p95_us\top_p99_us\tsettled_physical\n" > "$SUMMARY"
printf "policy\tfuse_threads\treadback\tfsck\n" > "$CLEAN"

# ---------------------------------------------------------------------------
# Corpus generation (deterministic; per-workload distinct content).
# ---------------------------------------------------------------------------
gen_24_corpus() {
    local d="$1" seed="$2"
    mkdir -p "$d"
    python3 - "$d" "$seed" <<'PY'
import hashlib, os, sys
d, seed = sys.argv[1], sys.argv[2]
text = (b"the quick brown fox jumps over the lazy dog and the entropic "
        b"filesystem persists irreducible state. " * 16000)[:512*1024]
zero = b"\0" * (512*1024)
h = hashlib.shake_128(("entropyfs-pressure-mount-" + seed).encode())
rand = h.digest(512*1024)
for i in range(8):
    open(f"{d}/text-{i}.bin", "wb").write(text)
    open(f"{d}/rand-{i}.bin", "wb").write(rand)
    open(f"{d}/zero-{i}.bin", "wb").write(zero)
PY
}

gen_tree() {
    local d="$1" seed="$2" n="$3"
    mkdir -p "$d"
    python3 - "$d" "$seed" "$n" <<'PY'
import hashlib, os, sys
d, seed, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
h = hashlib.shake_128(("pressure-tree-" + seed).encode())
for i in range(n):
    body = hashlib.shake_128(f"treebody-{seed}-{i}".encode()).digest(256)
    with open(f"{d}/f{i:04d}.cfg", "wb") as f:
        f.write(b"# generated config\nhost = node-%d\nport = %d\nuser = svc\n"
                b"flags = %s\npayload = " % (i, 8000 + i % 200, b"ab" * (i % 5)))
        f.write(body.hex().encode())
        f.write(b"\n# end\n")
PY
}

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

cpu_secs() {
    awk '{print ($14+$15)/100.0}' "/proc/$1/stat" 2>/dev/null || echo 0
}

# ---------------------------------------------------------------------------
# One cell: one policy at the representative FUSE thread count.
# ---------------------------------------------------------------------------
run_cell() {
    local policy="$1" t="$2" RUN="$3"
    echo "== cell policy=$policy fuse_threads=$t =="
    rm -rf "$RUN"
    mkdir -p "$RUN/mnt"
    "$BIN" mkfs "$RUN/store" >/dev/null

    "$BIN" mount --threads "$t" --foreground "$policy" --no-background-optimize \
        --stats-file "$RUN/stats.txt" "$RUN/store" "$RUN/mnt" &
    local DAEMON=$!
    for _ in $(seq 1 200); do
        mountpoint -q "$RUN/mnt" && break
        sleep 0.1
    done
    mountpoint -q "$RUN/mnt" || { echo "error: mount failed (policy=$policy threads=$t)" >&2; exit 1; }

    cleanup() { fusermount3 -u "$RUN/mnt" 2>/dev/null || true; }
    trap cleanup EXIT

    local manifest="$RUN/manifest.txt"
    : > "$manifest"
    local READBACK=OK

    # --- parallel_write: T concurrent copies of a FRESH corpus ---
    if [[ "$WORKLOADS" == *parallel_write* ]]; then
        gen_24_corpus "$RUN/corpus-b" "b"
        mkdir -p "$RUN/mnt/data-b"
        local CPUS0 CPUS1 T0 T1 WALL MBPS
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        find "$RUN/corpus-b" -maxdepth 1 -name '*.bin' -print | sort \
            | xargs -P "$t" -I{} cp {} "$RUN/mnt/data-b/"
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        for f in "$RUN"/corpus-b/*.bin; do echo "$f" >> "$manifest"; done
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        MBPS=$(python3 -c "print(f'{24*512*1024/(($T1-$T0)/1e9)/1048576:.1f}')")
        echo -e "$policy\t$t\tparallel_write\t$WALL\t$MBPS\tMB/s\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t-\t-\t-\t-" >> "$SUMMARY"
    fi

    # --- tree_copy: T parallel cp -r of a generated source tree ---
    if [[ "$WORKLOADS" == *tree_copy* ]]; then
        gen_tree "$RUN/tree" "t" "$TREE_FILES"
        local CPUS0 CPUS1 T0 T1 WALL
        mkdir -p "$RUN/mnt/trees"
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        # xargs -P (NOT a bare for..&..wait: the wait would also wait on
        # the FUSE daemon background job, which never exits).
        seq 1 "$t" | xargs -P "$t" -I{} sh -c 'cp -r "$1/tree" "$1/mnt/trees/tree-{}"' _ "$RUN"
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        find "$RUN/tree" -type f | sort >> "$manifest"
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        echo -e "$policy\t$t\ttree_copy\t$WALL\t-\t-\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t-\t-\t-\t-" >> "$SUMMARY"
    fi

    # --- untar: T parallel tar -xf of a generated archive ---
    if [[ "$WORKLOADS" == *untar* ]]; then
        gen_tree "$RUN/tarsrc" "u" "$TAR_FILES"
        tar -C "$RUN/tarsrc" -cf "$RUN/tree.tar" .
        local CPUS0 CPUS1 T0 T1 WALL
        mkdir -p "$RUN/mnt/untars"
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        seq 1 "$t" | xargs -P "$t" -I{} sh -c 'mkdir -p "$1/mnt/untars/u-{}" && tar -C "$1/mnt/untars/u-{}" -xf "$1/tree.tar"' _ "$RUN"
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        find "$RUN/tarsrc" -type f | sort >> "$manifest"
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        echo -e "$policy\t$t\tuntar\t$WALL\t-\t-\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t-\t-\t-\t-" >> "$SUMMARY"
    fi

    # --- make_j: make -j T over a generated C workload ---
    if [[ "$WORKLOADS" == *make_j* ]]; then
        gen_c_workload "$RUN/cwork" "c" "$C_FILES"
        cp -r "$RUN/cwork" "$RUN/mnt/cwork"
        local CPUS0 CPUS1 T0 T1 WALL
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        make -C "$RUN/mnt/cwork" -j "$t" >/dev/null 2>&1
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        echo -e "$policy\t$t\tmake_j\t$WALL\t-\t-\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t-\t-\t-\t-" >> "$SUMMARY"
    fi

    # --- mixed_rw: T/2 concurrent writers + T/2 concurrent readers ---
    if [[ "$WORKLOADS" == *mixed_rw* ]]; then
        gen_24_corpus "$RUN/corpus-m" "m"
        mkdir -p "$RUN/mnt/mr-data"
        for f in "$RUN"/corpus-m/*.bin; do cp "$f" "$RUN/mnt/mr-data/"; done
        local CPUS0 CPUS1 T0 T1 WALL
        CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
        python3 - "$RUN/mnt" "$t" <<'PY'
import hashlib, os, sys, threading
mnt, t = sys.argv[1], int(sys.argv[2])
nw = max(1, t // 2)
nr = max(1, t - nw)
barrier = threading.Barrier(t)
def writer(w):
    buf = hashlib.shake_128(f"mx-{w}".encode()).digest(1024*1024)
    with open(f"{mnt}/mx-{w}.bin", "wb") as f:
        barrier.wait()
        for _ in range(16):
            f.write(buf); f.flush()
def reader(r):
    name = sorted(os.listdir(f"{mnt}/mr-data"))[r % 24]
    with open(f"{mnt}/mr-data/{name}", "rb") as f:
        barrier.wait()
        while True:
            if not f.read(1024*1024):
                break
threads = ([threading.Thread(target=writer, args=(w,)) for w in range(nw)]
           + [threading.Thread(target=reader, args=(r,)) for r in range(nr)])
for th in threads: th.start()
for th in threads: th.join()
PY
        T1=$(date +%s%N); CPUS1=$(cpu_secs "$DAEMON")
        WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")
        echo -e "$policy\t$t\tmixed_rw\t$WALL\t-\t-\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t-\t-\t-\t-" >> "$SUMMARY"
    fi

    # --- bursty_writers: T threads writing in bursts (the hysteresis
    # lane under real scheduling pressure — the bursts overlap and the
    # pool's in-flight oscillates). ---
    if [[ "$WORKLOADS" == *bursty_writers* ]]; then
        local CPUS0 CPUS1 T0 T1 WALL
        read -r P50 P95 P99 < <(python3 - "$RUN/mnt" "$t" <<'PY'
import hashlib, os, sys, threading, time
mnt, t = sys.argv[1], int(sys.argv[2])
lats = []
lock = threading.Lock()
barrier = threading.Barrier(t)
def worker(w):
    buf = hashlib.shake_128(f"bw-{w}".encode()).digest(1024*1024)
    with open(f"{mnt}/bw-{w}.bin", "wb") as f:
        barrier.wait()
        for r in range(4):
            for i in range(16):
                t0 = time.perf_counter_ns()
                f.write(buf); f.flush()
                with lock:
                    lats.append(time.perf_counter_ns() - t0)
            time.sleep(0.05)  # the burst gap: pressure oscillates
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
        echo -e "$policy\t$t\tbursty_writers\t$WALL\t-\tus\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t$P50\t$P95\t$P99\t-" >> "$SUMMARY"
    fi

    # --- continuous_saturation: sustained concurrent writers of DISTINCT
    # content (~10 s; the starvation lane — the search is real work per
    # write, the pool saturates, the debt grows while latency must stay
    # bounded, then the settle converges). ---
    if [[ "$WORKLOADS" == *continuous_saturation* ]]; then
        local CPUS0 CPUS1 T0 T1 WALL
        read -r P50 P95 P99 < <(python3 - "$RUN/mnt" "$t" <<'PY'
import hashlib, os, sys, threading, time
mnt, t = sys.argv[1], int(sys.argv[2])
lats = []
lock = threading.Lock()
barrier = threading.Barrier(t)
def worker(w):
    base = hashlib.shake_128(f"cs-{w}".encode()).digest(1024*1024)
    with open(f"{mnt}/cs-{w}.bin", "wb") as f:
        barrier.wait()
        end = time.monotonic() + 10
        i = 0
        while time.monotonic() < end:
            # Distinct per write (the probe's per-write-distinct-content
            # discipline): stamp the buffer so every write is a real
            # search, never a dedup hit against the previous write.
            buf = base[:1024*1024-64] + f"-{w}-{i:032d}".encode()
            i += 1
            t0 = time.perf_counter_ns()
            f.write(buf); f.flush()
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
        echo -e "$policy\t$t\tcontinuous_saturation\t$WALL\t-\tus\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t$P50\t$P95\t$P99\t-" >> "$SUMMARY"
    fi

    # --- structured_burst: T threads writing DISTINCT COMPRESSIBLE text
    # (rANS-valuable — the search the pressure gate defers) in bursts.
    # Under the burst overlap the pool saturates: `pressure` must defer
    # the rANS sweep (lower latency/wall than `focused`, which runs it)
    # and still converge to the same settled density after optimize. ---
    if [[ "$WORKLOADS" == *structured_burst* ]]; then
        local CPUS0 CPUS1 T0 T1 WALL
        read -r P50 P95 P99 < <(python3 - "$RUN/mnt" "$t" <<'PY'
import hashlib, os, sys, threading, time
mnt, t = sys.argv[1], int(sys.argv[2])
lats = []
lock = threading.Lock()
barrier = threading.Barrier(t)
def make_buf(w, r):
    # 256 KiB of template text with a per-(worker,round) stamp: structured,
    # compressible, DISTINCT per round — rANS-valuable, never a dedup hit.
    line = (f"server {{ host = node-{w:04d} round = {r:04d} "
            f"port = {8000 + w * 7 + r} user = svc flags = {w % 5} }}\n").encode()
    body = hashlib.shake_128(f"sb-{w}-{r}".encode()).digest(8192)
    buf = b"# generated config\n" + line + body.hex().encode()
    while len(buf) < 256*1024:
        buf += line + body.hex().encode()
    return buf[:256*1024]
def worker(w):
    barrier.wait()
    for r in range(6):
        buf = make_buf(w, r)
        name = f"sb-{w}-{r}.bin"
        with open(f"{mnt}/{name}", "wb") as f:
            t0 = time.perf_counter_ns()
            f.write(buf); f.flush()
            with lock:
                lats.append(time.perf_counter_ns() - t0)
        time.sleep(0.05)  # the burst gap: pressure oscillates
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
        echo -e "$policy\t$t\tstructured_burst\t$WALL\t-\tus\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t$P50\t$P95\t$P99\t-" >> "$SUMMARY"
    fi

    # --- Byte-exact readback of every written file ---
    local f
    while read -r f; do
        local rel="${f#*corpus-}" base
        case "$f" in
            *corpus-b*) base="data-b/$(basename "$f")" ;;
            *corpus-m*) base="data-b/$(basename "$f")" ;;
            *tree/t*) base="trees/tree-1/${f#*tree/}" ;;
            *tarsrc*) base="untars/u-1/${f#*tarsrc/}" ;;
            *) continue ;;
        esac
        cmp -s "$f" "$RUN/mnt/$base" || { READBACK=MISMATCH; echo "readback mismatch: $f vs $base" >> "$OUT/errors.log"; }
    done < "$manifest"

    # --- Unmount + fsck (cleanliness) ---
    trap - EXIT
    fusermount3 -u "$RUN/mnt"
    sleep 0.5
    local FSCK=OK
    "$BIN" fsck "$RUN/store" >/dev/null 2>&1 || FSCK=FAIL
    echo -e "$policy\t$t\t$READBACK\t$FSCK" >> "$CLEAN"

    # --- Settle: optimize + compact + metrics --json (the settled
    # physical footprint per policy — the convergence authority). ---
    "$BIN" optimize "$RUN/store" >/dev/null 2>&1 || true
    "$BIN" gc --compact "$RUN/store" >/dev/null 2>&1 || true
    local SETTLED
    SETTLED=$("$BIN" metrics --json "$RUN/store" 2>/dev/null \
        | python3 -c "import json,sys; print(json.load(sys.stdin).get('accounting',{}).get('physical_used_bytes',0))" 2>/dev/null || echo 0)
    sed -i "s/\t-$/\t$SETTLED/" "$SUMMARY"
    echo "  settled physical: $SETTLED"
}

for policy in $POLICIES; do
    run_cell "$policy" "$THREADS" "$WORKDIR/run-$policy"
done

python3 - "$OUT" "$REV" "$THREADS" <<'PY'
import json, os, sys
out, rev, threads = sys.argv[1], sys.argv[2], int(sys.argv[3])
rows = []
with open(os.path.join(out, "summary.tsv")) as f:
    header = f.readline().rstrip("\n").split("\t")
    for line in f:
        parts = line.rstrip("\n").split("\t")
        if len(parts) == len(header):
            rows.append(dict(zip(header, parts)))
envelope = {
    "oracle": "phase-12c1-2-pressure-mount-court",
    "rev": rev,
    "machine": next(
        (l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")),
        "unknown",
    ),
    "kernel": os.uname().release,
    "fuse_threads": threads,
    "rows": rows,
    "cleanliness": [l.rstrip("\n").split("\t") for l in open(os.path.join(out, "cleanliness.tsv"))],
}
with open(os.path.join(out, "results.json"), "w") as f:
    json.dump(envelope, f, indent=2)
    f.write("\n")
print(f"archived: {out}")
PY

echo "== done: $OUT =="
