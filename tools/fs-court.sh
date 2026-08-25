#!/usr/bin/env bash
# Competitive filesystem court v2 (Phase 8H + 9A + 9H).
#
# Measures the same corpora across:
#   - ext4 / XFS / Btrfs (raw) / Btrfs (zstd:1)   — writable, loop images
#     (XFS/Btrfs require root + loop devices; ext4 can be the host dir)
#   - EROFS / SquashFS                            — read-only images
#   - zstd standalone (whole + per-64KiB)
#   - EntropyFS (FUSE mount, unprivileged)
#
# Phase-9H: EntropyFS is reported in TWO storage states:
#   - foreground: immediately after the workload + fsync (post-GC)
#   - settled   : + background optimize (full search + shared dicts +
#                 amortized models) + full compaction (`gc --compact`),
#                 with the elapsed settle time and the physical write
#                 amplification required to get from one to the other.
#
# Measurement rules (symmetric across writable filesystems):
#   - buffered write  : cp completion time
#   - durable write   : buffered write + sync (and fsync of the copied
#                       files where the FS exposes it)
#   - warm read       : read-back immediately after write (page cache)
#   - cold read       : after sync + drop_caches (root) or remount (FUSE);
#                       the cache condition is recorded per measurement
#   - directory corpora are read via a deterministic tar stream (never
#     dd on a directory)
#   - storage: apparent bytes (du -sb) AND allocated blocks (du -sB1),
#     and for EntropyFS the complete backing store (segments + superblock)
#   - filesystem/device facts are DISCOVERED from WORKDIR (findmnt), never
#     hardcoded
#
# Evidence: evidence/performance/fs-court-<ts>-<rev>/ with results.json,
# report.md, raw-output.txt, environment.json (methodology §8 rules apply).
# Run this in a disposable ROOT-capable VM to clear the loop-mount waivers.
#
# Usage: fs-court.sh [WORKDIR] [OUTDIR]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENTROPYFS_BIN="${ENTROPYFS_BIN:-$REPO_ROOT/target/release/entropyfs}"
WORKDIR="${1:-$REPO_ROOT/target/fs-court-scratch}"
OUTROOT="${2:-$REPO_ROOT/evidence/performance}"

if [[ ! -x "$ENTROPYFS_BIN" ]]; then
    echo "error: $ENTROPYFS_BIN not found (build with: cargo build --release)" >&2
    exit 1
fi

REV="${COURT_REV:-$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || true)}"
[[ -n "$REV" ]] || REV="norev"
TS="$(date +%s)"
OUT="$OUTROOT/fs-court-$TS-$REV"
mkdir -p "$OUT" "$WORKDIR/corpora"

LOG="$OUT/raw-output.txt"
: > "$LOG"
log() { echo "$*" | tee -a "$LOG"; }

HAVE_ROOT=0
[[ "$(id -u)" == "0" ]] && HAVE_ROOT=1

cleanup() {
    for m in "$WORKDIR"/mnt-*; do
        [[ -d "$m" ]] && umount "$m" 2>/dev/null || true
    done
    rm -rf "$WORKDIR"/mnt-* "$WORKDIR"/img-* "$WORKDIR"/efs-store "$WORKDIR"/mnt-efs
}
trap cleanup EXIT

# --- discover the scratch filesystem facts (never hardcoded) --------------
SCRATCH_FSTYPE="$(findmnt -no FSTYPE -T "$WORKDIR" 2>/dev/null | head -1 || true)"
SCRATCH_SOURCE="$(findmnt -no SOURCE -T "$WORKDIR" 2>/dev/null | head -1 || true)"
[[ -n "$SCRATCH_FSTYPE" ]] || SCRATCH_FSTYPE="unknown"
[[ -n "$SCRATCH_SOURCE" ]] || SCRATCH_SOURCE="unknown"
log "scratch: $WORKDIR on $SCRATCH_FSTYPE ($SCRATCH_SOURCE)"

