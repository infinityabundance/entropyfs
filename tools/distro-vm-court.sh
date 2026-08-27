#!/usr/bin/env bash
# EntropyFS distribution court — VM driver (Phase 12E.8).
#
# Runs the 17-stage portability court in ROOT-CAPABLE VMs built from the
# vendor minimal images:
#
#   leap-16.0-minimal     /run/media/one/toshiba4TB/images/Leap-16.0-Minimal-VM.x86_64-Cloud.qcow2
#                         (openSUSE Leap 16.0 — the SUSE-family lane,
#                         the vendor's legally distributable base)
#   ubuntu-26.04-minimal  /run/media/one/toshiba4TB/images/ubuntu-26.04-live-server-amd64.iso
#                         (autoinstall)
#   almalinux-10.2-minimal /run/media/one/toshiba4TB/images/AlmaLinux-10.2-x86_64-boot.iso
#                         (kickstart)
#
# Usage: tools/distro-vm-court.sh [leap-16.0-minimal|ubuntu-26.04-minimal|almalinux-10.2-minimal|all]
#
# Requirements: /dev/kvm, qemu-system-x86_64, qemu-img, genisoimage,
# ssh-keygen, ssh. Evidence archives under
# evidence/portability/distro-vm-court-<lane>-<ts>-<rev>/ with the
# immutable image sha256, boot logs, the inner court's per-stage
# artifacts, waivers, and the sealed evidence-manifest.json.
#
# The inner court (tools/distro-court-inner.sh) is the SAME 17-stage
# script the Docker lanes run; in the VM the capability stages (FUSE
# mount, io_uring) run NATIVELY — a VM is the stronger court.

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

IMAGES="/run/media/one/toshiba4TB/images"
REV="$(git rev-parse --short HEAD 2>/dev/null || echo norev)"
EVIDENCE_ROOT="evidence/portability"
COURT="${1:-all}"
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=10)

mkdir -p "$EVIDENCE_ROOT"

sha256_of() { sha256sum "$1" | cut -d' ' -f1; }

# ---------------------------------------------------------------------------
# seed helpers (cloud-init NoCloud)
# ---------------------------------------------------------------------------
make_seed() {
    # $1 = seed dir, $2 = output iso
    local seed_dir="$1" out="$2"
    (cd "$seed_dir" && genisoimage -quiet -o "$out" -V cidata -R -J . >/dev/null 2>&1)
}

seed_for_cloudinit() {
    # $1 = lane, $2 = workdir, $3 = pubkey — the plain cloud-init
    # user-data seed (first boot of a cloud image / installed system).
    local lane="$1" wd="$2" pubkey="$3"
    local sdir="$wd/seed"
    rm -rf "$sdir"; mkdir -p "$sdir"
    {
        echo "#cloud-config"
        echo "users:"
        echo "  - name: entropy"
        echo "    gecos: EntropyFS Court"
        echo "    sudo: ALL=(ALL) NOPASSWD:ALL"
        echo "    shell: /bin/bash"
        echo "    ssh_authorized_keys:"
        echo "      - $(cat "$pubkey")"
        echo "ssh_pwauth: false"
        echo "disable_root: false"
        echo "package_update: false"
    } > "$sdir/user-data"
    {
        echo "instance-id: iid-entropyfs-court-$lane"
        echo "local-hostname: entropyfs-$lane"
    } > "$sdir/meta-data"
    make_seed "$sdir" "$wd/seed.iso"
}

seed_for_autoinstall() {
    # $1 = lane, $2 = workdir, $3 = autoinstall yaml: user-data holds the
    # subiquity autoinstall stanza (plus the cloud-config header); the
    # installed system is configured by the installer's own identity/ssh.
    local lane="$1" wd="$2" ai="$3"
    local sdir="$wd/seed"
    rm -rf "$sdir"; mkdir -p "$sdir"
    { echo "#cloud-config"; cat "$ai"; } > "$sdir/user-data"
    {
        echo "instance-id: iid-entropyfs-court-$lane"
        echo "local-hostname: entropyfs-$lane"
    } > "$sdir/meta-data"
    make_seed "$sdir" "$wd/seed.iso"
}

