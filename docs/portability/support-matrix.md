# Support matrix (Phase 12E.8/12E.19)

## Distributions (mandatory enterprise matrix)

| Lane | Image | 12E.8 court | Sealed evidence |
|------|-------|-------------|-----------------|
| AlmaLinux 10.2 minimal | `almalinux:10.2` | **PASS** (0 waivers) | `evidence/portability/distro-court-almalinux-10.2-minimal-*` |
| Ubuntu Server 26.04 LTS minimal | `ubuntu:26.04` | **PASS** (0 waivers) | `evidence/portability/distro-court-ubuntu-26.04-minimal-*` |
| openSUSE Leap 16.0 minimal (SUSE-family) | `opensuse/leap:16.0` | **PASS** (0 waivers) | `evidence/portability/distro-court-leap-16.0-minimal-*` |
| SLES 16 | `registry.suse.com/...` (subscription) | authenticated lane; waived without credentials | documented in `docs/portability/distro-court.md` |

The native development/rolling environment is a separate lane and is
never substituted for the enterprise matrix.

## Build requirements (documented prereq list)

- C toolchain (`gcc`/`g++`, or `clang`): required by Rust and by
  `libublk-rs-sys` (bindgen needs **libclang** at build time).
- `curl` + `ca-certificates`: rustup bootstrap (TLS).
- `make`, `file`, `diffutils`, `util-linux`, `findutils`, `python3`,
  `attr` (court tooling).
- `fuse3` (FUSE mount stage).
- Rust: **rustup-pinned stable toolchain** (never the distribution's
  packaged Rust — the compatibility claim is about the OS/kernel/
  userspace environment, not the distro's compiler version).

## Runtime capabilities (as exercised by the courts)

| Capability | AlmaLinux 10.2 | Ubuntu 26.04 | Leap 16.0 |
|------------|----------------|--------------|-----------|
| io_uring (`UringIo`) | available | available | available |
| FUSE mount | available | available | available |
| SyncIo | reference path | reference path | reference path |

## Architecture

- `x86_64` — mandatory, exercised by the matrix.
- `aarch64` — compile/test where infrastructure permits (not yet
  exercised; recorded as a known lane).

## Frontends

- Engine facade — every lane (the engine smoke is a court stage).
- FUSE — every lane (mounted court stage).
- ublk adapter — kernel-free `ublk bench`; kernel ublk requires
  `CONFIG_BLK_DEV_UBLK` + root (recorded per-environment).