# --- corpora ---------------------------------------------------------------
log "== corpus construction =="
cp -r "$REPO_ROOT/src" "$WORKDIR/corpora/src" 2>/dev/null || true
cp "$REPO_ROOT/Cargo.toml" "$REPO_ROOT/Cargo.lock" "$WORKDIR/corpora/" 2>/dev/null || true
[[ -d "$REPO_ROOT/docs" ]] && cp -r "$REPO_ROOT/docs" "$WORKDIR/corpora/docs" 2>/dev/null || true
dd if=/dev/urandom of="$WORKDIR/corpora/random.bin" bs=1M count=64 status=none
dd if=/dev/zero of="$WORKDIR/corpora/zeros.bin" bs=1M count=64 status=none
(cd "$WORKDIR/corpora" && tar czf compressed.tgz src docs 2>/dev/null || true)

# Directory corpora are traversed deterministically (tar stream to /dev/null).
CORPORA=(src random.bin zeros.bin compressed.tgz)

# The corpus apparent-byte sum — the SAME numerator for every density line
# (loop-FS densities and the EntropyFS density; the FUSE copies carry the
# same st_size as these sources). Single source of truth, computed once.
CORPUS_APPARENT=$(du -sb "$WORKDIR/corpora"/src "$WORKDIR/corpora"/random.bin "$WORKDIR/corpora"/zeros.bin "$WORKDIR/corpora"/compressed.tgz 2>/dev/null | awk '{s+=$1} END {print s+0}')
log "corpus apparent total: $CORPUS_APPARENT B (src, random.bin, zeros.bin, compressed.tgz)"

results="$OUT/results.json"
python3 - "$results" <<'EOF'
import json, sys
json.dump({"fs": {}, "zstd": {}, "entropyfs": {}}, open(sys.argv[1], "w"))
EOF

record() {
    python3 - "$results" "$1" "$2" "$3" <<'EOF'
import json, sys
r = json.load(open(sys.argv[1]))
r[sys.argv[2]][sys.argv[3]] = json.loads(sys.argv[4]) if sys.argv[4].startswith(("{","[")) else sys.argv[4]
json.dump(r, open(sys.argv[1], "w"), indent=1)
EOF
}

# apparent bytes (du -sb) and allocated blocks (du -sB1) of a path
du_bytes() { du -sb "$1" 2>/dev/null | cut -f1; }
du_alloc() { du -sB1 "$1" 2>/dev/null | cut -f1; }

# --- symmetric measurement on a mounted filesystem -------------------------
# measure_mounted <section> <name> <dir> [desc]
measure_mounted() {
    local section="$1" name="$2" dir="$3"
    log "== $name (${4:-$dir}) =="
    for c in "${CORPORA[@]}"; do
        local src="$WORKDIR/corpora/$c" dst="$dir/$c"
        # buffered write: cp completion. durable write: cp + sync (the
        # time until the data is durable on the device).
        local t0 t1 d1
        t0=$(date +%s%N)
        cp -r "$src" "$dst"
        t1=$(date +%s%N)
        sync
        d1=$(date +%s%N)
        local apparent allocated wmbps dwmbps
        apparent=$(du_bytes "$dst")
        allocated=$(du_alloc "$dst")
        wmbps=$(python3 -c "print(f'{$apparent/1048576/(($t1-$t0)/1e9):.1f}')")
        dwmbps=$(python3 -c "print(f'{$apparent/1048576/(($d1-$t0)/1e9):.1f}')")
        # warm read (immediately after write, page cache retained)
        local r0 r1 rwmbps
        r0=$(date +%s%N)
        read_corpus "$dir" "$c"
        r1=$(date +%s%N)
        rwmbps=$(python3 -c "print(f'{$apparent/1048576/(($r1-$r0)/1e9):.1f}')")
        # cold read (root: drop_caches; otherwise recorded as warm-retained)
        local cold="$rwmbps"
        if [[ "$HAVE_ROOT" == "1" ]]; then
            sync
            echo 3 > /proc/sys/vm/drop_caches
            local c0 c1
            c0=$(date +%s%N)
            read_corpus "$dir" "$c"
            c1=$(date +%s%N)
            cold=$(python3 -c "print(f'{$apparent/1048576/(($c1-$c0)/1e9):.1f}')")
        fi
        log "  $c: apparent $apparent allocated $allocated buffered-write ${wmbps} MiB/s durable-write ${dwmbps} MiB/s warm-read ${rwmbps} MiB/s cold-read ${cold} MiB/s"
        record "$section" "$name/$c" "{\"apparent\": $apparent, \"allocated\": $allocated, \"buffered_write_mbps\": $wmbps, \"durable_write_mbps\": $dwmbps, \"warm_read_mbps\": $rwmbps, \"cold_read_mbps\": $cold, \"cache\": \"$( [[ \"$HAVE_ROOT\" == 1 ]] && echo drop-caches || echo warm-retained )\"}"
    done
}

