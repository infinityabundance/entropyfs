#!/usr/bin/env bash
# Phase 12E.13 — the object-store adoption court (driver).
#
# # PURPOSE
#
# The 12E.13 brief: benchmark and verify the embeddable immutable-object
# engine through its STABLE facade (`Engine::put_blob/get_blob/
# read_blob_range/sync/compact/metrics`) — NOT FUSE — across natural
# immutable-object workloads (versioned build artifacts, incremental
# source trees, container-like layers, near-duplicate generated assets,
# CI/cache-style object sets, versioned scientific outputs), against a
# raw-file baseline on the same device. The purpose is to DISCOVER an
# adoption wedge; "no compelling 10× pain-point win found yet" is a
# valid conclusion. This driver archives the sealed evidence.
#
# # BOUNDARY
#
# KNOWS: how to invoke the oracle (`src/tests/adoption_oracle.rs`) and
# archive its output. NEVER KNOWS: the store, the corpus bytes, or any
# policy. It changes NO production code.
#
# # GATE (normative, from the brief + the oracle's module doc)
#
# - any workload with footprint_vs_raw (settled physical / logical)
#   <= 0.10 -> WEDGE-CANDIDATE (the adoption story starts there);
# - otherwise -> NO-10X-WEDGE recorded as the valid conclusion.
#
# # USAGE
#
#     tools/court-adoption.sh [OUTROOT]
#
# Requires: bash, cargo, python3. Archives under
# `evidence/performance/adoption-oracle-<ts>-<rev>/`.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTROOT="${1:-$REPO_ROOT/evidence/performance}"
BUILD_DIR="${COURT_WORKTREE:-$REPO_ROOT}"

TS="$(date +%s)"
REV="$(git -C "$BUILD_DIR" rev-parse --short HEAD)"
OUT="$OUTROOT/adoption-oracle-$TS-$REV"
mkdir -p "$OUT"

echo "== phase-12E.13 adoption court: rev=$REV =="

cargo test --release --lib adoption_oracle -- --nocapture 2>&1 | tee "$OUT/run.log"

if ! grep -q "test result: ok" "$OUT/run.log"; then
    echo "error: oracle did not pass; no evidence archived" >&2
    exit 1
fi

grep -m1 '^ADOPTION_ORACLE ' "$OUT/run.log" | sed 's/^ADOPTION_ORACLE //' > "$OUT/result.json"

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
    "oracle": "phase-12e13-adoption-court",
    "schema": r.get("schema"),
    "rev": rev,
    "machine": next(
        (l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")),
        "unknown",
    ),
    "kernel": os.uname().release,
    "workloads": r.get("workloads"),
    "decision": r.get("decision"),
}
with open(os.path.join(out, "results.json"), "w") as f:
    json.dump(envelope, f, indent=2)
    f.write("\n")
print(f"archived: {out}")
PY

echo "== decision: $(python3 -c "import json; print(json.load(open('$OUT/result.json'))['decision']['verdict'])") =="
echo "== done: $OUT =="
