#!/usr/bin/env bash
# Phase 12C-1 — the adaptive-foreground-budget court (driver).
#
# # PURPOSE
#
# The 12C-1 brief: can EntropyFS preserve most of the 10–20× adoption
# wedge (12E.13) while spending dramatically less foreground search CPU?
# The court measures the cost–density FRONTIER (full / cheap / raw — the
# sealed 12E.13 replay, the 10B entropy-probe skip, and the no-search
# control) plus the 12C-1-1 adaptive budget (`focused` — the entropy
# probe + the semantic class-prior rANS deferral) on the sealed adoption
# corpora and two controls (noise, mixed classes). The oracle drives the
# real store through the engine's own put protocol (content-id names,
# fast-dedup lookup, tmp-write-rename) so the `full` arm is byte-
# comparable to the sealed 12E.13 rows.
#
# # GATE (normative; applied by the writeup to the JSON rows)
#
# ```text
# on the adoption-wedge workloads, for the adopted arm vs `full`:
#     put wall        >= 2x (ideally much more)
#     search CPU      materially improved
#     settled bytes   regression <= 5%
#     byte identity   absolute (asserted by the oracle)
#     p99             no material regression (<= 5%)
#     raw controls    unchanged (noise-control rows)
# ```
#
# # BOUNDARY
#
# KNOWS: how to invoke the oracle (`src/tests/adaptive_budget_probe.rs`)
# and archive its output. NEVER KNOWS: the store, the policy parameters,
# or any production code — it changes none.
#
# # USAGE
#
#     tools/court-adaptive-budget.sh [OUTROOT]
#
# Requires: bash, cargo, python3. Archives under
# `evidence/performance/adaptive-budget-probe-<ts>-<rev>/`.
#
# The settle passes (background optimizer + shared-dict pass) are part of
# the measurement — the frontier's "background recovers density later"
# finding depends on them. Quick smoke runs can set
# `ADAPTIVE_BUDGET_SETTLE=0` (then only the GC settle is measured).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTROOT="${1:-$REPO_ROOT/evidence/performance}"
BUILD_DIR="${COURT_WORKTREE:-$REPO_ROOT}"

TS="$(date +%s)"
REV="$(git -C "$BUILD_DIR" rev-parse --short HEAD)"
OUT="$OUTROOT/adaptive-budget-probe-$TS-$REV"
mkdir -p "$OUT"

echo "== phase-12C-1 adaptive-budget court: rev=$REV =="

ADAPTIVE_BUDGET_OUT="$OUT/result.json" \
    cargo test --release --lib adaptive_budget_probe -- --nocapture 2>&1 | tee "$OUT/run.log"

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
    "oracle": "phase-12c1-adaptive-budget-court",
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
}
with open(os.path.join(out, "results.json"), "w") as f:
    json.dump(envelope, f, indent=2)
    f.write("\n")
print(f"archived: {out}")
PY

echo "== done: $OUT =="