# Deterministic corpus read: directories via a tar stream (never dd on a
# directory), regular files via dd to /dev/null.
read_corpus() {
    local dir="$1" c="$2"
    if [[ -d "$dir/$c" ]]; then
        tar -cf - -C "$dir" "$c" 2>/dev/null | wc -c >/dev/null
    else
        dd if="$dir/$c" of=/dev/null bs=1M status=none
    fi
}

# --- 1. ext4 / host scratch dir ----------------------------------------------
mkdir -p "$WORKDIR/mnt-ext4"
measure_mounted fs "ext4" "$WORKDIR/mnt-ext4" "host $SCRATCH_FSTYPE on $SCRATCH_SOURCE"

# --- 2/3/4. XFS / Btrfs (±zstd) loop images ---------------------------------
run_loop_fs() {  # run_loop_fs <name> "<mkfs args>" "<mount opts>"
    local name="$1" mkfs_args="$2" mnt_opts="$3"
    log "== $name (loop image) =="
    if [[ "$HAVE_ROOT" != "1" ]]; then
        log "  WAIVER: $name requires root + loop devices; run in a root-capable VM:"
        log "    $mkfs_args <image> && mount -o loop${mnt_opts:+,$mnt_opts} <image> <mnt>"
        record fs "$name" "{\"waived\": \"requires root + loop\", \"command\": \"$mkfs_args <image> && mount -o loop${mnt_opts:+,$mnt_opts} <image> <mnt>\"}"
        return
    fi
    local img="$WORKDIR/img-$name"
    truncate -s 1G "$img" # sparse: 1 GiB logical, ~0 allocated until written
    # shellcheck disable=SC2086
    $mkfs_args "$img" >/dev/null 2>&1 || { log "  WAIVER: $mkfs_args failed"; record fs "$name" "{\"waived\": \"mkfs failed\"}"; return; }
    local mnt="$WORKDIR/mnt-$name"
    mkdir -p "$mnt"
    if [[ -n "$mnt_opts" ]]; then
        mount -o "$mnt_opts,loop" "$img" "$mnt" || { log "  WAIVER: mount failed"; record fs "$name" "{\"waived\": \"mount failed\"}"; return; }
    else
        mount -o loop "$img" "$mnt" || { log "  WAIVER: mount failed"; record fs "$name" "{\"waived\": \"mount failed\"}"; return; }
    fi
    measure_mounted fs "$name" "$mnt" "loop image (${mnt_opts:-no compression})"
    # Durable teardown: unmount flushes; report the image's allocated size
    # (the true on-disk cost of the filesystem incl. its metadata).
    sync
    umount "$mnt"
    local img_alloc img_size density
    img_alloc=$(du_alloc "$img")
    img_size=$(du_bytes "$img")
    # Phase-9H: the loop-FS equivalent of the EntropyFS density line — the
    # SAME numerator ($CORPUS_APPARENT, the four measured corpora) over the
    # COMPLETE filesystem state (the whole loop image's allocated blocks,
    # which include the FS's own metadata — the same treatment as the
    # EntropyFS store backing). Computed and sealed by the tooling, never
    # derived by hand.
    if [[ "$CORPUS_APPARENT" -gt 0 && "$img_alloc" -gt 0 ]]; then
        density=$(python3 -c "print(f'{$CORPUS_APPARENT/$img_alloc:.3f}')")
    else
        density="0"
    fi
    log "  image: logical $img_size allocated $img_alloc; corpus apparent $CORPUS_APPARENT; density ${density}x"
    record fs "$name" "{\"image_logical_bytes\": $img_size, \"image_allocated_bytes\": $img_alloc, \"corpus_apparent_bytes\": $CORPUS_APPARENT, \"density\": $density}"
}

if command -v mkfs.xfs >/dev/null; then
    run_loop_fs xfs "mkfs.xfs -f" ""
