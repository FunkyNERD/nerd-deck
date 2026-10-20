#pragma once

#include <stddef.h>

#include "stats.hpp"

namespace ui {

// Build the tab view (System / Network / Controls). Call after display::init().
void init();

void updateStats(const HostStats& s);
void setButtons(const ButtonDef* defs, size_t count);
void setConnected(bool connected);
void showToast(const char* text, bool ok);

}  // namespace ui
