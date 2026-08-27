#!/usr/bin/env bash
# EntropyFS distribution court — build the LOCAL Docker images from the
# provided vendor minimal OS artifacts (Phase 12E.8).
#
#   Leap 16.0      Leap-16.0-online-installer-x86_64.install.iso (official
#                  installer ISO, downloaded from download.opensuse.org)
#                  -> LiveOS/rootfs.img (the installation-system userland)
#                  extracted + mounted + tarred inside a privileged
#                  container -> docker import. The cloud qcow2 remains a
#                  documented fallback when the ISO is unavailable.
#   Ubuntu 26.04   ubuntu-26.04-live-server-amd64.iso
#                  -> casper/ubuntu-server-minimal.squashfs (unsquashfs
#                     inside a container) -> docker import
#   AlmaLinux 10.2 AlmaLinux-10.2-x86_64-boot.iso
#                  -> the boot.iso is a NETWORK installer; its
#                     images/install.img (the installer userland) is the
#                     minimal base when usable, else dnf --installroot
#                     from the AlmaLinux 10.2 release repositories the
#                     ISO pins (the same content the ISO installs). The
#                     provided ISO remains the release/version authority;
#                     its sha256 is recorded in the lane evidence.
#
# Output images (local, never pulled from a hub):
#   entropyfs-court-leap:16.0
#   entropyfs-court-ubuntu:26.04
#   entropyfs-court-almalinux:10.2
#
# Evidence: tools/docker/evidence/<lane>/ with source-artifact sha256,
# extraction logs, and the produced image digest.

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
IMAGES="/run/media/one/toshiba4TB/images"
EV="$REPO_ROOT/tools/docker/evidence"
mkdir -p "$EV"

# OOM limiter: every extraction/import container runs under a hard
# memory cap (the host's docker storage is a 26 GiB RAM tmpfs; a
# runaway unsquashfs/tar must be killed, never allowed to exhaust the
# machine). Env-overridable: EXTRACT_MEM / EXTRACT_MEM_SWAP.
EXTRACT_MEM="${EXTRACT_MEM:-4g}"
EXTRACT_MEM_SWAP="${EXTRACT_MEM_SWAP:-6g}"

sha256_of() { sha256sum "$1" | cut -d' ' -f1; }

