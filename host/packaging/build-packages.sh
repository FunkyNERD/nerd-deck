#!/usr/bin/env bash
# Build the host app's .deb and .rpm into host/dist/, with a SHA256SUMS file.
# CI runs this (.github/workflows/host-packages.yml); it works locally too.
#
#   packaging/build-packages.sh
#
# The packages don't carry the device firmware. Set FIRMWARE_IMAGE to a merged
# image (firmware/.pio/build/wt32-sc01-plus/firmware-merged.bin) to put it in
# dist/ beside them, as its own release asset named
# nerd-deck-firmware-<version>-<board>-proto<N>.bin, which the host downloads
# (host/src/firmware.rs reads the name).
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

if [[ -n ${FIRMWARE_IMAGE:-} ]]; then
    config="$HOST_DIR/../firmware/src/config.hpp"
    fw_version=$(sed -n 's/^#define FW_VERSION "\(.*\)"$/\1/p' "$config")
    fw_board=$(sed -n 's/^#define FW_BOARD "\(.*\)"$/\1/p' "$config")
    fw_proto=$(sed -n 's/.*kProtoVersion = \([0-9]*\);.*/\1/p' "$config")
    [[ -n $fw_version && -n $fw_board && -n $fw_proto ]] || { echo "can't read $config" >&2; exit 1; }
    grep -qa "nerd-deck-firmware:$fw_board:$fw_version;" "$FIRMWARE_IMAGE" ||
        { echo "$FIRMWARE_IMAGE isn't firmware $fw_version for $fw_board" >&2; exit 1; }
    cp "$FIRMWARE_IMAGE" "$DIST/nerd-deck-firmware-$fw_version-$fw_board-proto$fw_proto.bin"
fi

cd "$DIST"
sha256sum -- * >SHA256SUMS
cat SHA256SUMS
