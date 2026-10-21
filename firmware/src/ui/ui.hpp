#pragma once

#include <stddef.h>

#include "stats.hpp"

namespace ui {

// Build the tab view (System / Network / Controls). Call after display::init().
void init();

void updateStats(const HostStats& s);
void setButtons(const ButtonDef* defs, size_t count);
// One chunk of a button image in LV_IMG_CF_TRUE_COLOR_ALPHA (3 bytes/px).
// Chunks must arrive in order; the image appears once all `total` bytes are in.
void setButtonImage(const char* id, uint16_t w, uint16_t h, size_t offset, size_t total,
                    const uint8_t* data, size_t len);
void setConnected(bool connected);
void showToast(const char* text, bool ok);

}  // namespace ui
