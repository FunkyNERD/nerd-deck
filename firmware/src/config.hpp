#pragma once

#include <stddef.h>
#include <stdint.h>

#define FW_VERSION "0.1.0"

// Bump together with PROTO_VERSION in host/src/protocol.rs.
constexpr uint32_t kProtoVersion = 1;

constexpr int kScreenWidth = 480;
constexpr int kScreenHeight = 320;

// Host is considered gone if nothing arrives for this long.
constexpr uint32_t kHostTimeoutMs = 3000;

constexpr size_t kMaxLineLen = 2048;
constexpr size_t kMaxCores = 32;
constexpr size_t kMaxDisks = 4;
constexpr size_t kMaxButtons = 12;
