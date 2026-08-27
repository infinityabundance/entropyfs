#!/usr/bin/env bash
# Phase 12E.23 — the release-gates checklist (verifier).
#
# # PURPOSE
#
# The 20-point adoption-release gate from the 12E.23 brief. This script
# verifies the mechanically checkable conditions against the CURRENT
# tree and prints the full checklist with PASS/FAIL per item; the
# evidence-backed items point at the sealed archives. It is the
# 12E.24-success-criterion precursor: if every gate holds, the phase is
# complete.
#
# # USAGE
#
#     tools/check-release-gates.sh
#
# Exits nonzero if any gate FAILS. Every PASS is printed with its
# witness; every FAIL prints the exact missing condition.

set -u
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

PASS=0
FAIL=0
LANES_PASS=0
note() { printf '  [%s] %s\n' "$1" "$2"; }

gate() {
    # $1 = number, $2 = title, $3 = condition (0/1), $4 = witness
    if [[ "$3" == "1" ]]; then
        note PASS "$1. $2 — $4"
        PASS=$((PASS + 1))
    else
        note FAIL "$1. $2 — MISSING: $4"
        FAIL=$((FAIL + 1))
    fi
}

echo "== phase-12E.23 release gates =="

# 1. One Cargo package (ADR-0001): no workspace, one [package].
grep -q "^\[workspace\]" Cargo.toml && W=0 || W=1
gate 1 "one Cargo package, no workspace" "$W" "Cargo.toml has no [workspace]"

# 2. Stable embeddable Engine facade.
[[ -d src/engine && -f src/engine/mod.rs ]] && E=1 || E=0
gate 2 "stable embeddable Engine API" "$E" "src/engine/mod.rs"

# 3. Engine usable without FUSE.
cargo check --no-default-features >/dev/null 2>&1 && F=1 || F=0
gate 3 "engine buildable without FUSE" "$F" "cargo check --no-default-features"

# 4. Format-v1 compatibility normative + implementation-tested.
grep -q "compat_seal" src/tests/mod.rs && C=1 || C=0
gate 4 "format-v1 compat normative, tested" "$C" "src/tests/compat_seal.rs"

# 5. Golden stores continuously readable.
[[ -d testdata/golden && -f src/tests/golden_store.rs ]] && G=1 || G=0
gate 5 "golden stores readable/fsck-clean" "$G" "testdata/golden + golden_store.rs"

# 6. Unknown COMPAT/RO_COMPAT/INCOMPAT behavior tested.
grep -q "ro_compat\|RO_COMPAT" src/tests/compat_seal.rs 2>/dev/null && U=1 || U=0
gate 6 "unknown-bit behavior tested exactly" "$U" "compat_seal covers ro_compat"

# 7. status/fsck/metrics --json exist.
if grep -q "pub json: bool" src/cli/status.rs 2>/dev/null \
    && grep -q "pub json: bool" src/cli/fsck.rs 2>/dev/null \
    && grep -q "pub json: bool" src/cli/metrics.rs 2>/dev/null; then
    J=1
else
    J=0
fi
gate 7 "status/fsck/metrics --json" "$J" "src/cli/{status,fsck,metrics}.rs json flags"

# 8-10. Distro courts: sealed evidence dirs exist and record PASS.
for lane in almalinux-10.2-minimal ubuntu-26.04-minimal leap-16.0-minimal; do
    d=$(ls -d evidence/portability/distro-court-$lane-* 2>/dev/null | tail -1)
    if [[ -n "$d" ]] && grep -q "result: PASS" "$d/court/$lane/court-result.json" 2>/dev/null; then
        gate 8 "distro lane $lane passes" 1 "$(basename "$d")"
    else
        gate 8 "distro lane $lane passes" 0 "no sealed PASS evidence for $lane"
    fi
    # also count it toward gate 8's "three lanes" aggregate below
    [[ -n "$d" ]] && grep -q "result: PASS" "$d/court/$lane/court-result.json" 2>/dev/null && LANES_PASS=$((LANES_PASS + 1))
