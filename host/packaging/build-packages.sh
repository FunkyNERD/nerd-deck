#!/usr/bin/env bash
# Build the host app's .deb and .rpm into host/dist/, with a SHA256SUMS file.
# CI runs this (.github/workflows/host-packages.yml); it works locally too.
#
#   packaging/build-packages.sh
#
# Needs cargo-deb and cargo-generate-rpm:
#   cargo install --locked cargo-deb cargo-generate-rpm
#
# Package metadata lives in Cargo.toml ([package.metadata.deb] and
# [package.metadata.generate-rpm]). Build on the oldest distro you want to
# support: the binary needs at least the glibc it was built against.

set -euo pipefail

HOST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="$HOST_DIR/dist"
cd "$HOST_DIR"

cargo build --release --locked

rm -rf "$DIST"
mkdir -p "$DIST"
cargo deb --no-build --locked --output "$DIST/"
cargo generate-rpm --output "$DIST/"

cd "$DIST"
sha256sum -- *.deb *.rpm >SHA256SUMS
cat SHA256SUMS
