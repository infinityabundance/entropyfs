#!/usr/bin/env bash
# Phase-11F DSFB-observer shard court: archive one probe run as evidence.
#
# The 11F oracle compares the SAME probe (src/tests/dsfb_shard_probe.rs)
# at two commits:
#
#   before:  pool-16 + single-mutex StorageObserver  (rev = the 11F-0 commit)
#   after:   pool-16 + ShardedStorageObserver        (rev = the 0.7.7 release)
#
# This tool runs the probe once at the CURRENT tree and archives the
# sealed evidence dir:
#
#   evidence/performance/dsfb-shard-probe-<mode>-<ts>-<rev>/
#     run.log      full cargo test output (the raw receipt)
#     summary.tsv  the probe's oracle rows (written by the probe itself)
#     results.json machine/rev/workload + the row table (this script)
#
# Usage:
#   tools/court-dsfb-shard.sh <mode> [OUTROOT]
#     mode: mutex | sharded   (stamped into the summary header + dir name)
#     env:  COURT_WORKTREE    path to an alternate checkout to build/run in
#                             (for the mutex side: a worktree at the 11F-0
#                             commit, so the "before" binary really is the
#                             single-mutex observer)
#
# The before/after comparison and the decision live in the AFTER dir's
# results.json (written by the same discipline as the mount court's
# write_decision python below); this script only archives one side.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
MODE="${1:?usage: tools/court-dsfb-shard.sh <mutex|sharded> [OUTROOT]}"
OUTROOT="${2:-$REPO_ROOT/evidence/performance}"
BUILD_DIR="${COURT_WORKTREE:-$REPO_ROOT}"

case "$MODE" in
  mutex|sharded) ;;
  *) echo "error: mode must be mutex or sharded" >&2; exit 1 ;;
esac

TS="$(date +%s)"
REV="$(git -C "$BUILD_DIR" rev-parse --short HEAD)"
OUT="$OUTROOT/dsfb-shard-probe-$MODE-$TS-$REV"
mkdir -p "$OUT"
SUMMARY="$OUT/summary.tsv"

echo "== phase-11F dsfb-shard court: mode=$MODE rev=$REV build_dir=$BUILD_DIR =="

# Run the probe in release; the probe writes the TSV itself (it creates
# the parent dir). The full output is the raw receipt.
DSFB_PROBE_MODE="$MODE" \
DSFB_PROBE_OUT="$SUMMARY" \
  cargo test --release --lib dsfb_shard_probe -- --nocapture 2>&1 \
  | tee "$OUT/run.log"

# The receipt must show the probe passing and the summary must exist.
if ! grep -q "test result: ok" "$OUT/run.log"; then
  echo "error: probe did not pass; no evidence archived" >&2
  exit 1
fi
if [[ ! -s "$SUMMARY" ]]; then
  echo "error: probe summary missing" >&2
  exit 1
fi

# results.json: the archive envelope (rows copied from the summary).
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
env = {}
for line in open("/proc/cpuinfo"):
    if line.startswith("model name"):
        env["cpu"] = line.split(":", 1)[1].strip()
        break
kernel = os.uname().release
envelope = {
    "probe": "phase-11f-dsfb-observer-shard",
    "mode": mode,
    "rev": rev,
    "machine": env.get("cpu", "unknown"),
    "kernel": kernel,
    "workload": "pool-16; epoch_write sweeps (1/8/16 writers x 64 files; stress 16 writers x 256 files = 16k chunks, 1 GiB); per-write-distinct LCG content; byte-exact read-back + checkpoint + logical-identity + reachable-bytes + family histogram per run",
    "rows": rows,
}
with open(os.path.join(out, "results.json"), "w") as f:
    json.dump(envelope, f, indent=2)
    f.write("\n")
print(f"archived: {out}")
PY

echo "== done: $OUT =="
