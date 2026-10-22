#!/usr/bin/env bash
# Builds the firmware and puts the release file in firmware/dist/, with a
# SHA256SUMS file. CI runs this (.github/workflows/firmware.yml); it works
# locally too, with PlatformIO installed.
#
#   firmware/package.sh
#
# The file is the merged image (bootloader, partitions and app, written by
# merge_firmware.py), named nerd-deck-firmware-<version>-<board>-proto<N>.bin.
# The host reads that name (host/src/firmware.rs) to pick firmware that speaks
# its protocol, and checks the version and board inside the image.

set -euo pipefail

FW_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DIST="$FW_DIR/dist"
config="$FW_DIR/src/config.hpp"
pio=${PIO:-$(command -v pio || echo "$HOME/.platformio/penv/bin/pio")}

version=$(sed -n 's/^#define FW_VERSION "\(.*\)"$/\1/p' "$config")
board=$(sed -n 's/^#define FW_BOARD "\(.*\)"$/\1/p' "$config")
proto=$(sed -n 's/.*kProtoVersion = \([0-9]*\);.*/\1/p' "$config")
[[ -n $version && -n $board && -n $proto ]] || { echo "can't read $config" >&2; exit 1; }

"$pio" run -d "$FW_DIR"
image="$FW_DIR/.pio/build/$board/firmware-merged.bin"
grep -qa "nerd-deck-firmware:$board:$version;" "$image" ||
    { echo "$image isn't firmware $version for $board" >&2; exit 1; }

rm -rf "$DIST"
mkdir -p "$DIST"
cp "$image" "$DIST/nerd-deck-firmware-$version-$board-proto$proto.bin"
cd "$DIST"
sha256sum -- *.bin >SHA256SUMS
cat SHA256SUMS
