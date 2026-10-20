#include "display.hpp"

#include <Arduino.h>
#include <esp_heap_caps.h>
#include <lvgl.h>

#include "board/wt32_sc01_plus.hpp"
#include "config.hpp"

namespace display {
namespace {

constexpr int kBufLines = 40;

LGFX_WT32SC01Plus tft;
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

}  // namespace

void init() {
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
    lv_disp_drv_register(&dispDrv);

    lv_indev_drv_init(&indevDrv);
    indevDrv.type = LV_INDEV_TYPE_POINTER;
    indevDrv.read_cb = readTouch;
    lv_indev_drv_register(&indevDrv);
}

void setBrightness(uint8_t level) { tft.setBrightness(level); }

}  // namespace display
