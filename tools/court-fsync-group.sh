#!/usr/bin/env bash
# Phase-12B durability-group court: archive one fsync-group probe run.
#
# The oracle (src/tests/fsync_group_probe.rs) drives concurrent
# write+fsync loops and reports the barrier amplification and the fsync
# latency convoy. Two runs are archived:
#
#   before: mode=baseline at the 12B-0 commit (amplification ~1.00 —
#           every fsync ran its own physical barrier)
#   after:  mode=group at the 0.7.9 release (amplification << 1 at high
#           concurrency — the group gate coalesces generations)
#
# The gate (the brief): barrier amplification =
# physical durability barriers / logical fsyncs must fall well below 1
# under concurrency, fsync tail latency must not regress, and the crash
# court (src/tests/durability_group_crash.rs) must stay green.
#
# Usage: tools/court-fsync-group.sh <baseline|group> [OUTROOT]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
MODE="${1:?usage: tools/court-fsync-group.sh <baseline|group> [OUTROOT]}"
OUTROOT="${2:-$REPO_ROOT/evidence/performance}"
BUILD_DIR="${COURT_WORKTREE:-$REPO_ROOT}"

case "$MODE" in
  baseline|group) ;;
  *) echo "error: mode must be baseline or group" >&2; exit 1 ;;
esac

TS="$(date +%s)"
REV="$(git -C "$BUILD_DIR" rev-parse --short HEAD)"
OUT="$OUTROOT/fsync-group-probe-$MODE-$TS-$REV"
mkdir -p "$OUT"
SUMMARY="$OUT/summary.tsv"

echo "== phase-12B fsync-group court: mode=$MODE rev=$REV =="

FSYNC_GROUP_MODE="$MODE" \
FSYNC_GROUP_OUT="$SUMMARY" \
  cargo test --release --lib fsync_group_probe -- --nocapture 2>&1 \
  | tee "$OUT/run.log"

if ! grep -q "test result: ok" "$OUT/run.log"; then
  echo "error: probe did not pass; no evidence archived" >&2
  exit 1
fi

python3 - "$OUT" "$MODE" "$REV" <<'PY'
import json, os, sys
out, mode, rev = sys.argv[1], sys.argv[2], sys.argv[3]
rows = []
with open(os.path.join(out, "summary.tsv")) as f:
    header = f.readline().rstrip("\n").split("\t")
    for line in f:
        parts = line.rstrip("\n").split("\t")
        if len(parts) == len(header):
            rows.append(dict(zip(header, parts)))
envelope = {
    "oracle": "phase-12b-fsync-group",
    "mode": mode,
    "rev": rev,
    "machine": next(
        (l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")),
        "unknown",
    ),
    "kernel": os.uname().release,
    "workload": "concurrent write+fsync loops (1/2/4/8/16/32 writers x 16 cycles, distinct 64 KiB content), amplification = physical barriers / fsync requests, fsync latency p50/p95/p99, commit_lock_wait cumulative",
    "rows": rows,
}
with open(os.path.join(out, "results.json"), "w") as f:
    json.dump(envelope, f, indent=2)
    f.write("\n")
print(f"archived: {out}")
PY

echo "== done: $OUT =="
