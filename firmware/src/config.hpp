#pragma once

#include <stddef.h>
#include <stdint.h>

#define FW_VERSION "0.8.1"
// Sent in `hello`, and the name of the build environment in platformio.ini
// that sets it. The host only installs firmware built for the deck's board.
#ifndef FW_BOARD
#error "FW_BOARD comes from the build environment's flags (platformio.ini)"
#endif

// Bump together with PROTO_VERSION in host/src/protocol.rs.
constexpr uint32_t kProtoVersion = 6;

constexpr int kScreenWidth = 480;
constexpr int kScreenHeight = 320;

// The host is taken for gone when nothing arrives for this long, or for its
// stats interval (from `display`) plus kHostTimeoutSlackMs if that's longer.
constexpr uint32_t kHostTimeoutMs = 3000;
constexpr uint32_t kHostTimeoutSlackMs = 2000;

constexpr size_t kMaxLineLen = 2048;
// Serial receive buffer: holds a whole button image (~36 KB of lines) if the
// UI stalls while it arrives; the host waits for each image to land.
constexpr size_t kSerialRxBuffer = 40 * 1024;
constexpr size_t kMaxCores = 32;
constexpr size_t kMaxDisks = 4;
constexpr size_t kMaxButtons = 12;

// Button images: the host sizes them to fill a button's face (about 108x81,
// reported in `hello`), or to 40x40 icons for older hosts.
constexpr uint16_t kMaxImageSide = 160;
// Decoded bytes per `image` message (host sends 1024).
constexpr size_t kMaxImageChunk = 1536;