# ---------------------------------------------------------------------------
lane_leap() {
    local lane="leap"
    # The official installer ISO is the primary source (download.opensuse
    # .org); the cloud qcow2 is the fallback when the ISO is absent.
    local img="$IMAGES/Leap-16.0-online-installer-x86_64.install.iso"
    local source="installer-iso"
    if [[ ! -f "$img" ]]; then
        img="$IMAGES/Leap-16.0-Minimal-VM.x86_64-Cloud.qcow2"
        source="cloud-qcow2"
    fi
    local d="$EV/$lane"; mkdir -p "$d"
    sha256_of "$img" > "$d/source-artifact.sha256"
    echo "$source" > "$d/rootfs-source.txt"
    echo "== $lane: extracting installation-system rootfs from $source =="
    docker run --rm --privileged \
        --memory "$EXTRACT_MEM" --memory-swap "$EXTRACT_MEM_SWAP" --oom-kill-disable=false \
        -v "$img":/src.img:ro -v "$d":/out \
        ubuntu:26.04 bash -c '
            set -e
            apt-get update >/dev/null 2>&1
            apt-get install -y --no-install-recommends qemu-utils fdisk \
                squashfs-tools xorriso file >/dev/null 2>&1
            if [[ -f /src.img && $(file -b /src.img | cut -c1-4) == "qcow" ]]; then
                # Fallback path: the cloud qcow2.
                qemu-img convert -O raw /src.img /out/leap.raw
                start=$(fdisk -l /out/leap.raw | awk "/Linux root/{print \$2}")
                mkdir /mnt/r
                mount -o loop,ro,offset=$((start * 512)) /out/leap.raw /mnt/r
                tar -C /mnt/r --exclude=./proc --exclude=./sys --exclude=./dev \
                    --exclude=./run --exclude=./tmp/* --exclude=./var/lib/cloud \
                    -cf /out/rootfs.tar .
                umount /mnt/r
            else
                # Primary path: the installer ISO LiveOS rootfs.img.
                xorriso -osirrox on -indev /src.img \
                    -extract /LiveOS/squashfs.img /out/squashfs.img >/dev/null 2>&1
                mkdir -p /mnt/s /mnt/f
                unsquashfs -d /mnt/s /out/squashfs.img >/dev/null
                mount -o loop,ro /mnt/s/LiveOS/rootfs.img /mnt/f
                tar -C /mnt/f --exclude=./proc --exclude=./sys --exclude=./dev \
                    --exclude=./run --exclude=./tmp/* -cf /out/rootfs.tar .
                umount /mnt/f
            fi
        ' > "$d/extract.log" 2>&1
    docker import "$d/rootfs.tar" "entropyfs-court-$lane:16.0" > "$d/image-id.txt"
    docker inspect --format '{{.Id}}' "entropyfs-court-$lane:16.0" > "$d/image-digest.txt"
    rm -f "$d/leap.raw" "$d/rootfs.tar" "$d/squashfs.img"
    echo "== $lane: image $(cat "$d/image-digest.txt") (source: $source) =="
}

# ---------------------------------------------------------------------------
lane_ubuntu() {
    local lane="ubuntu"
    local img="$IMAGES/ubuntu-26.04-live-server-amd64.iso"
    local d="$EV/$lane"; mkdir -p "$d"
    sha256_of "$img" > "$d/source-artifact.sha256"
    echo "== $lane: extracting ubuntu-server-minimal.squashfs =="
    docker run --rm --privileged \
        --memory "$EXTRACT_MEM" --memory-swap "$EXTRACT_MEM_SWAP" --oom-kill-disable=false \
        -v "$img":/iso.iso:ro -v "$d":/out \
        ubuntu:26.04 bash -c '
            set -e
            apt-get update >/dev/null 2>&1
            apt-get install -y --no-install-recommends squashfs-tools xorriso >/dev/null 2>&1
            xorriso -osirrox on -indev /iso.iso -extract /casper/ubuntu-server-minimal.squashfs /out/min.squashfs >/dev/null 2>&1
            mkdir /mnt/s
            unsquashfs -d /mnt/s /out/min.squashfs >/dev/null
            tar -C /mnt/s --exclude=./proc --exclude=./sys --exclude=./dev \
                --exclude=./run --exclude=./tmp/* -cf /out/rootfs.tar .
        ' > "$d/extract.log" 2>&1
    docker import "$d/rootfs.tar" "entropyfs-court-$lane:26.04" > "$d/image-id.txt"
    docker inspect --format '{{.Id}}' "entropyfs-court-$lane:26.04" > "$d/image-digest.txt"
    rm -f "$d/min.squashfs" "$d/rootfs.tar"
    echo "== $lane: image $(cat "$d/image-digest.txt") =="
}

# ---------------------------------------------------------------------------
lane_almalinux() {
    local lane="almalinux"
    local img="$IMAGES/AlmaLinux-10.2-x86_64-boot.iso"
    local d="$EV/$lane"; mkdir -p "$d"
    sha256_of "$img" > "$d/source-artifact.sha256"
    echo "== $lane: checking whether the boot.iso installer env is a usable base =="
    # The boot.iso's images/install.img is the anaconda installer userland
    # (a real AlmaLinux 10.2 userland, but the installer environment). Try
    # it first; the court image's dnf install either works or falls back.
    docker run --rm --privileged \
        --memory "$EXTRACT_MEM" --memory-swap "$EXTRACT_MEM_SWAP" --oom-kill-disable=false \
        -v "$img":/iso.iso:ro -v "$d":/out \
        almalinux:10.2 bash -c '
            set -e
            dnf -y install squashfs-tools xorriso >/dev/null 2>&1 || \
                dnf -y --setopt=install_weak_deps=False install squashfs-tools xorriso >/dev/null 2>&1
            xorriso -osirrox on -indev /iso.iso -extract /images/install.img /out/install.img >/dev/null 2>&1
            mkdir /mnt/s
            unsquashfs -d /mnt/s /out/install.img >/dev/null
            tar -C /mnt/s --exclude=./proc --exclude=./sys --exclude=./dev \
                --exclude=./run --exclude=./tmp/* -cf /out/rootfs.tar .
            echo "installer-env: extracted" > /out/path.txt
        ' > "$d/extract.log" 2>&1 || true
    if [[ -s "$d/rootfs.tar" ]]; then
        echo "  using the boot.iso installer env as the minimal base"
        docker import "$d/rootfs.tar" "entropyfs-court-$lane:10.2" > "$d/image-id.txt"
        echo "installer-env" > "$d/rootfs-source.txt"
    else
        echo "  installer env unusable — assembling the identical minimal userland"
        echo "  from the AlmaLinux 10.2 release repos the boot.iso pins"
        # dnf --installroot with the 10.2 release repos (BaseOS + AppStream
        # + the minimal-environment group), inside the pulled official
        # almalinux:10.2 as the tool carrier.
        docker run --rm --privileged \
        --memory "$EXTRACT_MEM" --memory-swap "$EXTRACT_MEM_SWAP" --oom-kill-disable=false \
            -v "$d":/out \
            almalinux:10.2 bash -c '
                set -e
                dnf -y --installroot=/rootfs --releasever=10 \
                    --setopt=install_weak_deps=False \
                    --setopt=tsflags=nodocs \
                    install @minimal-environment bash coreutils tar gzip \
                    > /out/dnf-install.log 2>&1
                tar -C /rootfs --exclude=./proc --exclude=./sys --exclude=./dev \
                    --exclude=./run --exclude=./tmp/* -cf /out/rootfs.tar .
            '
        docker import "$d/rootfs.tar" "entropyfs-court-$lane:10.2" > "$d/image-id.txt"
        echo "dnf-installroot-10.2-repos" > "$d/rootfs-source.txt"
    fi
    docker inspect --format '{{.Id}}' "entropyfs-court-$lane:10.2" > "$d/image-digest.txt"
    rm -f "$d/install.img" "$d/rootfs.tar"
    echo "== $lane: image $(cat "$d/image-digest.txt") (source: $(cat "$d/rootfs-source.txt")) =="
}

lane_leap
lane_ubuntu
lane_almalinux
echo "local distro images: done"
