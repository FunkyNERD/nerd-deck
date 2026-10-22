#pragma once

#include <stddef.h>
#include <stdint.h>

#define FW_VERSION "0.5.1"
// Sent in `hello`. The host only installs its bundled firmware on this board.
#define FW_BOARD "wt32-sc01-plus"

// Bump together with PROTO_VERSION in host/src/protocol.rs.
constexpr uint32_t kProtoVersion = 5;

constexpr int kScreenWidth = 480;
constexpr int kScreenHeight = 320;

// Host is considered gone if nothing arrives for this long.
constexpr uint32_t kHostTimeoutMs = 3000;

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
