#!/usr/bin/env bash
# Phase-11B: the FUSE write-path request-reconciliation court (evidence-sealed).
#
# Mounts a fresh EntropyFS store at 1/2/4/8/16 FUSE threads (COURT_THREADS
# overridable) and drives each mount with a genuinely parallel write
# workload (T concurrent 1 MiB appends per file, T files, via the mount).
# The daemon's stats dump (--stats-file) now includes the Phase-11B request
# reconciliation: every write request partitioned into exclusive phases
# (inode_lock_wait, epoch_lock_wait, read_*, prepare, stage, commit_lock_wait,
# append, flush, epoch_wait, cp_*, barrier_*) with the identity
#
#     request latency == sum(phases) + residual
#
# The court asserts the identity per thread count (no OVERLAP flag, residual
# share below RECON_MAX_RESIDUAL) and archives:
#
#   evidence/performance/recon-court-<unix>-<rev>/
#     summary.tsv      threads, phase, total_ms, share (the stacked table)
#     identity.tsv     threads, requests, total_ms, residual_ms, residual_share, overlap
#     stats-t<N>.txt    the daemon's full stats dump per thread count
#     results.json     machine-readable summary
#
# Exit code: 0 iff every thread count reconciles (identity holds) AND the
# FUSE write workload is byte-exact (cmp against the source corpus).
#
# Usage: tools/recon-court.sh [WORKDIR] [OUTROOT]
#   Unprivileged (FUSE via fusermount3). Requires the release binary.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKDIR="${1:-$REPO_ROOT/target/recon-court-scratch}"
OUTROOT="${2:-$REPO_ROOT/evidence/performance}"
THREADS="${COURT_THREADS:-1 2 4 8 16}"
# Residual share (of request time) the court tolerates before it declares
# the write path "not accounted for".
RECON_MAX_RESIDUAL="${RECON_MAX_RESIDUAL:-0.15}"
BIN="$REPO_ROOT/target/release/entropyfs"

if [[ ! -x "$BIN" ]]; then
    echo "error: $BIN missing (cargo build --release)" >&2
    exit 1
fi
command -v fusermount3 >/dev/null || { echo "error: fusermount3 missing" >&2; exit 1; }

TS="$(date +%s)"
REV="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo norev)"
OUT="$OUTROOT/recon-court-$TS-$REV"
mkdir -p "$OUT" "$WORKDIR"
KERNEL="$(cat /proc/sys/kernel/osrelease 2>/dev/null || echo unknown)"
GOVERNOR="$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo unknown)"
CPU_MODEL="$(grep -m1 'model name' /proc/cpuinfo | sed 's/.*: //' || echo unknown)"
NPROC="$(nproc)"

SUMMARY="$OUT/summary.tsv"
IDENTITY="$OUT/identity.tsv"
echo -e "threads\tphase\ttotal_ms\tshare" > "$SUMMARY"
echo -e "threads\trequests\ttotal_ms\tresidual_ms\tresidual_share\toverlap\twall_s" > "$IDENTITY"

echo "=== EntropyFS Phase-11B reconciliation court ==="
echo "revision: $REV"
echo "timestamp: $TS"
echo "kernel: $KERNEL"
echo "governor: $GOVERNOR"
echo "cpu: $CPU_MODEL ($NPROC threads)"
echo "residual bar: $RECON_MAX_RESIDUAL"
echo "archive: $OUT"
echo

# Deterministic corpus: T files of 16 MiB (incompressible), written as
# 1 MiB appends. Fresh per thread count.
gen_corpus() {
    local d="$1" t="$2"
    rm -rf "$d"
    mkdir -p "$d"
    python3 - "$d" "$t" <<'PY'
import hashlib, os, sys
d, t = sys.argv[1], int(sys.argv[2])
# 16 distinct 16 MiB streams so the T concurrent writers never share a page.
for i in range(t):
    h = hashlib.shake_128(f"entropyfs-recon-court-v1-{i}".encode())
    with open(f"{d}/f{i}.bin", "wb") as f:
        f.write(h.digest(16 * 1024 * 1024))
print(f"corpus: {t} files x 16 MiB (shake_128, incompressible)")
PY
}

