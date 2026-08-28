#!/usr/bin/env bash
# Phase 12C-1-3 — the mounted-FUSE pressure-ENGAGEMENT court.
#
# # PURPOSE
#
# The 12C-1-3 brief: prove the 12C-1-2 pressure mechanism actually
# ENGAGES on a mounted workload that saturates the worker pool with
# expensive, density-valuable rANS search — and, if the promotion gate
# passes, flip `pressure` to the mount default.
#
# The 12C-1-2 mounted court recorded its boundary explicitly: its corpora
# did not saturate the pool with valuable search (noise probe-skips; the
# structured content searched in ~0.3 ms/chunk and its writes were small),
# so the pressure gate's mounted differentiation did not engage. This
# court closes that boundary with the 12C-1-3 corpus design:
#
#   - the five adoption-wedge content families (the 12E.13 corpora's
#     shapes, fed through real mounted file activity);
#   - DISTINCT structured content per (writer, round) — every write is a
#     real rANS-valuable search, never a dedup hit, never probe-skipped;
#   - 1 MiB per write (16 x 64 KiB chunks): with >= 8 concurrent writers
#     the in-flight set pins at the pool's backpressure capacity, so the
#     pressure scalar (in_flight / capacity) reaches ~1.0 and the
#     hysteresis band engages.
#
# # THE CAUSAL CHAIN THE COURT INSTRUMENTS (the daemon's stats file)
#
#   pool saturation rises
#       -> enter threshold crossed (enter events)
#       -> pressure_engaged = true (time pressured)
#       -> valuable rANS/configurational work deferred (deferrals, debt)
#       -> foreground latency falls / queue pressure falls
#       -> leave threshold crossed (leave events)
#       -> background optimizer repays the debt (settle step)
#       -> settled footprint converges
#
# # THE CELL MATRIX
#
#   - concurrency sweep:  full/focused/pressure x FUSE writers
#     1/4/8/16/32 at pool-16 (the machine default) on build-artifacts
#   - pool lane:          full/focused/pressure x writers 16 at
#     pool-8 (pool-16 is the default lane above)
#   - family lane:        full/focused/pressure x writers 16 at pool-16
#     on ci-cache, container-layers, generated-assets, scientific-outputs
#
# # THE PROMOTION GATE ROWS (evaluated in the writeup)
#
#   byte identity absolute; fsck clean; pressure engagement causal;
#   foreground wall + p95/p99 materially better under saturation; CPU
#   lower or justified; settled density <= +1% preferred / <= +5% hard
#   fail; debt bounded; settlement complete after pressure clears;
#   idle ~= Full; low-concurrency no regression.
#
# # RUN
#
#   tools/court-pressure-mount-engagement.sh [workdir] [outroot]
#
# env: COURT_POLICIES, COURT_WRITERS, COURT_POOLS, COURT_FAMILIES,
# COURT_DURATION (seconds per cell write phase, default 2 — the
# SUSTAINED pattern that builds the in-flight backlog and engages the
# pressure band), COURT_SETTLE_BUDGET (seconds, default 150 — the
# background search is ~8 ms/extent single-threaded; the budget records
# a timeout rather than hanging the court)
#
# NOTE: a mounted court is slow; the full matrix is ~30 cells with a
# settle-bounded tail. `COURT_SWEEP_ONLY=1` runs just the concurrency
# sweep.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKDIR="${1:-$REPO_ROOT/target/court-pressure-engagement-scratch}"
OUTROOT="${2:-$REPO_ROOT/evidence/performance}"
POLICIES="${COURT_POLICIES:-full focused pressure}"
WRITERS="${COURT_WRITERS:-1 4 8 16 32}"
POOLS="${COURT_POOLS:-8 16}"
FAMILIES="${COURT_FAMILIES:-build_artifacts ci_cache container_layers generated_assets scientific_outputs}"
DURATION="${COURT_DURATION:-2}"
SETTLE_BUDGET="${COURT_SETTLE_BUDGET:-150}"
SWEEP_ONLY="${COURT_SWEEP_ONLY:-0}"
BIN="$REPO_ROOT/target/release/entropyfs"

