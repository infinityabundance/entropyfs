#!/usr/bin/env bash
# Phase-10A: the FUSE event-loop thread sweep. Runs the sealed competitive
# filesystem court at 1/2/4/8/16 FUSE threads (background optimizer
# disabled for the foreground section) and produces a summary table of
# the EntropyFS rows: buffered/durable write, warm/cold read, daemon CPU
# utilization, and the settled density — the answer to "how much
# performance is already sitting unused in the one-thread default".
#
# Usage: tools/court-threads.sh [WORKDIR] [OUTROOT]
#   Requires the same root-capable environment as run-court-docker.sh
#   (privileged docker for the loop images), or a root shell.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKDIR="${1:-$REPO_ROOT/target/court-threads-scratch}"
OUTROOT="${2:-$REPO_ROOT/evidence/performance}"
THREADS="${COURT_THREADS:-1 2 4 8 16}"

if [[ ! -x "$REPO_ROOT/target/release/entropyfs" ]]; then
    echo "error: target/release/entropyfs missing (cargo build --release)" >&2
    exit 1
fi

SUMMARY="$OUTROOT/court-threads-summary-$(date +%s).tsv"
echo -e "threads\tcorpus\tbuffered_mbps\tdurable_mbps\twarm_read_mbps\tcold_read_mbps" > "$SUMMARY"

for t in $THREADS; do
    echo "== court at threads=$t =="
    SCRATCH="$WORKDIR/t$t"
    rm -rf "$SCRATCH"   # fresh scratch per run (no stale corpus artifacts)
    mkdir -p "$SCRATCH"
    COURT_FUSE_THREADS="$t" "$SCRIPT_DIR/fs-court.sh" "$SCRATCH" "$OUTROOT" | tee "$WORKDIR/run-t$t.log" | tail -1
    # Extract the newest fs-court archive's EntropyFS rows.
    NEWEST=$(ls -dt "$OUTROOT"/fs-court-* 2>/dev/null | head -1)
    python3 - "$NEWEST" "$t" "$SUMMARY" <<'EOF'
import json, os, sys
out, t, summary = sys.argv[1], sys.argv[2], sys.argv[3]
r = json.load(open(f"{out}/results.json"))
efs = r.get("entropyfs", {})
with open(summary, "a") as f:
    for c in ("src", "random.bin", "zeros.bin", "compressed.tgz"):
        row = efs.get("entropyfs/" + c, {})
        if not isinstance(row, dict):
            continue
        f.write(f"{t}\t{c}\t{row.get('buffered_write_mbps','')}\t{row.get('durable_write_mbps','')}\t{row.get('warm_read_mbps','')}\t{row.get('cold_read_mbps','')}\n")
    cpu = {k: v for k, v in efs.items() if k.startswith("daemon_cpu")}
    if cpu:
        for k, v in cpu.items():
            if isinstance(v, dict):
                f.write(f"{t}\t{k}\tutilization={v.get('utilization','')}\tcpu={v.get('cpu_secs','')}s\n")
    settled = efs.get("settled", {})
    if isinstance(settled, dict):
        f.write(f"{t}\tsettled_density\t{settled.get('settled_density','')}\tsettle_elapsed={settled.get('settle_elapsed_s','')}s\n")
EOF
done

echo
echo "== Phase-10A FUSE thread sweep summary =="
column -t -s$'\t' "$SUMMARY"
echo
echo "summary: $SUMMARY"