overlap_flags=()
for t in $THREADS; do
    echo "== court at threads=$t =="
    RUN="$WORKDIR/t$t"
    rm -rf "$RUN"
    mkdir -p "$RUN/mnt" "$RUN/corpus"
    gen_corpus "$RUN/corpus" "$t"

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

    # Parallel write: T concurrent copies of the T-file corpus (each cp is
    # one writer; the kernel delivers 1 MiB writeback requests).
    T0=$(date +%s%N)
    find "$RUN/corpus" -maxdepth 1 -name '*.bin' -print | sort \
        | xargs -P "$t" -I{} cp {} "$RUN/mnt/"
    T1=$(date +%s%N)
    WALL=$(python3 -c "print(f'{($T1-$T0)/1e9:.3f}')")

    # Byte-exactness: cmp every written file against the corpus.
    for f in "$RUN"/corpus/*.bin; do
        cmp "$f" "$RUN/mnt/$(basename "$f")" || { echo "error: byte mismatch $(basename "$f")" >&2; exit 1; }
    done

    # Unmount (the daemon drops and writes the stats dump, which now
    # contains the Phase-11B request reconciliation).
    fusermount3 -u "$RUN/mnt"
    wait "$DAEMON" 2>/dev/null || true
    trap - EXIT
    cp "$RUN/stats.txt" "$OUT/stats-t$t.txt"

    # Parse the reconciliation table + identity from the stats dump.
    python3 - "$t" "$RUN/stats.txt" "$SUMMARY" "$IDENTITY" "$WALL" "$RECON_MAX_RESIDUAL" <<'PY'
import re, sys
t, stats, summary, identity, wall, bar = (
    int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4], float(sys.argv[5]), float(sys.argv[6]))
txt = open(stats).read()
# The reconciliation block:
m = re.search(r"request reconciliation \(n=(\d+) requests, ([\d.]+) ms total\):\n(.*?)(?=\n\S|\Z)", txt, re.S)
if not m:
    print(f"  ERROR threads={t}: no reconciliation block in stats dump"); sys.exit(1)
n_req, total_ms = int(m.group(1)), float(m.group(2))
body = m.group(3)
rows = []
residual_ms = residual_share = None
for line in body.splitlines():
    line = line.strip()
    if not line or line.startswith("phase"):
        continue
    parts = line.split()
    # rows: "phase  total ms  share%"  (unaccounted row has a trailing comment)
    if len(parts) < 3:
        continue
    phase = parts[0]
    total = float(parts[1])
    share = float(parts[2].rstrip('%')) / 100.0
    if phase == "total":
        continue
    rows.append((phase, total, share))
    if phase == "unaccounted":
        residual_ms, residual_share = total, share
if residual_ms is None:
    print(f"  ERROR threads={t}: residual row missing"); sys.exit(1)
overlap = "OVERLAP" in txt[re.search(r"sum\(phases\)", txt).start():]
ok = (not overlap) and residual_share < bar
with open(summary, "a") as f:
    for phase, total, share in rows:
        f.write(f"{t}\t{phase}\t{total:.2f}\t{share:.3f}\n")
with open(identity, "a") as f:
    f.write(f"{t}\t{n_req}\t{total_ms:.2f}\t{residual_ms:.2f}\t{residual_share:.4f}\t{overlap}\t{wall}\n")
print(f"  threads={t}: n={n_req} requests total={total_ms:.0f} ms residual={residual_ms:.0f} ms "
      f"({residual_share*100:.1f}%) wall={wall}s -> {'RECONCILED' if ok else 'FAILED'}")
if not ok:
    print(f"  ERROR threads={t}: identity violated (overlap={overlap} residual_share={residual_share:.3f} bar={bar})")
    sys.exit(1)
PY
    if [[ $? -ne 0 ]]; then
        exit 1
    fi
done

echo
echo "== Phase-11B stacked accounting (request time shares) =="
column -t -s$'\t' "$SUMMARY"
echo
echo "== identity =="
column -t -s$'\t' "$IDENTITY"
echo

# Machine-readable results.
python3 - "$OUT" "$REV" "$KERNEL" "$GOVERNOR" "$CPU_MODEL" "$NPROC" "$TS" <<'PY'
import csv, json, sys
out, rev, kernel, gov, cpu, nproc, ts = sys.argv[1:]
identity = []
with open(f"{out}/identity.tsv") as f:
    for row in csv.DictReader(f, delimiter="\t"):
        identity.append({
            "threads": int(row["threads"]),
            "requests": int(row["requests"]),
            "total_ms": float(row["total_ms"]),
            "residual_ms": float(row["residual_ms"]),
            "residual_share": float(row["residual_share"]),
            "overlap": row["overlap"] == "True",
            "wall_s": float(row["wall_s"]),
        })
rows = []
with open(f"{out}/summary.tsv") as f:
    for row in csv.DictReader(f, delimiter="\t"):
        rows.append({
            "threads": int(row["threads"]),
            "phase": row["phase"],
            "total_ms": float(row["total_ms"]),
            "share": float(row["share"]),
        })
result = {
    "timestamp_unix": int(ts),
    "revision": rev,
    "kernel": kernel,
    "governor": gov,
    "cpu_model": cpu,
    "cpu_count": int(nproc),
    "identity": identity,
    "phases": rows,
    "admitted": all(not r["overlap"] and r["residual_share"] < 0.15 for r in identity),
}
with open(f"{out}/results.json", "w") as f:
    json.dump(result, f, indent=2)
    f.write("\n")
print(f"evidence written: {out}/")
PY

echo "=== court complete ==="