seed_for_kickstart() {
    # $1 = lane, $2 = workdir, $3 = pubkey, $4 = ks.cfg path: cloud-init
    # user-data (harmless for anaconda) PLUS ks.cfg at the seed root
    # (anaconda's inst.ks=cdrom:/ks.cfg).
    local lane="$1" wd="$2" pubkey="$3" ks="$4"
    seed_for_cloudinit "$lane" "$wd" "$pubkey"
    cp "$ks" "$wd/seed/ks.cfg"
    make_seed "$wd/seed" "$wd/seed.iso"
}

# ---------------------------------------------------------------------------
# qemu helpers
# ---------------------------------------------------------------------------
vm_start() {
    # $1 = workdir, $2 = port, $3... = extra qemu args. The serial log
    # goes to a file (daemonized, display-less).
    local wd="$1" port="$2"; shift 2
    qemu-system-x86_64 -enable-kvm -m 4096 -smp 4 \
        -drive file="$wd/disk.qcow2,if=virtio" \
        -drive file="$wd/seed.iso,format=raw,if=virtio" \
        -netdev user,id=net0,hostfwd=tcp:127.0.0.1:$port-:22 \
        -device virtio-net-pci,netdev=net0 \
        -display none -serial "file:$wd/serial.log" \
        -daemonize -pidfile "$wd/qemu.pid" "$@"
}

vm_wait_ssh() {
    # $1 = port, $2 = timeout seconds
    local port="$1" t="$2" i=0
    while [[ $i -lt $t ]]; do
        if ssh "${SSH_OPTS[@]}" -p "$port" entropy@127.0.0.1 true >/dev/null 2>&1; then
            return 0
        fi
        sleep 2; i=$((i + 2))
    done
    return 1
}

vm_stop() {
    local wd="$1" port="$2"
    ssh "${SSH_OPTS[@]}" -p "$port" entropy@127.0.0.1 "sudo poweroff" >/dev/null 2>&1 || true
    sleep 3
    kill "$(cat "$wd/qemu.pid" 2>/dev/null)" 2>/dev/null || true
    rm -f "$wd/qemu.pid"
}

# ---------------------------------------------------------------------------
# The court run (shared by all lanes once SSH is up)
# ---------------------------------------------------------------------------
run_court_in_vm() {
    # $1 = lane, $2 = wd, $3 = port, $4 = image digest
    local lane="$1" wd="$2" port="$3" digest="$4"
    echo "  transferring repo + running the 17-stage court in the VM ..."
    # Transfer the exact revision (excluding the host target/ and the
    # growing evidence archive).
    tar --exclude=./target --exclude=./evidence -C "$REPO_ROOT" -cf - . \
        | ssh "${SSH_OPTS[@]}" -p "$port" entropy@127.0.0.1 \
            "mkdir -p /home/entropy/entropyfs && tar -C /home/entropy/entropyfs -xf -"
    # Run the inner court as root (FUSE mount + mkfs + uring probes run
    # natively in the VM).
    ssh "${SSH_OPTS[@]}" -p "$port" entropy@127.0.0.1 \
        "sudo env COURT_REPO=/home/entropy/entropyfs \
             COURT_OUT=/home/entropy/court-out \
             COURT_IMAGE_DIGEST=$digest \
             bash /home/entropy/entropyfs/tools/distro-court-inner.sh $lane" \
        > "$wd/inner-court.log" 2>&1 || {
        echo "  lane $lane: FAILED (a runnable stage failed — not a waiver)"
        tail -30 "$wd/inner-court.log"
        return 1
    }
    # Collect the evidence.
    mkdir -p "$wd/evidence"
    scp "${SSH_OPTS[@]}" -p "$port" -r entropy@127.0.0.1:/home/entropy/court-out/court \
        "$wd/evidence/" >/dev/null 2>&1 || true
    return 0
}

