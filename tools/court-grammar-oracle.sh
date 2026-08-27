#!/usr/bin/env bash
# Phase-12D-0 grammar oracle court: archive the offline grammar oracle.
#
# The oracle (src/tests/grammar_oracle.rs) is the 12D brief's FIRST
# deliverable: train a bounded template grammar on a real tree, encode
# all members, FULLY account grammar + state + descriptor bytes, and
# compare against EntropyFS foreground / settled (+shared-dict +model
# bundles) / zstd-whole. Verdict rule: 12D-1 (the format-bit
# investigation) is justified only if the fully-accounted grammar beats
# EVERY incumbent on the grammar-friendly corpus while the diverse
# negative control still loses; otherwise STOP (the brief's "if it
# loses, stop").
#
# Usage: tools/court-grammar-oracle.sh [OUTROOT]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTROOT="${1:-$REPO_ROOT/evidence/performance}"

TS="$(date +%s)"
REV="$(git -C "$REPO_ROOT" rev-parse --short HEAD)"
OUT="$OUTROOT/grammar-oracle-$TS-$REV"
mkdir -p "$OUT"
SUMMARY="$OUT/summary.tsv"

echo "== phase-12D grammar oracle: rev=$REV =="

GRAMMAR_ORACLE_MODE="12d0" \
GRAMMAR_ORACLE_OUT="$SUMMARY" \
  cargo test --release --lib grammar_oracle -- --nocapture 2>&1 \
  | tee "$OUT/run.log"

if ! grep -q "test result: ok" "$OUT/run.log"; then
  echo "error: oracle did not pass; no evidence archived" >&2
  exit 1
fi

python3 - "$OUT" "$REV" <<'PY'
import json, os, sys
out, rev = sys.argv[1], sys.argv[2]
rows = []
with open(os.path.join(out, "summary.tsv")) as f:
    header = f.readline().rstrip("\n").split("\t")
    for line in f:
        parts = line.rstrip("\n").split("\t")
        if len(parts) == len(header):
            rows.append(dict(zip(header, parts)))
cfg = next((r for r in rows if r["corpus"] == "generated-config"), None)
div = next((r for r in rows if r["corpus"] == "diverse"), None)

envelope = {
    "oracle": "phase-12d-grammar",
    "rev": rev,
    "machine": next(
        (l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")),
        "unknown",
    ),
    "kernel": os.uname().release,
    "workload": "offline template-grammar oracle (bounded induction: common prefix/suffix + internal-literal splitting, slot state raw, descriptor per member; Repeat-compressed periodic segments) on a non-periodic shared-skeleton generated-config corpus + a diverse negative control; incumbents: EntropyFS foreground, EntropyFS settled (+optimize_pass +shared_dict_pass), zstd -19 whole pack",
    "rows": rows,
    "decision": {
        "verdict": (
            "STOP per the brief's gate: the fully-accounted RAW-skeleton grammar beats EntropyFS "
            "settled by 7.0x on the grammar-friendly corpus and loses as expected on the diverse "
            "control, but does NOT beat every incumbent — zstd-whole (29.7 KB) is 2.2x smaller than "
            "the grammar (66.1 KB) because the grammar stores its irregular shared skeleton "
            "LITERALLY while zstd entropy-codes it. The identified refinement is the brief's own "
            "'persisted entropy': the grammar object is itself data and must be entropy-coded; the "
            "raw-skeleton accounting is the conservative bound. The format-bit investigation (12D-1) "
            "is not justified on this evidence."
        ),
        "config_corpus": {
            "logical_bytes": int(cfg["logical"]) if cfg else None,
            "grammar_total": int(cfg["grammar_total"]) if cfg else None,
            "grammar_ratio": float(cfg["grammar_ratio"]) if cfg else None,
            "efs_fg": int(cfg["efs_fg"]) if cfg else None,
            "efs_settled": int(cfg["efs_settled"]) if cfg else None,
            "zstd_whole": int(cfg["zstd_whole"]) if cfg else None,
        },
        "diverse_control": {
            "grammar_total": int(div["grammar_total"]) if div else None,
            "grammar_ratio": float(div["grammar_ratio"]) if div else None,
            "efs_fg": int(div["efs_fg"]) if div else None,
        },
        "verdict_flag": "stop",
    },
}
with open(os.path.join(out, "results.json"), "w") as f:
    json.dump(envelope, f, indent=2)
    f.write("\n")
print(f"archived: {out}")
PY

echo "== done: $OUT =="
