#!/usr/bin/env bash
# Phase 12E.9: MSRV + toolchain policy gate.
#
# Tests the DECLARED MSRV (Cargo.toml rust-version = 1.87) and the
# current stable SEPARATELY — never conflated with any distribution's
# packaged Rust. The compatibility claim is about the OS/kernel/userspace
# environment, not whatever Rust compiler a distro repository happens to
# ship (the distro courts install a rustup-pinned stable toolchain).
#
# Usage: tools/check-msrv.sh

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

MSRV="$(grep -E '^rust-version' Cargo.toml | head -1 | sed 's/.*= *"\(.*\)"/\1/')"
echo "declared MSRV: $MSRV"
echo "current stable: $(rustc --version)"

if ! rustup toolchain list | grep -q "^${MSRV}-"; then
    echo "installing $MSRV toolchain ..."
    rustup toolchain install "$MSRV" --profile minimal
fi

echo "== $MSRV (declared MSRV) =="
cargo +"$MSRV" check --all-targets
cargo +"$MSRV" check --all-targets --no-default-features

echo "== stable (current) =="
cargo check --all-targets
cargo check --all-targets --no-default-features

echo "MSRV policy: OK"
