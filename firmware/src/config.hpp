#pragma once

#include <stddef.h>
#include <stdint.h>

#define FW_VERSION "0.4.0"
// Sent in `hello`. The host only installs its bundled firmware on this board.
#define FW_BOARD "wt32-sc01-plus"

// Bump together with PROTO_VERSION in host/src/protocol.rs.
constexpr uint32_t kProtoVersion = 3;

constexpr int kScreenWidth = 480;
constexpr int kScreenHeight = 320;

// Host is considered gone if nothing arrives for this long.
constexpr uint32_t kHostTimeoutMs = 3000;

constexpr size_t kMaxLineLen = 2048;
constexpr size_t kMaxCores = 32;
constexpr size_t kMaxDisks = 4;
constexpr size_t kMaxButtons = 12;

// Button images: host scales to 40x40; accept a little more for headroom.
constexpr uint16_t kMaxImageSide = 64;
// Decoded bytes per `image` message (host sends 1024).
constexpr size_t kMaxImageChunk = 1536;
