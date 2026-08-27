#!/usr/bin/env bash
# Phase 12E.11 — the SyncIo / UringIo real-device transport court (driver).
#
# # PURPOSE
#
# The 10F transport evidence was tmpfs-backed and left `SyncIo` as the
# default (the crash-consistency oracle, ADR-0021): on sub-µs tmpfs I/O
# the ~2.3 µs io_uring submit/wait floor made `UringIo` 5–27% slower on
# writes and 7–12% on reads. Phase 12E.11 reruns the comparison on REAL
# storage because the syscall-vs-ring tradeoff shifts when the device
# latency is microseconds instead of sub-microseconds.
#
# The oracle itself is `src/tests/transport_real_court.rs`; this driver
# runs it across:
#
#     devices:  real NVMe lane   (TRANSPORT_REAL_DIR, default /mnt/2tb_crucial)
#               tmpfs control    (TRANSPORT_SHM_DIR,  default /dev/shm)
#               SATA SSD lane    (TRANSPORT_SATA_DIR, default /mnt/256gb_btrfs)
#     backends: sync, uring (default features include `uring`)
#
# and archives the sealed evidence under
# `evidence/performance/transport-real-<ts>-<rev>/` with a per-lane
# `evidence-manifest.json` (12E.5), raw logs, device context, the
# machine-readable results and the DECISION record.
#
# # BOUNDARY
#
# KNOWS: how to invoke the oracle and archive its output. NEVER KNOWS:
# store internals, representation policy, or the on-disk format. It
# changes NO production code and NO default.
#
# # DECISION GATE (normative, from the 12E.11 brief)
#
# - Uring wins robustly across the target workloads on real storage →
#   consider flipping the default.
# - Sync wins small-QD, Uring wins high-QD → investigate a deterministic
#   `auto` policy. (This court drives one stream at a time = the small-QD
#   regime; a high-QD real-device sweep is a separate follow-up oracle.)
# - Uring still loses → retain the Sync default.
#
# Never flip the default to satisfy a roadmap bullet; `SyncIo` remains
# the semantic/crash-consistency oracle regardless.
#
# # ENVIRONMENT-CAPABILITY DISTINCTION (12E.8 discipline)
#
# A lane that fails is either an implementation failure or an
# environment capability waiver. Waivers record the exact failed probe,
# the exact command, the exact error, and the requirement to clear it.
# The driver classifies: store-create failures mentioning io_uring / ring
# / EOPNOTSUPP / ENOSYS / EPERM are waivers; everything else is a
# failure that fails the court.
#
# # USAGE
#
#     tools/court-transport-real.sh [OUTROOT]
#     TRANSPORT_REAL_DIR=/path TRANSPORT_SHM_DIR=/path TRANSPORT_SATA_DIR=/path \
#       TRANSPORT_WORK_MIB=256 tools/court-transport-real.sh
#
# Requires: bash, cargo, python3, findmnt, lsblk. Root is NOT required
# (no drop_caches; the read phases are warm-cache measurements).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REAL_DIR="${TRANSPORT_REAL_DIR:-/mnt/2tb_crucial}"
SHM_DIR="${TRANSPORT_SHM_DIR:-/dev/shm}"
SATA_DIR="${TRANSPORT_SATA_DIR:-/mnt/256gb_btrfs}"
WORK_MIB="${TRANSPORT_WORK_MIB:-256}"
OUTROOT="${1:-$REPO_ROOT/evidence/performance}"
BUILD_DIR="${COURT_WORKTREE:-$REPO_ROOT}"

TS="$(date +%s)"
REV="$(git -C "$BUILD_DIR" rev-parse --short HEAD)"
OUT="$OUTROOT/transport-real-$TS-$REV"
mkdir -p "$OUT"
exec > >(tee "$OUT/court.log") 2>&1

echo "== phase-12E.11 transport court: rev=$REV work=${WORK_MIB}MiB =="

# --- binary (for evidence-manifest) --------------------------------------
BIN="$REPO_ROOT/target/release/entropyfs"
if [[ ! -x "$BIN" ]]; then
    echo "building release binary..."
    (cd "$REPO_ROOT" && cargo build --release --locked)
fi
"$BIN" --version 2>/dev/null | head -1 || echo "binary: $BIN (version query skipped)"

# --- preflight ------------------------------------------------------------
for d in "$REAL_DIR" "$SHM_DIR" "$SATA_DIR"; do
    [[ -d "$d" ]] || { echo "error: device dir missing: $d" >&2; exit 1; }
    [[ -w "$d" ]] || { echo "error: device dir not writable: $d" >&2; exit 1; }