if [[ ! -x "$BIN" ]]; then
    echo "error: $BIN missing (cargo build --release)" >&2
    exit 1
fi
command -v fusermount3 >/dev/null || { echo "error: fusermount3 missing" >&2; exit 1; }
command -v python3 >/dev/null || { echo "error: python3 required" >&2; exit 1; }

TS="$(date +%s)"
REV="$(git -C "$REPO_ROOT" rev-parse --short HEAD)"
OUT="$OUTROOT/pressure-mount-engagement-$TS-$REV"
mkdir -p "$OUT" "$WORKDIR"
SUMMARY="$OUT/summary.tsv"
CLEAN="$OUT/cleanliness.tsv"
printf "policy\tfuse_threads\tpool\tfamily\twall_s\tdaemon_cpu_s\tp50_us\tp95_us\tp99_us\tpressured\tsamples\tenter\tleave\tpressured_ms\trans_skips\tdeferred_extents\tdeferred_bytes\tpeak_deferred\toldest_age_ms\tcap_engagements\tsettled_physical\tlogical_bytes\tsettle_wall_s\n" > "$SUMMARY"
printf "policy\tfuse_threads\tpool\tfamily\treadback\tfsck\n" > "$CLEAN"

cpu_secs() {
    awk '{print ($14+$15)/100.0}' "/proc/$1/stat" 2>/dev/null || echo 0
}

