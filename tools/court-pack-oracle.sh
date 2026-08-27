#!/usr/bin/env bash
# Phase 12E.12 — the physical small-object packing oracle (driver).
#
# # PURPOSE
#
# The 12E.12 brief: decompose the PHYSICAL cost of a realistic small-file
# tree and implement Physical Object Packs only if the oracle proves a
# meaningful real small-file win. The oracle (`src/tests/pack_oracle.rs`)
# builds the brief's exact corpus classes (tiny source files, headers,
# configs, package metadata, 1–16 KiB, 16–64 KiB), checkpoint-settles it,
# and decomposes physical cost: live bytes per RecordTag (Data/Model/
# Inode/BtreeNode/Root), record envelopes, padding/format, dead before
# and after `compact_full`. This driver archives the sealed evidence.
#
# # BOUNDARY
#
# KNOWS: how to invoke the oracle and archive its output. NEVER KNOWS:
# the store, the corpus bytes, or any policy. It changes NO production
# code and NO persistent format — the brief forbids a pack format without
# a proven win, and this oracle produces that decision.
#
# # GATE (normative, from the brief + the oracle's module doc)
#
# - packable (Data+Model) envelope share >= 20% AND
#   structural (trees+inodes+roots) + packable envelope >= 30% of
#   physical used after compaction
#     -> PACK-CANDIDATE (a format-bit investigation would be justified)
# - otherwise -> REJECT-PACKS (record the numbers)
#
# # USAGE
#
#     tools/court-pack-oracle.sh [OUTROOT]
#
# Requires: bash, cargo, python3. Archives under
# `evidence/performance/pack-oracle-<ts>-<rev>/` with the sealed
# `evidence-manifest.json` (12E.5), the raw run log, and
# `results.json`.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTROOT="${1:-$REPO_ROOT/evidence/performance}"
BUILD_DIR="${COURT_WORKTREE:-$REPO_ROOT}"

TS="$(date +%s)"
REV="$(git -C "$BUILD_DIR" rev-parse --short HEAD)"
OUT="$OUTROOT/pack-oracle-$TS-$REV"
mkdir -p "$OUT"

echo "== phase-12E.12 pack oracle: rev=$REV =="

cargo test --release --lib pack_oracle -- --nocapture 2>&1 | tee "$OUT/run.log"

if ! grep -q "test result: ok" "$OUT/run.log"; then
    echo "error: oracle did not pass; no evidence archived" >&2
    exit 1
fi

grep -m1 '^PACK_ORACLE ' "$OUT/run.log" | sed 's/^PACK_ORACLE //' > "$OUT/result.json"

BIN="$REPO_ROOT/target/release/entropyfs"
if [[ -x "$BIN" ]]; then
    "$BIN" evidence-manifest "$OUT/evidence-manifest.json" \
        --store "$OUT" --io-backend sync --worker-scheduler pool \
        --court-schema-version 2 >/dev/null 2>&1 \
        && echo "manifest: $OUT/evidence-manifest.json" \
        || echo "manifest FAILED (binary built without evidence-manifest?)"
else
    echo "manifest skipped: no release binary at $BIN"
fi

python3 - "$OUT" "$REV" <<'PY'
import json, os, sys
out, rev = sys.argv[1], sys.argv[2]
with open(os.path.join(out, "result.json")) as f:
    r = json.load(f)
envelope = {
    "oracle": "phase-12e12-pack-oracle",
    "schema": r.get("schema"),
    "rev": rev,
    "machine": next(
        (l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")),
        "unknown",
    ),
    "kernel": os.uname().release,
    "corpus": r.get("corpus"),
    "before": r.get("before"),
    "after": r.get("after"),
    "ratios": r.get("ratios"),
    "decision": r.get("decision"),
}
with open(os.path.join(out, "results.json"), "w") as f:
    json.dump(envelope, f, indent=2)
    f.write("\n")
print(f"archived: {out}")
PY

echo "== decision: $(python3 -c "import json; print(json.load(open('$OUT/result.json'))['decision']['verdict'])") =="
echo "== done: $OUT =="