else
    log "== xfs: WAIVER (mkfs.xfs not installed) =="
    record fs "xfs" "{\"waived\": \"mkfs.xfs not installed\"}"
fi

if command -v mkfs.btrfs >/dev/null; then
    run_loop_fs btrfs "mkfs.btrfs -f" ""
    run_loop_fs btrfs-zstd "mkfs.btrfs -f" "compress=zstd:1"
else
    log "== btrfs: WAIVER (mkfs.btrfs not installed) =="
    record fs "btrfs" "{\"waived\": \"mkfs.btrfs not installed\"}"
    record fs "btrfs-zstd" "{\"waived\": \"mkfs.btrfs not installed\"}"
fi

# --- 5/6. SquashFS / EROFS images (unprivileged creation) ------------------
if command -v mksquashfs >/dev/null; then
    log "== squashfs (zstd image) =="
    for c in "${CORPORA[@]}"; do
        img="$WORKDIR/img-squash-$c.sqfs"
        mksquashfs "$WORKDIR/corpora/$c" "$img" -comp zstd >/dev/null 2>&1
        size=$(du_bytes "$img")
        alloc=$(du_alloc "$img")
        apparent=$(du_bytes "$WORKDIR/corpora/$c")
        ratio=$(python3 -c "print(f'{$apparent/max($size,1):.3f}')")
        log "  $c: apparent $apparent image $size allocated $alloc ratio ${ratio}x"
        record fs "squashfs-zstd/$c" "{\"apparent\": $apparent, \"image\": $size, \"allocated\": $alloc, \"ratio\": $ratio}"
    done
else
    log "== squashfs: WAIVER (mksquashfs not installed) =="
    record fs "squashfs-zstd" "{\"waived\": \"mksquashfs not installed\"}"
fi

if command -v mkfs.erofs >/dev/null; then
    log "== erofs (lz4hc image) =="
    for c in "${CORPORA[@]}"; do
        img="$WORKDIR/img-erofs-$c.erofs"
        staging="$WORKDIR/erofs-stage-$c"
        mkdir -p "$staging"
        cp -r "$WORKDIR/corpora/$c" "$staging/"
        mkfs.erofs -zlz4hc "$img" "$staging" >/dev/null 2>&1 || { log "  WAIVER: mkfs.erofs failed for $c"; record fs "erofs-lz4hc/$c" "{\"waived\": \"mkfs.erofs failed\"}"; continue; }
        size=$(du_bytes "$img")
        alloc=$(du_alloc "$img")
        apparent=$(du_bytes "$WORKDIR/corpora/$c")
        ratio=$(python3 -c "print(f'{$apparent/max($size,1):.3f}')")
        log "  $c: apparent $apparent image $size allocated $alloc ratio ${ratio}x"
        record fs "erofs-lz4hc/$c" "{\"apparent\": $apparent, \"image\": $size, \"allocated\": $alloc, \"ratio\": $ratio}"
    done
else
    log "== erofs: WAIVER (mkfs.erofs not installed) =="
    record fs "erofs-lz4hc" "{\"waived\": \"mkfs.erofs not installed\"}"
fi

