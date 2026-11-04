#pragma once

#include <stddef.h>
#include <stdint.h>

// Settings from the host's `display` message, kept in flash so they apply from
// power-on, before the host connects. See docs/PROTOCOL.md.
namespace settings {

// The built-in tabs by their protocol names, in their default order. ui.cpp's
// Tab enum follows this order.
constexpr uint8_t kBuiltinTabs = 5;
constexpr const char* kTabIds[kBuiltinTabs] = {"system", "network", "storage", "battery", "controls"};
// Slots for the host's custom tabs (protocol v7), numbered after the built-in
// tabs. Each keeps its tab's id, so the tab keeps its place and on/off.
constexpr uint8_t kCustomTabs = 3;
constexpr uint8_t kTabCount = kBuiltinTabs + kCustomTabs;
constexpr size_t kCustomIdLen = 16;  // with the NUL
constexpr int8_t kStartFirst = -1;
// A bit per tab, by its number.
constexpr uint8_t kAllTabs = (1u << kTabCount) - 1;

struct Display {
    uint8_t tabs[kTabCount];  // tab at each position
    int8_t start;             // a tab, or kStartFirst
    uint8_t brightness;       // percent
    uint32_t dimAfterS;       // 0 = never
    uint32_t offAfterS;       // on the waiting screen; 0 = never
    uint8_t tempWarm, tempHot;  // °C: the CPU temperature shows orange, then red
    bool flip;                  // upside down, for the USB cable on the other side
    uint8_t hidden;             // tabs turned off, a bit each
    char custom[kCustomTabs][kCustomIdLen];  // each slot's custom tab, by id; "" when free
};

// The tab named `id`: a built-in one, or the custom slot holding it; else -1.
int8_t tabFromName(const Display& d, const char* id);
// Tab `t`'s name, or nullptr for a free custom slot.
const char* tabName(const Display& d, uint8_t t);
// Gives each of `ids` a custom slot: the one it had, else a free one. Slots
// whose tab is gone are freed. Ids past the slots are left out. Returns
// whether `d` changed.
bool assignCustom(Display& d, const char* const* ids, uint8_t count);

// The stored settings, or the defaults.
Display load();
// Stores `d` if it differs from what's stored.
void save(const Display& d);

}  // namespace settings