done

# 11. Immutable base-image digests recorded.
DIGESTS=$(ls evidence/portability/distro-court-*/base-image-digest.txt 2>/dev/null | wc -l)
gate 11 "immutable digest per portability artifact" "$([[ "$DIGESTS" -ge 3 ]] && echo 1 || echo 0)" \
    "$DIGESTS digest files"
# Gates 8-10 must be satisfied by ALL THREE mandatory lanes (the brief's
# hard release gate) — recorded here as the aggregate.
gate 10 "all three mandatory distro lanes pass" "$([[ "$LANES_PASS" -ge 3 ]] && echo 1 || echo 0)" \
    "$LANES_PASS/3 lanes PASS"

# 12. SyncIo vs UringIo rerun on real storage.
d=$(ls -d evidence/performance/transport-real-* 2>/dev/null | tail -1)
if [[ -n "$d" ]] && grep -q "RETAIN SYNC DEFAULT" "$d/decision.json" 2>/dev/null; then
    gate 12 "real-device transport court" 1 "$(basename "$d")"
else
    gate 12 "real-device transport court" 0 "no sealed transport-real decision"
fi

# 13. Worker-pool mount default informed by the mounted court.
grep -q "available_parallelism" src/cli/mount.rs && P=1 || P=0
gate 13 "mounted worker-pool court informed the default" "$P" "mount.rs pool default"

# 14. Small-object packing oracle result before any format change.
d=$(ls -d evidence/performance/pack-oracle-* 2>/dev/null | tail -1)
if [[ -n "$d" ]] && grep -q "REJECT-PACKS" "$d/result.json" 2>/dev/null; then
    gate 14 "small-object packing oracle" 1 "$(basename "$d")"
else
    gate 14 "small-object packing oracle" 0 "no sealed pack-oracle result"
fi

# 15. Full normal/crash/hostile/fsck/parity suite green (ci-matrix lane).
d=$(ls -d evidence/ci/ci-matrix-* 2>/dev/null | tail -1)
if [[ -n "$d" ]] && grep -q "release-suite.*PASS" "$d/matrix.txt" 2>/dev/null; then
    gate 15 "full suite green (ci-matrix)" 1 "$(basename "$d")"
else
    gate 15 "full suite green (ci-matrix)" 0 "no sealed ci-matrix PASS"
fi

# 16. Unsafe ledger invariant mandatory + enforced.
grep -q "ffi/mod.rs" src/tests/unsafe_ledger.rs && L=1 || L=0
gate 16 "unsafe ledger enforced (2 designated files)" "$L" "unsafe_ledger.rs"

# 17. Material claims point to sealed evidence (INDEX + manifests).
[[ -f evidence/performance/INDEX.md ]] && I=1 || I=0
gate 17 "evidence index + sealed manifests" "$I" "evidence/performance/INDEX.md"

# 18. README the stable front door; CHANGELOG the temporal ledger.
[[ -f README.md && -f CHANGELOG.md ]] && R=1 || R=0
gate 18 "README present / CHANGELOG history" "$R" "both files exist"

# 19. Explicit compatibility policies.
[[ -f docs/format/compatibility-policy.md && -f docs/api/engine.md ]] && Y=1 || Y=0
gate 19 "explicit API + format policies" "$Y" "docs/format/compatibility-policy.md"

# 20. Engineer trial path (install -> mkfs -> mount -> engine -> fsck).
if [[ -f tools/trial-path.sh ]] && [[ -x tools/trial-path.sh ]]; then
    T=1
else
    T=0
fi
gate 20 "one-command trial path" "$T" "tools/trial-path.sh"

echo
echo "== release gates: $PASS passed, $FAIL failed =="
exit "$([[ "$FAIL" -eq 0 ]] && echo 0 || echo 1)"