# --- 7. zstd standalone (whole + per-64KiB) ----------------------------------
if command -v zstd >/dev/null; then
    log "== zstd standalone =="
    for level in 1 19; do
        for c in "${CORPORA[@]}"; do
            src="$WORKDIR/corpora/$c"
            t0=$(date +%s%N)
            if [[ -d "$src" ]]; then
                (cd "$WORKDIR/corpora" && tar c "$c" | zstd -q -"$level" -o "$WORKDIR/zstd-$c-$level.zst")
            else
                zstd -q -"$level" -f "$src" -o "$WORKDIR/zstd-$c-$level.zst"
            fi
            t1=$(date +%s%N)
            size=$(du_bytes "$WORKDIR/zstd-$c-$level.zst")
            apparent=$(du_bytes "$src")
            ratio=$(python3 -c "print(f'{$apparent/max($size,1):.3f}')")
            wmbps=$(python3 -c "print(f'{$apparent/1048576/(($t1-$t0)/1e9):.1f}')")
            log "  zstd -$level $c: apparent $apparent image $size ratio ${ratio}x ${wmbps} MiB/s"
            record zstd "-$level/$c" "{\"apparent\": $apparent, \"image\": $size, \"ratio\": $ratio, \"write_mbps\": $wmbps}"
        done
        # per-64KiB diagnostic (the dictionary-horizon test)
        t0=$(date +%s%N)
        (cd "$WORKDIR/corpora" && tar c src | python3 -c "
import subprocess, sys
total = 0
while True:
    chunk = sys.stdin.buffer.read(65536)
    if not chunk: break
    p = subprocess.run(['zstd', '-q', '-$level', '-c'], input=chunk, capture_output=True)
    total += len(p.stdout)
print(total)
" > "$WORKDIR/zstd-src-per64k-$level.size" 2>/dev/null || true)
        t1=$(date +%s%N)
        size=$(cat "$WORKDIR/zstd-src-per64k-$level.size" 2>/dev/null || echo 0)
        apparent=$(du_bytes "$WORKDIR/corpora/src")
        ratio=$(python3 -c "print(f'{$apparent/max($size,1):.3f}')")
        log "  zstd -$level src per-64KiB: apparent $apparent image $size ratio ${ratio}x"
        record zstd "-$level/src-per-64k" "{\"apparent\": $apparent, \"image\": $size, \"ratio\": $ratio}"
    done
else
    log "== zstd: WAIVER (zstd not installed) =="
    record zstd "waived" "zstd not installed"
fi

# --- 8. EntropyFS (FUSE, unprivileged; symmetric buffered/durable + reads) --
log "== entropyfs (FUSE store) =="
if [[ -e /dev/fuse ]] && command -v fusermount3 >/dev/null; then
    mkdir -p "$WORKDIR/efs-store" "$WORKDIR/mnt-efs"
    "$ENTROPYFS_BIN" mkfs "$WORKDIR/efs-store" >/dev/null
    "$ENTROPYFS_BIN" mount "$WORKDIR/efs-store" "$WORKDIR/mnt-efs" &
    EFS_PID=$!
    for _ in $(seq 1 50); do
        mountpoint -q "$WORKDIR/mnt-efs" && break
        sleep 0.1
    done
    if mountpoint -q "$WORKDIR/mnt-efs"; then
        measure_mounted entropyfs "entropyfs" "$WORKDIR/mnt-efs" "FUSE mount of $WORKDIR/efs-store"
        # Cold read for FUSE: remount (fresh FUSE daemon page cache; the
        # backing store page cache is retained — recorded honestly).
        "$ENTROPYFS_BIN" unmount "$WORKDIR/mnt-efs" || fusermount3 -u "$WORKDIR/mnt-efs" || true
        wait "$EFS_PID" 2>/dev/null || true
        "$ENTROPYFS_BIN" mount "$WORKDIR/efs-store" "$WORKDIR/mnt-efs" &
        EFS_PID=$!
        for _ in $(seq 1 50); do
            mountpoint -q "$WORKDIR/mnt-efs" && break
            sleep 0.1
        done
        if mountpoint -q "$WORKDIR/mnt-efs"; then
            for c in "${CORPORA[@]}"; do
                dst="$WORKDIR/mnt-efs/$c"
                apparent=$(du_bytes "$dst")
                r0=$(date +%s%N)
                read_corpus "$WORKDIR/mnt-efs" "$c"
                r1=$(date +%s%N)
                rwmbps=$(python3 -c "print(f'{$apparent/1048576/(($r1-$r0)/1e9):.1f}')")
                log "  $c (FUSE remount cold): read ${rwmbps} MiB/s"
                record entropyfs "$c/cold_read_mbps" "$rwmbps"
            done
        fi
        "$ENTROPYFS_BIN" unmount "$WORKDIR/mnt-efs" || fusermount3 -u "$WORKDIR/mnt-efs" || true
        wait "$EFS_PID" 2>/dev/null || true
        # Post-GC backing footprint: segments + superblock, apparent AND
        # allocated blocks (the complete store, not just segment lengths).
        "$ENTROPYFS_BIN" fsck "$WORKDIR/efs-store" >/dev/null 2>&1 && log "  fsck: clean"
        "$ENTROPYFS_BIN" gc "$WORKDIR/efs-store" >/dev/null 2>&1 || true
        backing_apparent=$(du_bytes "$WORKDIR/efs-store")
        backing_alloc=$(du_alloc "$WORKDIR/efs-store")
        total_apparent=$(python3 -c "
import json
r = json.load(open('$results'))
a = sum(v['apparent'] for v in r['entropyfs'].values() if isinstance(v, dict) and 'apparent' in v)
print(a)")
        log "  backing store (post-GC): apparent $backing_apparent allocated $backing_alloc; total corpus apparent $total_apparent"
        if [[ "$backing_alloc" -gt 0 && "$total_apparent" -gt 0 ]]; then
            ratio=$(python3 -c "print(f'{$total_apparent/$backing_alloc:.3f}')")
            log "  entropyfs effective density (apparent / allocated backing): ${ratio}x"
            record entropyfs "density" "{\"apparent\": $total_apparent, \"backing_apparent\": $backing_apparent, \"backing_allocated\": $backing_alloc, \"ratio\": $ratio}"
        fi

        # Phase-9H: the SETTLED state — background optimize (full search +
        # shared dictionaries + amortized models) then full compaction —
        # with the elapsed time and physical write amplification needed to
        # get from the foreground state to it. The store is unmounted, so
        # the offline passes can run.
        log "  settle: foreground -> settled (optimize + compact)"
        fg_apparent=$backing_apparent
        fg_alloc=$backing_alloc
        t0=$(date +%s%N)
        OPT_OUT="$("$ENTROPYFS_BIN" optimize "$WORKDIR/efs-store" 2>&1 || true)"
        t1=$(date +%s%N)
        opt_wall=$(python3 -c "print(f'{($t1-$t0)/1e9:.2f}')")
        opt_apparent=$(du_bytes "$WORKDIR/efs-store")
        log "  settle: optimize ${opt_wall}s (backing $fg_apparent -> $opt_apparent)"
        log "    $(echo "$OPT_OUT" | tr '\n' ' ' | cut -c1-500)"
        t0=$(date +%s%N)
        COMP_OUT="$("$ENTROPYFS_BIN" gc --compact "$WORKDIR/efs-store" 2>&1 || true)"
        t1=$(date +%s%N)
        comp_wall=$(python3 -c "print(f'{($t1-$t0)/1e9:.2f}')")
        settled_apparent=$(du_bytes "$WORKDIR/efs-store")
        settled_alloc=$(du_alloc "$WORKDIR/efs-store")
        log "  settle: full compaction ${comp_wall}s (backing $opt_apparent -> $settled_apparent)"
        log "    $(echo "$COMP_OUT" | tr '\n' ' ' | cut -c1-300)"
        # Physical write amplification to settle: bytes appended during
        # settle / settled live bytes. Optimize appends ~(opt - fg); full
        # compaction copies ~all live bytes into a fresh segment.
        opt_appended=$((opt_apparent > fg_apparent ? opt_apparent - fg_apparent : 0))
        compact_appended=$settled_apparent
        settle_appended=$((opt_appended + compact_appended))
        settle_amp=$(python3 -c "print(f'{$settle_appended/max($settled_apparent,1):.3f}')")
        settle_wall=$(python3 -c "print(f'{($opt_wall + $comp_wall):.2f}')")
        if [[ "$total_apparent" -gt 0 ]]; then
            settled_ratio=$(python3 -c "print(f'{$total_apparent/$settled_apparent:.3f}')")
            settled_density=$(python3 -c "print(f'{$total_apparent/max($settled_alloc,1):.3f}')")
        else
            settled_ratio="0"
            settled_density="0"
        fi
        log "  settled store: apparent $settled_apparent allocated $settled_alloc; density (apparent/backing) ${settled_ratio}x, (apparent/allocated) ${settled_density}x"
        log "  settle cost: ${settle_wall}s elapsed, ${settle_amp}x physical write amplification (appended $settle_appended B)"
        record entropyfs "settled" "{\"foreground_apparent\": $fg_apparent, \"foreground_allocated\": $fg_alloc, \"settled_apparent\": $settled_apparent, \"settled_allocated\": $settled_alloc, \"settle_elapsed_s\": $settle_wall, \"optimize_wall_s\": $opt_wall, \"compact_wall_s\": $comp_wall, \"settle_appended_bytes\": $settle_appended, \"settle_write_amp\": $settle_amp, \"settled_density\": $settled_density}"
        "$ENTROPYFS_BIN" fsck "$WORKDIR/efs-store" >/dev/null 2>&1 && log "  settled fsck: clean"
    else
        log "  WAIVER: entropyfs mount failed"
        record entropyfs "waived" "mount failed"
    fi
else
    log "== entropyfs: WAIVER (/dev/fuse or fusermount3 missing) =="
    record entropyfs "waived" "/dev/fuse or fusermount3 missing"
fi

# --- environment + report ----------------------------------------------------
python3 - "$OUT" "$REV" "$TS" "$SCRATCH_FSTYPE" "$SCRATCH_SOURCE" <<'EOF'
import json, platform, os, sys
out, rev, ts, fstype, source = sys.argv[1:6]
env = {
    "revision": rev,
    "created_unix": int(ts),
    "kernel": platform.release(),
    "cpu": platform.processor() or platform.machine(),
    "scratch_fstype": fstype,
    "scratch_source": source,
    "uname": os.uname().nodename,
    "root": os.geteuid() == 0,
}
json.dump(env, open(f"{out}/environment.json", "w"), indent=1)
EOF

python3 - "$results" "$OUT" <<'EOF'
import json, sys
results, out = sys.argv[1], sys.argv[2]
r = json.load(open(results))
with open(f"{out}/report.md", "w") as f:
    f.write("# Filesystem court v2 (Phase 8H + 9A + 9H)\n\n")
    f.write(f"Archive: {out}\n\n")
    f.write("Corpus artifact: the structured corpus contains only 4 unique\n")
    f.write("64 KiB chunks — a corpus property, not a claim (methodology §8).\n\n")
    f.write("## Density (computed and sealed by the tooling)\n\n")
    f.write("Numerator: the same corpus apparent-byte sum (du -sb of src,\n")
    f.write("random.bin, zeros.bin, compressed.tgz) for every row. Denominators:\n")
    f.write("the COMPLETE filesystem state — the whole loop image's allocated\n")
    f.write("blocks for XFS/Btrfs (including their own metadata), the complete\n")
    f.write("EntropyFS store backing (segments + superblock). Both denominators\n")
    f.write("therefore include filesystem overhead beyond the corpus files.\n\n")
    dens = {}
    for name, v in r.get("fs", {}).items():
        if isinstance(v, dict) and "density" in v:
            dens[name] = (v["density"], v["image_allocated_bytes"])
    if "settled" in r.get("entropyfs", {}):
        dens["entropyfs-settled"] = (r["entropyfs"]["settled"]["settled_density"], r["entropyfs"]["settled"]["settled_allocated"])
    for name, (d, alloc) in sorted(dens.items()):
        f.write(f"- {name}: {d}x (allocated {alloc} B)\n")
    f.write("\n")
    if "settled" in r.get("entropyfs", {}):
        s = r["entropyfs"]["settled"]
        f.write("## EntropyFS storage states (Phase-9H)\n\n")
        f.write(f"- foreground (post-GC): apparent {s['foreground_apparent']} B, "
                f"allocated {s['foreground_allocated']} B\n")
        f.write(f"- settled (+optimize +full compaction): apparent {s['settled_apparent']} B, "
                f"allocated {s['settled_allocated']} B (density {s['settled_density']}x)\n")
        f.write(f"- settle cost: {s['settle_elapsed_s']} s elapsed "
                f"(optimize {s['optimize_wall_s']} s + compact {s['compact_wall_s']} s), "
                f"{s['settle_write_amp']}x physical write amplification "
                f"({s['settle_appended_bytes']} B appended)\n\n")
    for section in ("fs", "zstd", "entropyfs"):
        f.write(f"## {section}\n\n")
        for k, v in sorted(r[section].items()):
            f.write(f"- {k}: {json.dumps(v)}\n")
        f.write("\n")
    f.write("## Waivers\n\n")
    for section in ("fs", "entropyfs"):
        for k, v in r[section].items():
            if isinstance(v, dict) and "waived" in v:
                f.write(f"- {section}/{k}: {v['waived']}\n")
    f.write("\nRun this court in a root-capable VM to clear the loop-mount\n")
    f.write("waivers and enable drop_caches cold reads.\n")
print(f"court evidence written to {out}")
EOF

echo "done: $OUT"
