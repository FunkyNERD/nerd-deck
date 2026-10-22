#include "display.hpp"

#include <Arduino.h>
#include <esp_heap_caps.h>
#include <lvgl.h>

// Each build environment in platformio.ini names its board.
#if defined(BOARD_WAVESHARE_C5_TOUCH_35)
#include "board/waveshare_c5_touch_35.hpp"
#else
#include "board/wt32_sc01_plus.hpp"
#endif
#include "config.hpp"

namespace display {
namespace {

constexpr int kBufLines = 40;

board::Lcd tft;
lv_disp_draw_buf_t drawBuf;
lv_disp_drv_t dispDrv;
lv_indev_drv_t indevDrv;

void flush(lv_disp_drv_t* drv, const lv_area_t* area, lv_color_t* px) {
    const uint32_t w = area->x2 - area->x1 + 1;
    const uint32_t h = area->y2 - area->y1 + 1;
    tft.pushImage(area->x1, area->y1, w, h, reinterpret_cast<lgfx::swap565_t*>(&px->full));
    lv_disp_flush_ready(drv);
}

void readTouch(lv_indev_drv_t*, lv_indev_data_t* data) {
    uint16_t x, y;
    if (tft.getTouch(&x, &y)) {
        data->state = LV_INDEV_STATE_PR;
        data->point.x = x;
        data->point.y = y;
    } else {
        data->state = LV_INDEV_STATE_REL;
    }
}

#ifdef NERD_PERF
// Build with -DNERD_PERF to print refresh stats over Serial once a second.
// The host ignores non-JSON lines, so this is safe to leave on while testing.
void monitor(lv_disp_drv_t*, uint32_t ms, uint32_t px) {
    static uint32_t windowStart = 0, frames = 0, busyMs = 0, worstMs = 0, pixels = 0;
    frames++;
    busyMs += ms;
    pixels += px;
    if (ms > worstMs) worstMs = ms;
    const uint32_t now = millis();
    if (now - windowStart >= 1000) {
        Serial.printf("perf fps=%u busy=%u%% worst=%ums px/frame=%u\n", frames,
                      busyMs * 100 / (now - windowStart), worstMs, frames ? pixels / frames : 0);
        windowStart = now;
        frames = busyMs = worstMs = pixels = 0;
    }
}
#endif

}  // namespace

void init() {
    board::powerUp();
    tft.init();
    tft.setRotation(1);  // landscape, USB on the left; use 3 to flip
    tft.setBrightness(200);
    tft.fillScreen(TFT_BLACK);

    lv_init();

    const size_t px = kScreenWidth * kBufLines;
    auto* buf = static_cast<lv_color_t*>(
        heap_caps_malloc(px * sizeof(lv_color_t), MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL));
    lv_disp_draw_buf_init(&drawBuf, buf, nullptr, px);

    lv_disp_drv_init(&dispDrv);
    dispDrv.hor_res = kScreenWidth;
    dispDrv.ver_res = kScreenHeight;
    dispDrv.flush_cb = flush;
    dispDrv.draw_buf = &drawBuf;
#ifdef NERD_PERF
    dispDrv.monitor_cb = monitor;
#endif
    lv_disp_drv_register(&dispDrv);

    lv_indev_drv_init(&indevDrv);
    indevDrv.type = LV_INDEV_TYPE_POINTER;
    indevDrv.read_cb = readTouch;
    lv_indev_drv_register(&indevDrv);
}

void setBrightness(uint8_t level) { tft.setBrightness(level); }

}  // namespace display
