#pragma once

#include <stddef.h>

#include "settings.hpp"
#include "stats.hpp"

namespace ui {

// Build the tab view (System / Network / Storage / Battery / Controls). Call after display::init().
void init();

// Tab order, the tab to open on, brightness and power saving. Call once at
// start with the stored settings, then for each `display` message.
void applyDisplay(const settings::Display& d);
void updateStats(const HostStats& s);
void setButtons(const ButtonDef* defs, size_t count);
// One chunk of a button image in LV_IMG_CF_TRUE_COLOR_ALPHA (3 bytes/px).
// Chunks must arrive in order; the image appears once all `total` bytes are in.
void setButtonImage(const char* id, uint16_t w, uint16_t h, size_t offset, size_t total,
                    const uint8_t* data, size_t len);
void setConnected(bool connected);
// A Controls button's face in pixels, inside its border. Images this size
// fill the button.
void buttonTileSize(uint16_t* w, uint16_t* h);
void showToast(const char* text, bool ok);
// Host notice, such as an available update. Shares the toast pill.
void showNotice(const char* text);
// Desktop notification card. A critical one stays until tapped.
void showNotification(const char* app, const char* summary, const char* body, bool critical);

}  // namespace ui
