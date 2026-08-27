#!/usr/bin/env bash
# Phase 12C-1-2 — the pressure-aware foreground deferral court (driver).
#
# # PURPOSE
#
# The 12C-1-2 brief: can EntropyFS spend the expensive rANS/representation
# search when the machine has capacity but DEFER that work to the
# background optimizer when the foreground path is under real execution
# pressure — without materially changing settled density? The oracle
# (`src/tests/pressure_deferral_probe.rs`) runs the sealed 12E.13
# adoption corpora + the shared noise control through the engine's put
# protocol under a deterministic pressure matrix (Full / Focused / P25 /
# P50 / P75 / RawOnly at P=0.9), the hysteresis + condition lanes
# (idle / pressured / oscillating / clearing with enter 0.80 / leave
# 0.60), the oscillation flap contrast (plain threshold vs hysteresis),
# and the starvation lane (a 2 MiB debt cap under sustained pressure).
#
# # GATE (normative; applied by the writeup to the JSON rows)
#
# ```text
# byte exactness        absolute (asserted by the oracle)
# settled density       <= +1% preferred, <= +5% hard reject
# 10x wedge             retained wherever Full had it
# foreground wall       >= 2x where the 12C-1 frontier says possible,
#                        else >= 70% of the measured available headroom
# search CPU            >= 70% of the RawOnly-vs-Full removable
#                        opportunity captured under pressure
# p99                   materially improved or neutral
# background convergence all deferred debt settles
# starvation            no unbounded debt growth
# raw controls          unchanged
# ```
#
# # BOUNDARY
#
# KNOWS: how to invoke the oracle and archive its output. NEVER KNOWS:
# the store, the policy parameters, or any production code — it changes
# none.
#
# # USAGE
#
#     tools/court-pressure-deferral.sh [OUTROOT]
#
# Requires: bash, cargo, python3. Archives under
# `evidence/performance/pressure-deferral-probe-<ts>-<rev>/`.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTROOT="${1:-$REPO_ROOT/evidence/performance}"
BUILD_DIR="${COURT_WORKTREE:-$REPO_ROOT}"

TS="$(date +%s)"
REV="$(git -C "$BUILD_DIR" rev-parse --short HEAD)"
OUT="$OUTROOT/pressure-deferral-probe-$TS-$REV"
mkdir -p "$OUT"

echo "== phase-12C-1-2 pressure-deferral court: rev=$REV =="

PRESSURE_DEFERRAL_OUT="$OUT/result.json" \
    cargo test --release --lib pressure_deferral_probe -- --nocapture 2>&1 | tee "$OUT/run.log"

if ! grep -q "test result: ok" "$OUT/run.log"; then
    echo "error: oracle did not pass; no evidence archived" >&2
    exit 1
fi

BIN="$REPO_ROOT/target/release/entropyfs"
if [[ -x "$BIN" ]]; then
    "$BIN" evidence-manifest "$OUT/evidence-manifest.json" \
        --store "$OUT" --io-backend sync --worker-scheduler pool \
        --court-schema-version 2 >/dev/null 2>&1 \
        && echo "manifest: $OUT/evidence-manifest.json" \
        || echo "manifest FAILED"
else
    echo "manifest skipped: no release binary at $BIN"
fi

python3 - "$OUT" "$REV" <<'PY'
import json, os, sys
out, rev = sys.argv[1], sys.argv[2]
with open(os.path.join(out, "result.json")) as f:
    r = json.load(f)
envelope = {
    "oracle": "phase-12c1-2-pressure-deferral",
    "schema": r.get("schema"),
    "rev": rev,
    "machine": next(
        (l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")),
        "unknown",
    ),
    "kernel": os.uname().release,
    "arms": r.get("arms"),
    "gate": r.get("gate"),
    "workloads": r.get("workloads"),
    "condition_lanes": r.get("condition_lanes"),
    "oscillation_contrast": r.get("oscillation_contrast"),
    "starvation": r.get("starvation"),
}
with open(os.path.join(out, "results.json"), "w") as f:
    json.dump(envelope, f, indent=2)
    f.write("\n")
print(f"archived: {out}")
PY

echo "== done: $OUT =="