# ---------------------------------------------------------------------------
# Lanes
# ---------------------------------------------------------------------------
lane_leap() {
    local lane="leap-16.0-minimal"
    local img="$IMAGES/Leap-16.0-Minimal-VM.x86_64-Cloud.qcow2"
    local ts out wd port
    ts="$(date +%s)"
    out="$REPO_ROOT/$EVIDENCE_ROOT/distro-vm-court-$lane-$ts-$REV"
    wd="$out/work"
    port=$((22000 + (ts % 1000)))
    mkdir -p "$wd"
    echo "== lane $lane =="
    local digest
    digest="sha256:$(sha256_of "$img")"
    echo "$digest" > "$out/base-image-digest.txt"
    echo "base image: $img ($digest)"

    # Cloud image: overlay on the pristine base; cloud-init seed with the
    # court's SSH key.
    ssh-keygen -t ed25519 -N '' -f "$wd/id_court" -q
    seed_for_cloudinit "$lane" "$wd" "$wd/id_court.pub"
    qemu-img create -f qcow2 -b "$img" -F qcow2 "$wd/disk.qcow2" >/dev/null

    vm_start "$wd" "$port"
    if ! vm_wait_ssh "$port" 300; then
        echo "  lane $lane: VM did not become reachable over ssh"
        vm_stop "$wd" "$port"
        return 1
    fi
    run_court_in_vm "$lane" "$wd" "$port" "$digest"
    vm_stop "$wd" "$port"
    echo "lane $lane: sealed at $out"
}

lane_ubuntu() {
    local lane="ubuntu-26.04-minimal"
    local img="$IMAGES/ubuntu-26.04-live-server-amd64.iso"
    local ts out wd port
    ts="$(date +%s)"
    out="$REPO_ROOT/$EVIDENCE_ROOT/distro-vm-court-$lane-$ts-$REV"
    wd="$out/work"
    port=$((22100 + (ts % 1000)))
    mkdir -p "$wd"
    echo "== lane $lane =="
    local digest
    digest="sha256:$(sha256_of "$img")"
    echo "$digest" > "$out/base-image-digest.txt"

    ssh-keygen -t ed25519 -N '' -f "$wd/id_court" -q
    # Autoinstall seed: the subiquity autoinstall stanza + the court user.
    cat > "$wd/autoinstall.yml" <<EOF
autoinstall:
  version: 1
  locale: en_US
  keyboard:
    layout: us
  identity:
    hostname: entropyfs-ubuntu
    username: entropy
    password: "courtnopw"
  ssh:
    install-server: true
    allow-pw: false
    authorized-keys:
      - $(cat "$wd/id_court.pub")
  storage:
    layout:
      name: direct
  packages: [gcc, g++, make, curl, file, diffutils, python3, attr, fuse3, clang, libclang-dev]
  late-commands:
    - curtin in-target -- passwd --delete entropy
EOF
    seed_for_autoinstall "$lane" "$wd" "$wd/autoinstall.yml"
    qemu-img create -f qcow2 "$wd/disk.qcow2" 32G >/dev/null

    # Phase A: run the installer from the ISO; it installs to disk.qcow2
    # and powers off when done.
    qemu-system-x86_64 -enable-kvm -m 4096 -smp 4 \
        -cdrom "$img" \
        -drive file="$wd/disk.qcow2,if=virtio" \
        -drive file="$wd/seed.iso,format=raw,if=virtio" \
        -netdev user,id=net0,hostfwd=tcp:127.0.0.1:$port-:22 \
        -device virtio-net-pci,netdev=net0 \
        -display none -serial "file:$wd/install-serial.log" \
        -daemonize -pidfile "$wd/qemu-install.pid" >/dev/null 2>&1
    echo "  installer running (autoinstall) ..."
    local i=0
    while [[ $i -lt 900 ]]; do
        if ! kill -0 "$(cat "$wd/qemu-install.pid")" 2>/dev/null; then
            break
        fi
        sleep 5; i=$((i + 5))
    done
    kill "$(cat "$wd/qemu-install.pid" 2>/dev/null)" 2>/dev/null || true

    # Phase B: boot the installed system (same seed for the court user).
    vm_start "$wd" "$port"
    if ! vm_wait_ssh "$port" 300; then
        echo "  lane $lane: installed system did not become reachable"
        vm_stop "$wd" "$port"
        return 1
    fi
    run_court_in_vm "$lane" "$wd" "$port" "$digest"
    vm_stop "$wd" "$port"
    echo "lane $lane: sealed at $out"
}

