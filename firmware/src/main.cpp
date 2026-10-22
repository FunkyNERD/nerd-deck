#include <Arduino.h>
#include <lvgl.h>

#include "config.hpp"
#include "display.hpp"
#include "protocol.hpp"
#include "settings.hpp"
#include "ui/ui.hpp"

void setup() {
    // The default 256-byte RX buffer overflows on a stats line while LVGL is
    // rendering, and HWCDC on core 2.x never recovers from that. Must be set
    // before begin().
    Serial.setRxBufferSize(kSerialRxBuffer);
    // USB CDC; baud is ignored on native USB but kept for clarity.
    Serial.begin(115200);

    display::init();
    ui::init();
    ui::applyDisplay(settings::load());
    protocol::init();
}

void loop() {
    protocol::poll();
    ui::setConnected(protocol::isConnected());
    lv_timer_handler();
    delay(5);
}
