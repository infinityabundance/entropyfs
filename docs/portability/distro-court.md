# Distribution court (Phase 12E.8)

The **hard release gate** of the adoption phase: EntropyFS must build,
test, install, create a store, mount where FUSE is possible, execute a
POSIX smoke battery, unmount, fsck, and reopen on minimal enterprise
distributions running in Docker.

A distribution does not pass merely because Rust compiles. The gate is
the full stage list below.

## Mandatory matrix (Docker Hub images)

| Lane | Image | Family |
|------|-------|--------|
| `almalinux-10.2-minimal` | `almalinux:10.2` | RHEL/Alma enterprise |
| `ubuntu-26.04-minimal` | `ubuntu:26.04` | Debian/Ubuntu enterprise |
| `leap-16.0-minimal` | `opensuse/leap:16.0` | SUSE enterprise family |

`opensuse/leap:16.0` is the SUSE-family lane — the vendor's legally
distributable base image from the public registry. The native
development/rolling environment remains a separate test lane — it is
never substituted for the enterprise matrix.

Images extracted from the provided vendor OS artifacts
(`tools/docker/build-distro-images.sh`: Leap installer ISO LiveOS
rootfs, Ubuntu live-server `ubuntu-server-minimal.squashfs`, AlmaLinux
boot.iso installer env) remain available as the offline/vendor-artifact
lane; the Docker Hub images are the primary lanes.

## The 18 stages

```text
 1  pristine minimal image (pulled fresh, digest recorded)
 2  only documented build/runtime prerequisites installed
 3  rustup pinned toolchain (never the distro's packaged Rust)
 4  cargo build --release --locked (rlib + cdylib)
 5  selected release courts (engine, persistent store, compat seal,
    golden stores, fsck)
 6  cargo install --path . --locked
 7  entropyfs mkfs
 8  library Engine API smoke (examples/engine_smoke)
 9  SyncIo smoke
10  UringIo capability detection + smoke where the runtime permits
11  FUSE mount when /dev/fuse + privileges exist
12  POSIX smoke (mkdir/create/write/read/rename/hardlink/symlink/
    truncate/xattr/fsync/concurrent writers)
13  unmount
14  fsck --json
15  reopen + exact-hash verification
16  compact / GC
17  reopen + fsck again
18  Go binding court (12E.15): pinned upstream Go 1.24.6, then
    go vet + go test + the mandatory `go test -race` gate against the
    cdylib (never the distro's packaged Go)
```

## Running

```bash
tools/distro-court.sh all                     # almalinux + ubuntu + leap
tools/distro-court.sh ubuntu-26.04-minimal    # one lane
```

Environment:

- `COURT_MEM` / `COURT_MEM_SWAP` — the **OOM limiter** (default `4g` /
  `6g`): every court container runs under a hard memory cap so a
  pathological build/court is killed by the kernel, never allowed to
  exhaust the host. This is a hard requirement of the court.

Evidence per run: `evidence/portability/distro-court-<lane>-<ts>-<rev>/`
with the immutable base-image digest (`base-image-digest.txt`), per-stage
logs, capability probes, waivers, `fsck.json`, hash manifests,
`court-result.json`, the OOM-limiter record, and the sealed
`evidence-manifest.json` (written by the container's own binary; the
revision is patched in by the driver — the containers have no git).

## Capability waivers

The court distinguishes **implementation failure** from **container
capability unavailable** (`/dev/fuse` absent, io_uring blocked by
seccomp, mount privileges denied). Environment-only impossibilities
become explicit waivers (`waivers/<stage>.txt` with the exact probe
command, exact output, exact error, and the host/runtime requirement to
clear it). Actual EntropyFS failures are never converted to waivers —
they fail the lane. The current matrix (almalinux 10.2 / ubuntu 26.04 /
Leap 16.0, Docker Hub images, OOM-limited) passes every stage with
**zero waivers**: io_uring and FUSE mount both run natively in the
privileged containers.

## Packaging assumptions the court exposed (and fixed)

- `libublk-rs-sys` builds C bindings with `bindgen`, which needs
  **libclang at build time** — an undocumented prerequisite; `clang` /
  `clang-devel` is now part of the documented prereq list.
- The minimal Ubuntu image ships **no CA trust store**: `curl` cannot
  verify TLS, silently downloading nothing and leaving `rustc --version`
  failing with 127; `ca-certificates` is a documented prereq.
- `opensuse/leap:16.0` lacks `xargs`; the court's own tools are
  shell-native and `findutils` is a documented prereq.
- Imported minimal rootfs images may lack `/tmp` (a tmpfs mountpoint in
  the live environment); extraction excludes only its contents.

## Docker/VM infrastructure note

The court's containers run `--privileged --device /dev/fuse` so the
capability probes (FUSE mount, io_uring) can run natively; where a
runtime still cannot provide them, waivers are recorded. The same
17-stage inner script (`tools/distro-court-inner.sh`) is reused by any
root-capable VM lane.
