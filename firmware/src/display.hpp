#pragma once

#include <stdint.h>

namespace display {

// Bring up the panel, touch and LVGL drivers. Call before ui::init().
void init();

void setBrightness(uint8_t level);

}  // namespace display