done

# --- context capture ------------------------------------------------------
{
    echo "kernel: $(uname -r)"
    echo "cpu: $(grep -m1 'model name' /proc/cpuinfo | sed 's/.*: //')"
    echo "nproc: $(nproc)"
    echo "memory_kib: $(grep -m1 MemTotal /proc/meminfo | awk '{print $2}')"
    echo "governor: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo unknown)"
    echo "rustc: $(rustc --version 2>/dev/null || echo unknown)"
} | tee "$OUT/environment.txt"
lsblk -o NAME,MODEL,SIZE,ROTA,TRAN 2>/dev/null | tee "$OUT/lsblk.txt" || true
: > "$OUT/mounts.txt"
for d in "$REAL_DIR" "$SHM_DIR" "$SATA_DIR"; do
    {
        echo "== $d"
        findmnt -n -o SOURCE,FSTYPE,OPTIONS -T "$d" 2>/dev/null || echo "  (not a mountpoint / unknown)"
        stat -c '  dev=%d fs=%T' "$d" 2>/dev/null || true
    } >> "$OUT/mounts.txt"
done
cat "$OUT/mounts.txt"

# --- lane runner ----------------------------------------------------------
run_lane() {
    local dev_name="$1" dev_dir="$2" be="$3"
    local log="$OUT/$dev_name-$be.log"
    echo "== lane: $dev_name / $be =="
    local ok=0
    if TRANSPORT_DEVICE="$dev_dir" TRANSPORT_BACKEND="$be" TRANSPORT_WORK_MIB="$WORK_MIB" \
        cargo test --release --features fuse,ublk,uring,tracing --locked --lib \
        transport_real_court -- --nocapture > "$log" 2>&1; then
        grep -m1 '^TRANSPORT_RESULT ' "$log" | sed 's/^TRANSPORT_RESULT //' > "$OUT/$dev_name-$be.json"
        echo "  ok (result JSON written)"
    else
        # Classify: capability waiver vs implementation failure. The
        # waiver must carry the exact probe, command, error and the
        # requirement needed to clear it.
        local err
        err="$(grep -m1 -E 'panicked at|Error: ' "$log" || tail -2 "$log")"
        if grep -qEi 'io.?uring|submit.*ring|EOPNOTSUPP|ENOSYS|EPERM|Operation not permitted|function not implemented' "$err"; then
            {
                echo "WAIVER [$dev_name/$be]"
                echo "  probe:     Store::create on $dev_dir with backend $be"
                echo "  command:   TRANSPORT_DEVICE=$dev_dir TRANSPORT_BACKEND=$be TRANSPORT_WORK_MIB=$WORK_MIB cargo test --release --features fuse,ublk,uring,tracing --locked --lib transport_real_court"
                echo "  error:     $err"
                echo "  requires:  io_uring-capable kernel + unrestricted ring syscalls (see log)"
            } | tee -a "$OUT/waivers.txt"
        else
            {
                echo "FAILURE [$dev_name/$be]"
                echo "  error: $err"
                echo "  full log: $log"
            } | tee -a "$OUT/failures.txt"
        fi
    fi
}

run_lane real "$REAL_DIR" sync
run_lane real "$REAL_DIR" uring
run_lane shm  "$SHM_DIR"  sync
run_lane shm  "$SHM_DIR"  uring
run_lane sata "$SATA_DIR" sync
run_lane sata "$SATA_DIR" uring

# --- per-lane sealed evidence manifests ------------------------------------
lane_specs=("real:sync" "real:uring" "shm:sync" "shm:uring" "sata:sync" "sata:uring")
for spec in "${lane_specs[@]}"; do
    dev_name="${spec%%:*}"
    be="${spec##*:}"
    [[ -s "$OUT/$dev_name-$be.json" ]] || continue
    lane_dir="$OUT/lanes/$dev_name-$be"
    mkdir -p "$lane_dir"
    "$BIN" evidence-manifest "$lane_dir/evidence-manifest.json" \
        --store "$OUT" --io-backend "$be" --worker-scheduler pool \
        --court-schema-version 2 >/dev/null 2>&1 \
        && echo "  manifest: $dev_name/$be" || echo "  manifest FAILED: $dev_name/$be"
done

# --- decision pass ---------------------------------------------------------
python3 - "$OUT" "$WORK_MIB" <<'PY'
import json, os, sys
out, work_mib = sys.argv[1], sys.argv[2]

