# MSRV and Rust toolchain policy (Phase 12E.9)

## The rule

EntropyFS declares `rust-version = "1.87"` in `Cargo.toml`. The **declared
MSRV** and the **current stable** are tested separately
(`tools/check-msrv.sh`) and are deliberately **never conflated with any
distribution's packaged Rust**.

The compatibility claim of the distribution courts (12E.8) is about the
**OS / kernel / userspace environment** — not whatever Rust compiler a
distro repository happens to ship. The court installs a rustup-pinned
stable toolchain in every minimal image; a distro's packaged Rust being
older than the MSRV is a packaging reality, never a reason to lower the
MSRV.

## What is tested

| Toolchain | Scope |
|-----------|-------|
| `1.87.0` (declared MSRV) | `cargo check --all-targets` (default + `--no-default-features`) |
| stable (current) | same, plus the full release courts |

## Consequences

- If a distro's packaged Rust is too old: vendor system packages +
  rustup toolchain (documented in `docs/portability/support-matrix.md`).
- The MSRV is raised only by an explicit decision with evidence (e.g. a
  dependency requiring it); it is never silently drifted.