lane_almalinux() {
    local lane="almalinux-10.2-minimal"
    local img="$IMAGES/AlmaLinux-10.2-x86_64-boot.iso"
    local ts out wd port
    ts="$(date +%s)"
    out="$REPO_ROOT/$EVIDENCE_ROOT/distro-vm-court-$lane-$ts-$REV"
    wd="$out/work"
    port=$((22200 + (ts % 1000)))
    mkdir -p "$wd"
    echo "== lane $lane =="
    local digest
    digest="sha256:$(sha256_of "$img")"
    echo "$digest" > "$out/base-image-digest.txt"

    ssh-keygen -t ed25519 -N '' -f "$wd/id_court" -q
    # Kickstart: unattended install of the minimal system + the court's
    # documented build/runtime prereqs + the ssh key. The boot.iso is a
    # NETWORK install media: packages come from the public AlmaLinux 10
    # BaseOS mirror (recorded in the lane evidence).
    cat > "$wd/ks.cfg" <<EOF
#version=RHEL10
lang en_US.UTF-8
keyboard us
timezone UTC --isUtc
rootpw --plaintext courtenospw
user --name=entropy --groups=wheel --plaintext courtenospw
sshkey --username=entropy "$(cat "$wd/id_court.pub")"
network --bootproto=dhcp --device=link --activate
services --enabled=sshd
zerombr
clearpart --all --initlabel
autopart --type=lvm
url --url="https://repo.almalinux.org/almalinux/10/BaseOS/x86_64/os/"
%packages
@minimal-environment
gcc
gcc-c++
make
curl
file
diffutils
util-linux
python3
attr
fuse3
clang
clang-devel
%end
reboot
EOF
    seed_for_kickstart "$lane" "$wd" "$wd/id_court.pub" "$wd/ks.cfg"
    qemu-img create -f qcow2 "$wd/disk.qcow2" 32G >/dev/null

    # Extract the boot.iso's kernel + initrd (direct kernel boot: qemu's
    # -kernel/-initrd/-append) — the boot.iso's isolinux menu cannot take
    # extra args under -kernel-append.
    xorriso -osirrox on -indev "$img" \
        -extract /images/pxeboot/vmlinuz "$wd/vmlinuz" \
        -extract /images/pxeboot/initrd.img "$wd/initrd.img" >/dev/null 2>&1

    # Phase A: run the installer (direct kernel boot with inst.ks); it
    # installs to disk.qcow2 and reboots when done.
    qemu-system-x86_64 -enable-kvm -m 4096 -smp 4 \
        -kernel "$wd/vmlinuz" -initrd "$wd/initrd.img" \
        -append "inst.ks=cdrom:/ks.cfg inst.repo=https://repo.almalinux.org/almalinux/10/BaseOS/x86_64/os/ console=ttyS0" \
        -drive file="$wd/disk.qcow2,if=virtio" \
        -drive file="$wd/seed.iso,format=raw,if=virtio" \
        -netdev user,id=net0,hostfwd=tcp:127.0.0.1:$port-:22 \
        -device virtio-net-pci,netdev=net0 \
        -display none -serial "file:$wd/install-serial.log" \
        -daemonize -pidfile "$wd/qemu-install.pid" >/dev/null 2>&1
    echo "  installer running (kickstart) ..."
    local i=0
    while [[ $i -lt 1200 ]]; do
        if ! kill -0 "$(cat "$wd/qemu-install.pid")" 2>/dev/null; then
            break
        fi
        sleep 5; i=$((i + 5))
    done
    kill "$(cat "$wd/qemu-install.pid" 2>/dev/null)" 2>/dev/null || true

    vm_start "$wd" "$port"
    if ! vm_wait_ssh "$port" 300; then
        echo "  lane $lane: installed system did not become reachable"
        vm_stop "$wd" "$port"
        return 1
    fi
    run_court_in_vm "$lane" "$wd" "$port" "$digest"
    vm_stop "$wd" "$port"
    echo "lane $lane: sealed at $out"
}

case "$COURT" in
leap-16.0-minimal) lane_leap ;;
ubuntu-26.04-minimal) lane_ubuntu ;;
almalinux-10.2-minimal) lane_almalinux ;;
all)
    lane_leap
    lane_ubuntu
    lane_almalinux
    ;;
*)
    echo "unknown lane: $COURT" >&2
    exit 1
    ;;
esac
echo "distro VM court: done"