def load(dev, be):
    p = os.path.join(out, f"{dev}-{be}.json")
    if not os.path.exists(p):
        return None
    with open(p) as f:
        return json.load(f)

devices = ["real", "sata", "shm"]
labels = {"real": "NVMe (real)", "sata": "SATA SSD (real)", "shm": "tmpfs (control)"}
metrics = ["write_mbps", "write_fsync_every_mbps", "read_mbps", "random_read_mbps", "mixed_mbps"]

rows = []
for dev in devices:
    s = load(dev, "sync")
    u = load(dev, "uring")
    row = {"device": dev, "label": labels[dev]}
    if s is None and u is None:
        row["status"] = "no-lane-data"
        rows.append(row)
        continue
    row["status"] = "ok"
    row["sync"] = s
    row["uring"] = u
    # per-metric winner (higher throughput / lower latency / lower CPU wins)
    tally = {"sync": 0, "uring": 0, "tie": 0}
    detail = {}
    for m in metrics:
        sv = s[m] if s else None
        uv = u[m] if u else None
        if sv is None or uv is None:
            detail[m] = "n/a"
            continue
        if uv > sv * 1.01:
            tally["uring"] += 1; detail[m] = "uring"
        elif sv > uv * 1.01:
            tally["sync"] += 1; detail[m] = "sync"
        else:
            tally["tie"] += 1; detail[m] = "tie"
    for m, key in [("read_p95_us", "p95"), ("read_p99_us", "p99"), ("random_read_p95_us", "rp95"), ("random_read_p99_us", "rp99")]:
        sv, uv = (s or {}).get(m), (u or {}).get(m)
        if sv is None or uv is None:
            detail[key] = "n/a"
            continue
        if uv < sv * 0.99:
            tally["uring"] += 1; detail[key] = "uring"
        elif sv < uv * 0.99:
            tally["sync"] += 1; detail[key] = "sync"
        else:
            tally["tie"] += 1; detail[key] = "tie"
    for m in ["write_cpu_s", "write_fsync_cpu_s", "read_cpu_s", "random_read_cpu_s", "mixed_cpu_s"]:
        sv, uv = (s or {}).get(m), (u or {}).get(m)
        if sv is None or uv is None:
            continue
        if uv < sv * 0.99:
            tally["uring"] += 1
        elif sv < uv * 0.99:
            tally["sync"] += 1
    row["tally"] = tally
    row["detail"] = detail
    rows.append(row)

# The gate is about the REAL device. "Robustly" = uring wins the
# majority of the 9 headline comparisons AND does not lose read
# latency on the real device.
real = next(r for r in rows if r["device"] == "real")
decide = {
    "oracle": "phase-12e11-transport-real",
    "work_mib": int(work_mib),
    "gate": (
        "Uring wins robustly across the target workloads on real storage "
        "-> consider flipping the default; Sync wins small-QD / Uring wins "
        "high-QD -> investigate deterministic auto; Uring still loses -> "
        "retain the Sync default (the crash-consistency oracle)."
    ),
    "note": "single-stream in-process court = the small-queue-depth regime",
}
if real.get("tally"):
    t = real["tally"]
    total = t["sync"] + t["uring"]
    if t["uring"] > t["sync"] and t["uring"] * 2 >= total:
        decide["recommendation"] = "CONSIDER URING DEFAULT"
        decide["rationale"] = f"uring wins {t['uring']}/{total} headline comparisons on the real NVMe lane"
    else:
        decide["recommendation"] = "RETAIN SYNC DEFAULT"
        decide["rationale"] = (
            f"uring wins only {t['uring']}/{total} headline comparisons on the real NVMe lane; "
            "the 12E.11 gate requires a robust real-storage win before any default change"
        )
else:
    decide["recommendation"] = "INCONCLUSIVE (no real-device lane data)"
    decide["rationale"] = "the real NVMe lane produced no result JSON; see waivers/failures"

with open(os.path.join(out, "results.json"), "w") as f:
    json.dump({"rows": rows, "decision": decide}, f, indent=2)
    f.write("\n")
with open(os.path.join(out, "decision.json"), "w") as f:
    json.dump(decide, f, indent=2)
    f.write("\n")
print("== decision ==")
print(decide["recommendation"])
print(decide["rationale"])
for r in rows:
    if r.get("tally"):
        print(f"  {r['label']:18s} tally sync/uring/tie: {r['tally']['sync']}/{r['tally']['uring']}/{r['tally']['tie']}")
PY

echo "== archived: $OUT =="
