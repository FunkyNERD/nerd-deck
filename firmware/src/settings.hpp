#pragma once

#include <stdint.h>

// Settings from the host's `display` message, kept in flash so they apply from
// power-on, before the host connects. See docs/PROTOCOL.md.
namespace settings {

// The tabs by their protocol names, in their default order. ui.cpp's Tab
// enum follows this order.
constexpr uint8_t kTabCount = 5;
constexpr const char* kTabIds[kTabCount] = {"system", "network", "storage", "battery", "controls"};
constexpr int8_t kStartFirst = -1;

struct Display {
    uint8_t tabs[kTabCount];  // tab at each position
    int8_t start;             // a tab, or kStartFirst
    uint8_t brightness;       // percent
    uint32_t dimAfterS;       // 0 = never
    uint32_t offAfterS;       // on the waiting screen; 0 = never
};

// The tab named `id`, or -1.
int8_t tabFromName(const char* id);

// The stored settings, or the defaults.
Display load();
// Stores `d` if it differs from what's stored.
void save(const Display& d);

}  // namespace settings