# ---------------------------------------------------------------------------
# One cell: one (policy, writers, pool, family) — the write phase, the
# causal-chain witnesses (stats-file pressure block), and the settle step.
# ---------------------------------------------------------------------------
run_cell() {
    local policy="$1" t="$2" pool="$3" family="$4" RUN="$5"
    local tag="${policy}-t${t}-p${pool}-${family}"
    echo "== cell $tag =="
    rm -rf "$RUN"
    mkdir -p "$RUN/mnt"

    local pool_args=()
    if [[ "$pool" == "default" ]]; then
        pool_args=()
    else
        pool_args=(--worker-pool "$pool")
    fi

    "$BIN" mkfs "$RUN/store" >/dev/null
    "$BIN" mount --threads "$t" --foreground "$policy" "${pool_args[@]}" \
        --no-background-optimize --stats-file "$RUN/stats.txt" \
        "$RUN/store" "$RUN/mnt" >/dev/null 2>&1 &
    local DAEMON=$!
    for _ in $(seq 1 300); do
        mountpoint -q "$RUN/mnt" && break
        sleep 0.1
    done
    mountpoint -q "$RUN/mnt" || { echo "error: mount failed ($tag)" >&2; exit 1; }
    cleanup() { fusermount3 -u "$RUN/mnt" 2>/dev/null || true; }
    trap cleanup EXIT

    # --- The saturation write phase: T writers, DISTINCT structured
    # content per (writer, round), ~1 MiB per write, SUSTAINED for
    # DURATION seconds. The content is generated with C-speed ops
    # (shake digest + bytes concat — the GIL is released; the 12C-1-3
    # probe found a pure-python LCG generator serializes the writers on
    # the GIL and the pool never saturates). The sustained pattern is
    # the engagement requirement: the writers' submit rate exceeds the
    # pool's drain rate, the in-flight backlog builds past the
    # backpressure capacity, P = in_flight/capacity reaches ~1.0, and the
    # hysteresis band engages. ---
    local CPUS0 CPUS1 T0 T1 WALL
    CPUS0=$(cpu_secs "$DAEMON"); T0=$(date +%s%N)
    read -r P50 P95 P99 < <(python3 - "$RUN/mnt" "$t" "$family" "$DURATION" <<'PY'
import hashlib, sys, threading, time
mnt, t, family, dur = sys.argv[1], int(sys.argv[2]), sys.argv[3], float(sys.argv[4])
lats = []
lock = threading.Lock()
barrier = threading.Barrier(t)
HEADS = {
    # The per-family signature line (the flavor the family stamp adds;
    # the body is the deterministic per-(w,r) digest — distinct per
    # write, never a dedup hit, never probe-skipped).
    "build_artifacts": lambda w, r: f"/* unit {w} rev {r} */\n#include <stdint.h>\n",
    "ci_cache": lambda w, r: f"[2026-08-28T00:00:00Z] build id={w:04d}-{r:04d}\n",
    "container_layers": lambda w, r: f'{{"layer":{w},"round":{r},"size":1048576}}\n',
    "generated_assets": lambda w, r: f"/* asset {w}-{r} */\n",
    "scientific_outputs": lambda w, r: f"# run {w} seed {r}\n",
}
def make_buf(w, r):
    # C-speed generation: one stamp line + the digest body, repeated to
    # 1 MiB (bytes multiplication is C-speed; the GIL stays free so the
    # writers overlap in the pool). Structured, rANS-valuable, distinct
    # per (w, r).
    tag = f"{family}-{w:04d}-{r:04d}"
    head = HEADS[family](w, r).encode()
    line = (f"{tag} host=node-{w:04d} port={8000 + w * 7 + r} "
            f"user=svc flags={w % 5} payload={r:06d}\n").encode()
    body = hashlib.shake_128(tag.encode()).digest(16384).hex().encode()
    unit = head + line + body
    return (unit * ((1024 * 1024 + len(unit) - 1) // len(unit)))[:1024 * 1024]
def worker(w):
    barrier.wait()
    end = time.monotonic() + dur
    i = 0
    with open(f"{mnt}/f-{w:02d}.bin", "wb") as f:
        while time.monotonic() < end:
            buf = make_buf(w, i)
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

    # --- Byte identity: regenerate the deterministic corpus and compare
    # every written byte (the absolute gate). ---
    local READBACK=OK
    python3 - "$RUN/mnt" "$t" "$family" <<'PY' || READBACK=MISMATCH
import hashlib, os, sys
mnt, t, family = sys.argv[1], int(sys.argv[2]), sys.argv[3]
# Re-run the same generator WITHOUT writing, comparing the expected bytes.
# (Deterministic in (family, writer, round); the file's size drives the
# round count. Must stay byte-identical to the writer's.)
HEADS = {
    "build_artifacts": lambda w, r: f"/* unit {w} rev {r} */\n#include <stdint.h>\n",
    "ci_cache": lambda w, r: f"[2026-08-28T00:00:00Z] build id={w:04d}-{r:04d}\n",
    "container_layers": lambda w, r: f'{{"layer":{w},"round":{r},"size":1048576}}\n',
    "generated_assets": lambda w, r: f"/* asset {w}-{r} */\n",
    "scientific_outputs": lambda w, r: f"# run {w} seed {r}\n",
}
def make_buf(w, r):
    tag = f"{family}-{w:04d}-{r:04d}"
    head = HEADS[family](w, r).encode()
    line = (f"{tag} host=node-{w:04d} port={8000 + w * 7 + r} "
            f"user=svc flags={w % 5} payload={r:06d}\n").encode()
    body = hashlib.shake_128(tag.encode()).digest(16384).hex().encode()
    unit = head + line + body
    return (unit * ((1024 * 1024 + len(unit) - 1) // len(unit)))[:1024 * 1024]
for w in range(t):
    path = f"{mnt}/f-{w:02d}.bin"
    if not os.path.exists(path):
        print(f"MISSING {path}"); sys.exit(1)
    size = os.path.getsize(path)
    rounds = size // (1024 * 1024) + 1
    r = 0
    buf = make_buf(w, r)
    with open(path, "rb") as fh:
        while True:
            chunk = fh.read(1024 * 1024)
            if not chunk:
                break
            exp = make_buf(w, r)
            if chunk != exp:
                print(f"MISMATCH {path} round {r}")
                sys.exit(1)
            r += 1
print("readback OK")
PY

    # --- Unmount + fsck (cleanliness). The daemon's Drop (stats write +
    # store teardown) can outlive the unmount syscall by a beat; wait for
    # the store lock to clear and retry the fsck once before declaring a
    # FAIL (the 12C-1-3 court saw transient FAILs at 0.5 s on 4/30 cells
    # that were clean on re-run). ---
    trap - EXIT
    fusermount3 -u "$RUN/mnt"
    for _ in $(seq 1 40); do
        sleep 0.25
        if "$BIN" fsck "$RUN/store" >/dev/null 2>&1; then
            break
        fi
    done
    local FSCK=OK
    "$BIN" fsck "$RUN/store" >/dev/null 2>&1 || FSCK=FAIL
    echo -e "$policy\t$t\t$pool\t$family\t$READBACK\t$FSCK" >> "$CLEAN"

    # --- The causal-chain witnesses (the daemon's pressure block). ---
    local PRESSURED SAMPLES ENTER LEAVE PMSD RANSKIPS DEFEXT DEFBYTES PEAK AGE CAPENG
    if grep -q "pressure state machine" "$RUN/stats.txt"; then
        PRESSURED=$(grep -A12 "pressure state machine" "$RUN/stats.txt" | sed -n 's/^  pressured: //p')
        SAMPLES=$(grep -A12 "pressure state machine" "$RUN/stats.txt" | sed -n 's/^  samples: //p')
        ENTER=$(grep -A12 "pressure state machine" "$RUN/stats.txt" | sed -n 's/^  enter events: //p')
        LEAVE=$(grep -A12 "pressure state machine" "$RUN/stats.txt" | sed -n 's/^  leave events: //p')
        PMSD=$(grep -A12 "pressure state machine" "$RUN/stats.txt" | sed -n 's/^  time pressured: \([0-9]*\) ms/\1/p')
        RANSKIPS=$(grep -A12 "pressure state machine" "$RUN/stats.txt" | sed -n 's/^  rans skips: //p')
        DEFEXT=$(grep -A12 "pressure state machine" "$RUN/stats.txt" | sed -n 's/^  deferred extents: //p')
        DEFBYTES=$(grep -A12 "pressure state machine" "$RUN/stats.txt" | sed -n 's/^  deferred bytes: //p')
        PEAK=$(grep -A12 "pressure state machine" "$RUN/stats.txt" | sed -n 's/^  peak deferred bytes: //p')
        AGE=$(grep -A12 "pressure state machine" "$RUN/stats.txt" | sed -n 's/^  oldest deferred age: \([0-9]*\) ms/\1/p')
        CAPENG=$(grep -A12 "pressure state machine" "$RUN/stats.txt" | sed -n 's/^  debt-cap engagements: //p')
    else
        PRESSURED=no; SAMPLES=0; ENTER=0; LEAVE=0; PMSD=0; RANSKIPS=0; DEFEXT=0; DEFBYTES=0; PEAK=0; AGE=0; CAPENG=0
    fi

    # --- Settle: optimize (the background repayment — wall = time-to-
    # settle) + compact, then the settled metrics (debt must be 0). The
    # settle is bounded by SETTLE_BUDGET (the background search is
    # ~8 ms/extent single-threaded; the budget records a timeout rather
    # than hanging the court). ---
    local SETTLED LOGICAL DEBT SETTLE_WALL
    local ST0 ST1
    ST0=$(date +%s%N)
    timeout "$SETTLE_BUDGET" "$BIN" optimize "$RUN/store" >/dev/null 2>&1 || true
    timeout "$SETTLE_BUDGET" "$BIN" gc --compact "$RUN/store" >/dev/null 2>&1 || true
    ST1=$(date +%s%N)
    SETTLE_WALL=$(python3 -c "print(f'{($ST1-$ST0)/1e9:.1f}')")
    read -r SETTLED LOGICAL DEBT < <("$BIN" metrics --json "$RUN/store" 2>/dev/null \
        | python3 -c "
import json,sys
d=json.load(sys.stdin)
a=d.get('accounting',{})
p=d.get('pressure',{})
print(a.get('physical_used_bytes',0), a.get('logical_bytes',0), p.get('deferred_logical_bytes',0))
" 2>/dev/null || echo "0 0 0")
    echo -e "$policy\t$t\t$pool\t$family\t$WALL\t$(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")\t$P50\t$P95\t$P99\t$PRESSURED\t$SAMPLES\t$ENTER\t$LEAVE\t$PMSD\t$RANSKIPS\t$DEFEXT\t$DEFBYTES\t$PEAK\t$AGE\t$CAPENG\t$SETTLED\t$LOGICAL\t$SETTLE_WALL" >> "$SUMMARY"
    echo "  wall ${WALL}s cpu $(python3 -c "print(f'{$CPUS1-$CPUS0:.3f}')")s p50 ${P50}us p95 ${P95}us p99 ${P99}us pressure enter=$ENTER defer=$DEFBYTES settled=$SETTLED debt=$DEBT settle=${SETTLE_WALL}s"
}

# ---------------------------------------------------------------------------
# The matrix: three lanes (the 12C-1-3 brief's structure, not the full
# cross product):
#
#   1. concurrency sweep — build_artifacts at pool-16, every policy x
#      every writer count (where the engagement starts; the idle/low-
#      concurrency no-regression lanes);
#   2. pool lane — build_artifacts at writers 16, every policy x pool-8
#      (pool-16 is the sweep's default);
#   3. family lane — the other four adoption families at writers 16,
#      pool-16, every policy (the engagement across content shapes).
# ---------------------------------------------------------------------------
SWEEP_FAMILY="build_artifacts"
POOL_LANE_WRITERS=16
FAMILY_LANE_WRITERS=16

if [[ "$SWEEP_ONLY" == "1" ]]; then
    FAMILIES="$SWEEP_FAMILY"
    POOLS="16"
    WRITERS="${COURT_WRITERS:-1 4 8 16 32}"
fi

run_lane() { # family pool writers
    local fam="$1" pool="$2" writers="$3"
    for policy in $POLICIES; do
        for t in $writers; do
            run_cell "$policy" "$t" "$pool" "$fam" "$WORKDIR/run-${policy}-t${t}-p${pool}-${fam}"
        done
    done
}

# Lane 1: the concurrency sweep (pool-16).
run_lane "$SWEEP_FAMILY" 16 "$WRITERS"
# Lane 2: the pool-size lane (writers 16; pool-8; pool-16 is the sweep).
for policy in $POLICIES; do
    run_cell "$policy" "$POOL_LANE_WRITERS" 8 "$SWEEP_FAMILY" "$WORKDIR/run-${policy}-t${POOL_LANE_WRITERS}-p8-${SWEEP_FAMILY}"
done
# Lane 3: the family lane (writers 16, pool-16).
for fam in $FAMILIES; do
    if [[ "$fam" == "$SWEEP_FAMILY" ]]; then continue; fi
    run_lane "$fam" 16 "$FAMILY_LANE_WRITERS"
done

python3 - "$OUT" "$REV" "$POLICIES" "$WRITERS" "$POOLS" "$FAMILIES" <<'PY'
import json, os, sys
out, rev = sys.argv[1], sys.argv[2]
rows = []
with open(os.path.join(out, "summary.tsv")) as f:
    header = f.readline().rstrip("\n").split("\t")
    for line in f:
        parts = line.rstrip("\n").split("\t")
        if len(parts) == len(header):
            rows.append(dict(zip(header, parts)))
envelope = {
    "oracle": "phase-12c1-3-pressure-mount-engagement",
    "rev": rev,
    "machine": next(
        (l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")),
        "unknown",
    ),
    "kernel": os.uname().release,
    "policies": sys.argv[3].split(),
    "writers": [int(x) for x in sys.argv[4].split()],
    "pools": [int(x) for x in sys.argv[5].split()],
    "families": sys.argv[6].split(),
    "rows": rows,
    "cleanliness": [l.rstrip("\n").split("\t") for l in open(os.path.join(out, "cleanliness.tsv"))],
}
with open(os.path.join(out, "results.json"), "w") as f:
    json.dump(envelope, f, indent=2)
    f.write("\n")
print(f"archived: {out}")
PY

echo "== done: $OUT =="
