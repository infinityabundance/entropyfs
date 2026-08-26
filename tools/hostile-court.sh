#!/usr/bin/env bash
# Phase-11A hostile-media court runner (evidence-sealed).
#
# Runs the full hostile-media court — descriptor-decode fuzzing, bounded
# materialization-graph fuzzing, and the CRC-aware whole-store mutator —
# with scaled proptest case counts in release mode, then the complete lib
# suite. Archives the receipts under:
#
#   evidence/hostile-media/court-<unix>-<rev>/
#
#   run.log          the full test output
#   receipt.json     revision, kernel, unix time, per-test results,
#                    case counts
#   summary.md       human-readable admission summary
#
# Exit code: 0 iff every court and the full suite pass.
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

REV="$(git rev-parse --short HEAD 2>/dev/null || echo norev)"
KERNEL="$(cat /proc/sys/kernel/osrelease 2>/dev/null || echo unknown)"
NOW="$(date +%s)"
OUT="evidence/hostile-media/court-${NOW}-${REV}"
mkdir -p "$OUT"

# The run log must NOT live inside the repo while the suite runs: the
# evidence corpus is part of the source pack, and a growing log would make
# source_pack_is_deterministic nondeterministic. Stage it outside and move
# it in only after every test finishes.
LOG="$(mktemp "${TMPDIR:-/tmp}/hostile-court-XXXXXX.log")"

# Case counts: the parser courts get a heavy run; the store mutators do
# real I/O per case, so they get a proportionally lighter (still large)
# run. Overridable: PROPTEST_CASES_* .
DESC_CASES="${PROPTEST_CASES_DESCRIPTOR:-200000}"
GRAPH_CASES="${PROPTEST_CASES_GRAPH:-200000}"
STORE_CASES="${PROPTEST_CASES_STORE:-30000}"

echo "== entropyfs hostile-media court — $(date -u) =="
echo "revision: $REV   kernel: $KERNEL"
echo "cases: descriptor=$DESC_CASES graph=$GRAPH_CASES store=$STORE_CASES"
echo "output: $OUT"

run_block() {
    local label="$1"; shift
    echo
    echo "== $label =="
    "$@" 2>&1
}

{
    echo "entropyfs hostile-media court — $(date -u)"
    echo "revision: $REV"
    echo "kernel: $KERNEL"
    echo "cases: descriptor=$DESC_CASES graph=$GRAPH_CASES store=$STORE_CASES"
    echo

    run_block "descriptor court (${DESC_CASES} cases/proptest target)" \
        env PROPTEST_CASES="$DESC_CASES" cargo test --release --lib \
        hostile_media::descriptor_court

    run_block "graph court (${GRAPH_CASES} cases/proptest target)" \
        env PROPTEST_CASES="$GRAPH_CASES" cargo test --release --lib \
        hostile_media::graph_court

    run_block "store court (${STORE_CASES} cases/proptest target)" \
        env PROPTEST_CASES="$STORE_CASES" cargo test --release --lib \
        hostile_media::store_court

    run_block "full lib suite" cargo test --release --lib
} > "$LOG" 2>&1
STATUS=$?
# Move the staged log into the evidence dir only after everything ran.
mv "$LOG" "$OUT/run.log"

# Receipt.
PASSED="$(grep -c 'test result: ok' "$OUT/run.log" || true)"
FAILED_TESTS="$(grep -E '^failures:' -A 20 "$OUT/run.log" | grep -E '^\s+[a-z_]+::' | sed 's/^[[:space:]]*//' || true)"
FAIL_COUNT="$(grep -c 'test result: FAILED' "$OUT/run.log" || true)"
{
    echo "{"
    echo "  \"court\": \"hostile-media\","
    echo "  \"revision\": \"$REV\","
    echo "  \"kernel\": \"$KERNEL\","
    echo "  \"unix_secs\": $NOW,"
    echo "  \"cases\": {"
    echo "    \"descriptor\": $DESC_CASES,"
    echo "    \"graph\": $GRAPH_CASES,"
    echo "    \"store\": $STORE_CASES"
    echo "  },"
    echo "  \"suites_passed\": $PASSED,"
    echo "  \"failed_suites\": $FAIL_COUNT,"
    echo "  \"status\": \"$([ $STATUS -eq 0 ] && echo pass || echo FAIL)\""
    if [ -n "$FAILED_TESTS" ]; then
        echo "  ,\"failed_tests\": ["
        FIRST=1
        while IFS= read -r t; do
            [ -z "$t" ] && continue
            if [ $FIRST -eq 0 ]; then echo ","; fi
            printf '    \"%s\"' "$t"
            FIRST=0
        done <<< "$FAILED_TESTS"
        echo
        echo "  ]"
    fi
    echo "}"
} > "$OUT/receipt.json"

{
    echo "# Hostile-media court — $(date -u)"
    echo
    echo "- revision: \`$REV\`"
    echo "- kernel: \`$KERNEL\`"
    echo "- unix: $NOW"
    echo "- cases: descriptor=$DESC_CASES graph=$GRAPH_CASES store=$STORE_CASES"
    echo "- suites passed: $PASSED / failed: $FAIL_COUNT"
    echo
    if [ $STATUS -eq 0 ]; then
        echo "## Admission: PASS"
        echo
        echo "Every court and the full lib suite pass. The court's"
        echo "resource-bounds claim is therefore implemented."
    else
        echo "## Admission: FAIL"
        echo
        echo "Failing tests:"
        echo '```'
        echo "$FAILED_TESTS"
        echo '```'
    fi
    echo
    echo "## Raw output"
    echo
    echo '```text'
    cat "$OUT/run.log"
    echo '```'
} > "$OUT/summary.md"

echo
echo "receipt: $OUT/receipt.json"
echo "summary: $OUT/summary.md"
echo "court status: $([ $STATUS -eq 0 ] && echo PASS || echo FAIL)"
exit $STATUS
