#include "ui.hpp"

#include <esp_heap_caps.h>
#include <lvgl.h>
#include <stdio.h>
#include <string.h>

#include "config.hpp"
#include "protocol.hpp"

namespace ui {
namespace {

constexpr size_t kNetHistory = 60;  // visible points
constexpr lv_coord_t kNetChartMax = 1000;

// Value changes ease over this long.
constexpr uint32_t kAnimMs = 500;

const lv_color_t kRxColor = lv_palette_main(LV_PALETTE_CYAN);
const lv_color_t kTxColor = lv_palette_main(LV_PALETTE_ORANGE);

// System tab
lv_obj_t* cpuArc;
lv_obj_t* cpuLabel;
lv_obj_t* cpuInfoLabel;
lv_obj_t* coreBars[kMaxCores];
lv_obj_t* memBar;
lv_obj_t* memLabel;
lv_obj_t* swapBar;
lv_obj_t* swapLabel;
lv_obj_t* infoLabel;

// Network tab
lv_obj_t* netTitle;
lv_obj_t* rxLabel;
lv_obj_t* txLabel;
// The chart is one point wider than its clipping parent and slides left by one
// point spacing per stats interval, so the graph scrolls continuously.
lv_obj_t* netClip;
lv_obj_t* netChart;
lv_obj_t* netPeakLabel;
lv_chart_series_t* rxSeries;
lv_chart_series_t* txSeries;
lv_coord_t netStep;  // px between points
float rxHist[kNetHistory + 1];
float txHist[kNetHistory + 1];
lv_coord_t rxPoints[kNetHistory + 1];
lv_coord_t txPoints[kNetHistory + 1];
float netScaleFrom = 1.0f, netScaleTo = 1.0f, netScale = 1.0f;  // bytes/s at top of chart
uint32_t lastStatsMs = 0;
// The scroll runs on its own clock rather than stepping per sample: stats
// arrive with ~±75 ms jitter, which made a per-sample scroll stall and jump.
float netPeriodMs = 1000.0f;  // smoothed stats interval = time per scroll step
bool netScrolling = false;
lv_obj_t* diskBars[kMaxDisks];
lv_obj_t* diskLabels[kMaxDisks];

// Controls tab
lv_obj_t* buttonGrid;
lv_obj_t* noButtonsLabel;
struct ButtonSlot {
    ButtonDef def;
    lv_obj_t* btn;
    lv_obj_t* img;      // created once all pixels are in
    lv_img_dsc_t dsc;   // points at `pixels`
    uint8_t* pixels;
    size_t received;
};
ButtonSlot slots[kMaxButtons];
size_t slotCount = 0;

// Waiting screen: covers everything while the host isn't connected.
constexpr uint32_t kWaitBg = 0x0B0E13;
constexpr uint32_t kWaitAccent = 0x26C6DA;
constexpr uint32_t kWaitMarkColors[] = {0x26C6DA, 0x66BB6A, 0xFFA726};
constexpr lv_coord_t kWaitBarHeights[] = {30, 56, 40};
lv_obj_t* waitScreen;
lv_obj_t* waitBars[3];
lv_obj_t* waitWord;
lv_obj_t* waitTag;
lv_obj_t* waitPill;
lv_obj_t* waitRing;
bool waitShown = false;

// Overlays
lv_obj_t* toast;
lv_timer_t* toastTimer;

void fmtBytes(uint64_t v, char* out, size_t n) {
    static const char* units[] = {"B", "K", "M", "G", "T"};
    double d = static_cast<double>(v);
    size_t u = 0;
    while (d >= 1024.0 && u < 4) {
        d /= 1024.0;
        u++;
    }
    snprintf(out, n, u == 0 ? "%.0f%s" : "%.1f%s", d, units[u]);
}

void fmtUptime(uint64_t s, char* out, size_t n) {
    const unsigned days = s / 86400, hours = (s % 86400) / 3600, mins = (s % 3600) / 60;
    if (days) snprintf(out, n, "%ud %uh %um", days, hours, mins);
    else snprintf(out, n, "%uh %um", hours, mins);
}

uint8_t pct(uint64_t used, uint64_t total) {
    return total ? static_cast<uint8_t>(used * 100 / total) : 0;
}

lv_obj_t* makeBar(lv_obj_t* parent, lv_coord_t w) {
    lv_obj_t* bar = lv_bar_create(parent);
    lv_obj_set_size(bar, w, 12);
    lv_bar_set_range(bar, 0, 100);
    lv_obj_set_style_anim_time(bar, kAnimMs, 0);
    return bar;
}

// CPU gauge: sweep the arc and count the percentage along with it.
void cpuAnimExec(void* arc, int32_t v) {
    lv_arc_set_value(static_cast<lv_obj_t*>(arc), static_cast<int16_t>(v));
    lv_label_set_text_fmt(cpuLabel, "%d%%", static_cast<int>(v));
}

void animateCpu(int32_t target) {
    lv_anim_t a;
    lv_anim_init(&a);
    lv_anim_set_var(&a, cpuArc);
    lv_anim_set_exec_cb(&a, cpuAnimExec);
    lv_anim_set_values(&a, lv_arc_get_value(cpuArc), target);
    lv_anim_set_time(&a, kAnimMs);
    lv_anim_set_path_cb(&a, lv_anim_path_ease_out);
    lv_anim_start(&a);  // replaces any running animation on the arc
}

void noScroll(lv_obj_t* obj) { lv_obj_clear_flag(obj, LV_OBJ_FLAG_SCROLLABLE); }

void buildSystemTab(lv_obj_t* tab) {
    noScroll(tab);

    cpuArc = lv_arc_create(tab);
    lv_obj_set_size(cpuArc, 130, 130);
    lv_obj_align(cpuArc, LV_ALIGN_TOP_LEFT, 0, 0);
    lv_arc_set_rotation(cpuArc, 135);
    lv_arc_set_bg_angles(cpuArc, 0, 270);
    lv_arc_set_range(cpuArc, 0, 100);
    lv_arc_set_value(cpuArc, 0);
    lv_obj_remove_style(cpuArc, nullptr, LV_PART_KNOB);
    lv_obj_clear_flag(cpuArc, LV_OBJ_FLAG_CLICKABLE);

    cpuLabel = lv_label_create(cpuArc);
    lv_obj_set_style_text_font(cpuLabel, &lv_font_montserrat_28, 0);
    lv_label_set_text(cpuLabel, "--%");
    lv_obj_center(cpuLabel);

    cpuInfoLabel = lv_label_create(tab);
    lv_obj_set_style_text_font(cpuInfoLabel, &lv_font_montserrat_12, 0);
    lv_obj_set_width(cpuInfoLabel, 130);
    lv_obj_set_style_text_align(cpuInfoLabel, LV_TEXT_ALIGN_CENTER, 0);
    lv_label_set_text(cpuInfoLabel, "CPU");
    lv_obj_align_to(cpuInfoLabel, cpuArc, LV_ALIGN_OUT_BOTTOM_MID, 0, -14);

    // Right column: memory, swap, misc info.
    const lv_coord_t colX = 150, colW = 290;

    memLabel = lv_label_create(tab);
    lv_label_set_text(memLabel, "RAM");
    lv_obj_set_pos(memLabel, colX, 0);
    memBar = makeBar(tab, colW);
    lv_obj_set_pos(memBar, colX, 22);

    swapLabel = lv_label_create(tab);
    lv_label_set_text(swapLabel, "Swap");
    lv_obj_set_pos(swapLabel, colX, 44);
    swapBar = makeBar(tab, colW);
    lv_obj_set_pos(swapBar, colX, 66);

    infoLabel = lv_label_create(tab);
    lv_obj_set_style_text_font(infoLabel, &lv_font_montserrat_12, 0);
    lv_label_set_text(infoLabel, "");
    lv_obj_set_pos(infoLabel, colX, 90);

    // Per-core usage across the bottom: one vertical bar per core, shown as
    // the host reports them. Separate bars (not lv_chart) so each animates.
    lv_obj_t* coreRow = lv_obj_create(tab);
    lv_obj_set_size(coreRow, LV_PCT(100), 80);
    lv_obj_align(coreRow, LV_ALIGN_BOTTOM_MID, 0, 0);
    lv_obj_set_flex_flow(coreRow, LV_FLEX_FLOW_ROW);
    lv_obj_set_style_pad_all(coreRow, 6, 0);
    lv_obj_set_style_pad_column(coreRow, 3, 0);
    noScroll(coreRow);
    for (size_t i = 0; i < kMaxCores; i++) {
        lv_obj_t* bar = lv_bar_create(coreRow);
        lv_bar_set_range(bar, 0, 100);
        lv_obj_set_height(bar, LV_PCT(100));
        lv_obj_set_flex_grow(bar, 1);
        lv_obj_set_style_radius(bar, 2, LV_PART_MAIN);
        lv_obj_set_style_radius(bar, 2, LV_PART_INDICATOR);
        lv_obj_set_style_bg_color(bar, lv_palette_main(LV_PALETTE_GREEN), LV_PART_INDICATOR);
        lv_obj_set_style_anim_time(bar, kAnimMs, 0);
        lv_obj_add_flag(bar, LV_OBJ_FLAG_HIDDEN);
        coreBars[i] = bar;
    }
}

void buildNetworkTab(lv_obj_t* tab) {
    noScroll(tab);

    netTitle = lv_label_create(tab);
    lv_label_set_text(netTitle, "Network");
    lv_obj_set_pos(netTitle, 0, 0);

    rxLabel = lv_label_create(tab);
    lv_obj_set_style_text_color(rxLabel, kRxColor, 0);
    lv_label_set_text(rxLabel, LV_SYMBOL_DOWNLOAD " --");
    lv_obj_set_pos(rxLabel, 170, 0);

    txLabel = lv_label_create(tab);
    lv_obj_set_style_text_color(txLabel, kTxColor, 0);
    lv_label_set_text(txLabel, LV_SYMBOL_UPLOAD " --");
    lv_obj_set_pos(txLabel, 310, 0);

    // The clip box carries the chart's look; the chart itself is transparent.
    netClip = lv_obj_create(tab);
    lv_obj_set_size(netClip, LV_PCT(100), 120);
    lv_obj_set_pos(netClip, 0, 24);
    lv_obj_set_style_pad_all(netClip, 0, 0);
    noScroll(netClip);
    lv_obj_update_layout(netClip);
    const lv_coord_t visibleW = lv_obj_get_content_width(netClip);
    netStep = visibleW / (kNetHistory - 1);

    netChart = lv_chart_create(netClip);
    // One extra point, one step wider: chart spacing = width / points = netStep.
    lv_obj_set_size(netChart, netStep * kNetHistory, LV_PCT(100));
    lv_obj_set_style_bg_opa(netChart, LV_OPA_TRANSP, 0);
    lv_obj_set_style_border_width(netChart, 0, 0);
    lv_obj_set_style_radius(netChart, 0, 0);
    lv_obj_set_style_pad_all(netChart, 0, 0);
    lv_obj_set_style_pad_ver(netChart, 4, 0);
    lv_chart_set_type(netChart, LV_CHART_TYPE_LINE);
    lv_chart_set_point_count(netChart, kNetHistory + 1);
    lv_chart_set_range(netChart, LV_CHART_AXIS_PRIMARY_Y, 0, kNetChartMax);
    lv_chart_set_div_line_count(netChart, 3, 0);
    lv_obj_set_style_size(netChart, 0, LV_PART_INDICATOR);  // hide point markers
    rxSeries = lv_chart_add_series(netChart, kRxColor, LV_CHART_AXIS_PRIMARY_Y);
    txSeries = lv_chart_add_series(netChart, kTxColor, LV_CHART_AXIS_PRIMARY_Y);
    lv_chart_set_ext_y_array(netChart, rxSeries, rxPoints);
    lv_chart_set_ext_y_array(netChart, txSeries, txPoints);

    // Top-of-chart scale; the graph is normalised to it.
    netPeakLabel = lv_label_create(netClip);
    lv_obj_set_style_text_font(netPeakLabel, &lv_font_montserrat_12, 0);
    lv_obj_set_style_text_opa(netPeakLabel, LV_OPA_70, 0);
    lv_label_set_text(netPeakLabel, "");
    lv_obj_align(netPeakLabel, LV_ALIGN_TOP_RIGHT, -4, 2);

    // Disks: two per row under the chart.
    for (size_t i = 0; i < kMaxDisks; i++) {
        const lv_coord_t x = (i % 2) * 225, y = 154 + (i / 2) * 40;
        diskLabels[i] = lv_label_create(tab);
        lv_obj_set_style_text_font(diskLabels[i], &lv_font_montserrat_12, 0);
        lv_obj_set_pos(diskLabels[i], x, y);
        diskBars[i] = makeBar(tab, 210);
        lv_obj_set_pos(diskBars[i], x, y + 18);
        lv_obj_add_flag(diskLabels[i], LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(diskBars[i], LV_OBJ_FLAG_HIDDEN);
    }
}

void freeImage(ButtonSlot& slot) {
    if (slot.img) {
        lv_obj_del(slot.img);
        slot.img = nullptr;
    }
    if (slot.pixels) {
        lv_img_cache_invalidate_src(&slot.dsc);
        heap_caps_free(slot.pixels);
        slot.pixels = nullptr;
    }
    slot.received = 0;
}

void onButtonClicked(lv_event_t* e) {
    const auto idx = reinterpret_cast<uintptr_t>(lv_event_get_user_data(e));
    if (idx < slotCount) protocol::sendPress(slots[idx].def.id);
}

void buildControlsTab(lv_obj_t* tab) {
    noScroll(tab);

    buttonGrid = lv_obj_create(tab);
    lv_obj_remove_style_all(buttonGrid);
    lv_obj_set_size(buttonGrid, LV_PCT(100), LV_PCT(100));
    // 4x3 grid; LVGL sizes the cells, so this works before the tab is first laid out.
    static const lv_coord_t cols[] = {LV_GRID_FR(1), LV_GRID_FR(1), LV_GRID_FR(1), LV_GRID_FR(1),
                                      LV_GRID_TEMPLATE_LAST};
    static const lv_coord_t rows[] = {LV_GRID_FR(1), LV_GRID_FR(1), LV_GRID_FR(1),
                                      LV_GRID_TEMPLATE_LAST};
    lv_obj_set_grid_dsc_array(buttonGrid, cols, rows);
    lv_obj_set_style_pad_gap(buttonGrid, 8, 0);

    noButtonsLabel = lv_label_create(tab);
    lv_label_set_text(noButtonsLabel, "No buttons yet.\nAdd [[button]] entries to the host config.");
    lv_obj_set_style_text_align(noButtonsLabel, LV_TEXT_ALIGN_CENTER, 0);
    lv_obj_center(noButtonsLabel);
}

// A plain filled rectangle, without the theme's styles.
lv_obj_t* makeRect(lv_obj_t* parent, lv_coord_t w, lv_coord_t h, uint32_t color, lv_coord_t radius) {
    lv_obj_t* r = lv_obj_create(parent);
    lv_obj_remove_style_all(r);
    lv_obj_set_size(r, w, h);
    lv_obj_set_style_bg_color(r, lv_color_hex(color), 0);
    lv_obj_set_style_bg_opa(r, LV_OPA_COVER, 0);
    lv_obj_set_style_radius(r, radius, 0);
    lv_obj_clear_flag(r, LV_OBJ_FLAG_CLICKABLE);
    return r;
}

lv_obj_t* makeWaitLabel(lv_obj_t* parent, const lv_font_t* font, uint32_t color, const char* text) {
    lv_obj_t* l = lv_label_create(parent);
    lv_obj_set_style_text_font(l, font, 0);
    lv_obj_set_style_text_color(l, lv_color_hex(color), 0);
    lv_label_set_text(l, text);
    return l;
}

void buildWaitScreen() {
    waitScreen = lv_obj_create(lv_layer_top());
    lv_obj_remove_style_all(waitScreen);
    lv_obj_set_size(waitScreen, LV_PCT(100), LV_PCT(100));
    lv_obj_set_style_bg_color(waitScreen, lv_color_hex(kWaitBg), 0);
    lv_obj_set_style_bg_opa(waitScreen, LV_OPA_COVER, 0);
    noScroll(waitScreen);  // stays clickable, so it swallows touches meant for the tabs below

    // Three-colour stripe along the top edge.
    for (size_t i = 0; i < 3; i++)
        lv_obj_set_pos(makeRect(waitScreen, kScreenWidth / 3, 3, kWaitMarkColors[i], 0),
                       i * (kScreenWidth / 3), 0);

    lv_obj_t* col = lv_obj_create(waitScreen);
    lv_obj_remove_style_all(col);
    lv_obj_set_size(col, LV_SIZE_CONTENT, LV_SIZE_CONTENT);
    lv_obj_set_flex_flow(col, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_flex_align(col, LV_FLEX_ALIGN_START, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
    lv_obj_set_style_pad_row(col, 6, 0);
    lv_obj_align(col, LV_ALIGN_CENTER, 0, -14);

    // Mark: three bars, bottom-aligned, like a tiny stats graph.
    lv_obj_t* mark = lv_obj_create(col);
    lv_obj_remove_style_all(mark);
    // 12 px taller than the bars: extra space above the wordmark (LVGL 8 has no margins).
    lv_obj_set_size(mark, 3 * 14 + 2 * 6, kWaitBarHeights[1] + 12);
    for (size_t i = 0; i < 3; i++) {
        waitBars[i] = makeRect(mark, 14, kWaitBarHeights[i], kWaitMarkColors[i], 3);
        lv_obj_align(waitBars[i], LV_ALIGN_BOTTOM_LEFT, i * 20, -12);  // stays bottom-aligned as it resizes
    }

    char word[48];
    snprintf(word, sizeof word, "nerd#%06X -#deck", static_cast<unsigned>(kWaitAccent));
    waitWord = makeWaitLabel(col, &lv_font_montserrat_40, 0xECEFF4, word);
    lv_label_set_recolor(waitWord, true);

    // The built-in fonts are ASCII plus LVGL's symbols, so the separator is LV_SYMBOL_BULLET.
    waitTag = makeWaitLabel(col, &lv_font_montserrat_12, 0x9AA4B2,
                            "SYSTEM STATS  " LV_SYMBOL_BULLET "  TOUCH CONTROLS");
    lv_obj_set_style_text_letter_space(waitTag, 3, 0);

    // Status pill with a pulsing dot.
    waitPill = lv_obj_create(waitScreen);
    lv_obj_remove_style_all(waitPill);
    lv_obj_set_size(waitPill, LV_SIZE_CONTENT, LV_SIZE_CONTENT);
    lv_obj_set_style_bg_color(waitPill, lv_color_hex(0x151B24), 0);
    lv_obj_set_style_bg_opa(waitPill, LV_OPA_COVER, 0);
    lv_obj_set_style_border_color(waitPill, lv_color_hex(0x232C38), 0);
    lv_obj_set_style_border_width(waitPill, 1, 0);
    lv_obj_set_style_radius(waitPill, LV_RADIUS_CIRCLE, 0);
    lv_obj_set_style_pad_hor(waitPill, 16, 0);
    lv_obj_set_style_pad_ver(waitPill, 8, 0);
    lv_obj_set_style_pad_column(waitPill, 10, 0);
    lv_obj_set_flex_flow(waitPill, LV_FLEX_FLOW_ROW);
    lv_obj_set_flex_align(waitPill, LV_FLEX_ALIGN_START, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
    lv_obj_align(waitPill, LV_ALIGN_BOTTOM_MID, 0, -24);

    lv_obj_t* dot = lv_obj_create(waitPill);
    lv_obj_remove_style_all(dot);
    lv_obj_set_size(dot, 8, 8);
    lv_obj_add_flag(dot, LV_OBJ_FLAG_OVERFLOW_VISIBLE);  // the ring grows past it
    waitRing = makeRect(dot, 8, 8, kWaitAccent, LV_RADIUS_CIRCLE);
    lv_obj_center(waitRing);
    lv_obj_center(makeRect(dot, 8, 8, kWaitAccent, LV_RADIUS_CIRCLE));

    makeWaitLabel(waitPill, &lv_font_montserrat_12, 0xC3CAD5, "Waiting for host");

    lv_obj_t* ver = makeWaitLabel(waitScreen, &lv_font_montserrat_12, 0x8792A2, "fw " FW_VERSION);
    lv_obj_align(ver, LV_ALIGN_BOTTOM_RIGHT, -12, -8);
}

void waitBarExec(void* bar, int32_t h) { lv_obj_set_height(static_cast<lv_obj_t*>(bar), h); }

// After the bars grow in they keep rising and falling, out of step, so the
// screen never looks frozen.
void waitBarIdle(lv_anim_t* grown) {
    const auto i = reinterpret_cast<uintptr_t>(grown->user_data);
    lv_anim_t a;
    lv_anim_init(&a);
    lv_anim_set_var(&a, waitBars[i]);
    lv_anim_set_exec_cb(&a, waitBarExec);
    lv_anim_set_values(&a, kWaitBarHeights[i], kWaitBarHeights[i] * 55 / 100);
    lv_anim_set_time(&a, 1200);
    lv_anim_set_playback_time(&a, 1200);
    lv_anim_set_delay(&a, 300 + i * 280);
    lv_anim_set_repeat_count(&a, LV_ANIM_REPEAT_INFINITE);
    lv_anim_set_path_cb(&a, lv_anim_path_ease_in_out);
    lv_anim_start(&a);
}

void waitOpaExec(void* obj, int32_t v) {
    lv_obj_set_style_opa(static_cast<lv_obj_t*>(obj), static_cast<lv_opa_t>(v), 0);
}

void waitRiseExec(void* obj, int32_t v) {
    lv_obj_set_style_translate_y(static_cast<lv_obj_t*>(obj), static_cast<lv_coord_t>(v), 0);
}

void fadeUp(lv_obj_t* obj, uint32_t delay) {
    lv_obj_set_style_opa(obj, LV_OPA_TRANSP, 0);
    lv_anim_t a;
    lv_anim_init(&a);
    lv_anim_set_var(&a, obj);
    lv_anim_set_time(&a, 600);
    lv_anim_set_delay(&a, delay);
    lv_anim_set_path_cb(&a, lv_anim_path_ease_out);
    lv_anim_set_exec_cb(&a, waitOpaExec);
    lv_anim_set_values(&a, LV_OPA_TRANSP, LV_OPA_COVER);
    lv_anim_start(&a);
    lv_anim_set_exec_cb(&a, waitRiseExec);
    lv_anim_set_values(&a, 6, 0);
    lv_anim_start(&a);
}

// v: 0..1000 through one pulse; the ring grows to 2.6x and fades out.
void waitRingExec(void* ring, int32_t v) {
    auto* obj = static_cast<lv_obj_t*>(ring);
    const lv_coord_t d = 8 + 13 * v / 1000;
    lv_obj_set_size(obj, d, d);
    lv_obj_set_style_opa(obj, static_cast<lv_opa_t>(153 * (1000 - v) / 1000), 0);
}

void startWaitAnims() {
    for (size_t i = 0; i < 3; i++) {
        lv_anim_t a;
        lv_anim_init(&a);
        lv_anim_set_var(&a, waitBars[i]);
        lv_anim_set_user_data(&a, reinterpret_cast<void*>(static_cast<uintptr_t>(i)));
        lv_anim_set_exec_cb(&a, waitBarExec);
        lv_anim_set_values(&a, kWaitBarHeights[i] * 15 / 100, kWaitBarHeights[i]);
        lv_anim_set_time(&a, 900);
        lv_anim_set_delay(&a, i * 120);
        lv_anim_set_early_apply(&a, true);  // start short, not full height
        lv_anim_set_path_cb(&a, lv_anim_path_ease_out);
        lv_anim_set_ready_cb(&a, waitBarIdle);
        lv_anim_start(&a);
    }
    fadeUp(waitWord, 350);
    fadeUp(waitTag, 550);
    fadeUp(waitPill, 800);

    lv_anim_t ring;
    lv_anim_init(&ring);
    lv_anim_set_var(&ring, waitRing);
    lv_anim_set_exec_cb(&ring, waitRingExec);
    lv_anim_set_values(&ring, 0, 1000);
    lv_anim_set_time(&ring, 1600);
    lv_anim_set_repeat_count(&ring, LV_ANIM_REPEAT_INFINITE);
    lv_anim_set_path_cb(&ring, lv_anim_path_ease_out);
    lv_anim_start(&ring);
}

void stopWaitAnims() {
    for (lv_obj_t* bar : waitBars) lv_anim_del(bar, nullptr);
    lv_anim_del(waitWord, nullptr);
    lv_anim_del(waitTag, nullptr);
    lv_anim_del(waitPill, nullptr);
    lv_anim_del(waitRing, nullptr);
}

void showWaitScreen() {
    lv_anim_del(waitScreen, nullptr);  // a fade-out still running
    lv_obj_set_style_opa(waitScreen, LV_OPA_COVER, 0);
    lv_obj_clear_flag(waitScreen, LV_OBJ_FLAG_HIDDEN);
    startWaitAnims();
}

void hideWaitScreen() {
    lv_anim_t a;
    lv_anim_init(&a);
    lv_anim_set_var(&a, waitScreen);
    lv_anim_set_exec_cb(&a, waitOpaExec);
    lv_anim_set_values(&a, LV_OPA_COVER, LV_OPA_TRANSP);
    lv_anim_set_time(&a, 250);
    lv_anim_set_ready_cb(&a, [](lv_anim_t*) {
        lv_obj_add_flag(waitScreen, LV_OBJ_FLAG_HIDDEN);
        stopWaitAnims();
    });
    lv_anim_start(&a);
}

void buildOverlays() {
    buildWaitScreen();
    waitShown = true;  // until the host's first message
    startWaitAnims();

    toast = lv_label_create(lv_layer_top());
    lv_obj_set_style_bg_opa(toast, LV_OPA_90, 0);
    lv_obj_set_style_radius(toast, 6, 0);
    lv_obj_set_style_pad_all(toast, 8, 0);
    lv_obj_align(toast, LV_ALIGN_BOTTOM_MID, 0, -30);
    lv_obj_add_flag(toast, LV_OBJ_FLAG_HIDDEN);

    toastTimer = lv_timer_create([](lv_timer_t*) { lv_obj_add_flag(toast, LV_OBJ_FLAG_HIDDEN); },
                                 1500, nullptr);
    lv_timer_pause(toastTimer);
}

void renormaliseNet() {
    const float scale = netScale > 1.0f ? netScale : 1.0f;
    for (size_t i = 0; i <= kNetHistory; i++) {
        const float rx = rxHist[i] / scale, tx = txHist[i] / scale;
        rxPoints[i] = static_cast<lv_coord_t>((rx > 1.0f ? 1.0f : rx) * kNetChartMax);
        txPoints[i] = static_cast<lv_coord_t>((tx > 1.0f ? 1.0f : tx) * kNetChartMax);
    }
    lv_chart_refresh(netChart);
}

// v: 0..1024 progress from netScaleFrom to netScaleTo.
void netScaleExec(void*, int32_t v) {
    netScale = netScaleFrom + (netScaleTo - netScaleFrom) * (v / 1024.0f);
    renormaliseNet();
}

void netScrollExec(void* chart, int32_t x) { lv_obj_set_x(static_cast<lv_obj_t*>(chart), x); }

// Plot relative to the peak in view (lv_coord_t is 16-bit), easing toward a
// new peak instead of snapping.
void updateNetScale() {
    float peak = 1.0f;
    for (size_t i = 0; i <= kNetHistory; i++) {
        if (rxHist[i] > peak) peak = rxHist[i];
        if (txHist[i] > peak) peak = txHist[i];
    }
    char buf[16];
    fmtBytes(static_cast<uint64_t>(peak), buf, sizeof buf);
    lv_label_set_text_fmt(netPeakLabel, "%s/s", buf);

    renormaliseNet();
    if (peak != netScaleTo) {
        netScaleFrom = netScale;
        netScaleTo = peak;
        lv_anim_t scale;
        lv_anim_init(&scale);
        lv_anim_set_var(&scale, &netScale);
        lv_anim_set_exec_cb(&scale, netScaleExec);
        lv_anim_set_values(&scale, 0, 1024);
        lv_anim_set_time(&scale, kAnimMs);
        lv_anim_set_path_cb(&scale, lv_anim_path_ease_out);
        lv_anim_start(&scale);
    }
}

void netStepDone(lv_anim_t*);

// Slide the chart one point spacing left over one period.
void startNetStep() {
    lv_obj_set_x(netChart, 0);
    lv_anim_t a;
    lv_anim_init(&a);
    lv_anim_set_var(&a, netChart);
    lv_anim_set_exec_cb(&a, netScrollExec);
    lv_anim_set_values(&a, 0, -netStep);
    lv_anim_set_time(&a, static_cast<uint32_t>(netPeriodMs));
    lv_anim_set_path_cb(&a, lv_anim_path_linear);
    lv_anim_set_ready_cb(&a, netStepDone);
    lv_anim_start(&a);
    netScrolling = true;
}

// A step finished: shift the data one point left and jump the chart back to
// x=0, which looks identical. The new right-edge point repeats the last value
// until a sample arrives to replace it.
void netStepDone(lv_anim_t*) {
    memmove(rxHist, rxHist + 1, kNetHistory * sizeof(float));
    memmove(txHist, txHist + 1, kNetHistory * sizeof(float));
    rxHist[kNetHistory] = rxHist[kNetHistory - 1];
    txHist[kNetHistory] = txHist[kNetHistory - 1];
    updateNetScale();

    if (lv_tick_elaps(lastStatsMs) < kHostTimeoutMs) {
        startNetStep();
    } else {
        // Host gone: stop rather than scroll flat lines forever.
        lv_obj_set_x(netChart, 0);
        netScrolling = false;
    }
}

void onNetSample(float rx, float tx) {
    const uint32_t now = lv_tick_get();
    if (lastStatsMs) {
        float gap = static_cast<float>(now - lastStatsMs);
        if (gap < 200) gap = 200;
        if (gap > 3000) gap = 3000;
        netPeriodMs += (gap - netPeriodMs) * 0.2f;
    }
    lastStatsMs = now;

    // The right-edge point is the one scrolling into view; show the sample there now.
    rxHist[kNetHistory] = rx;
    txHist[kNetHistory] = tx;
    updateNetScale();

    if (!netScrolling) startNetStep();
}

}  // namespace

void init() {
    lv_obj_t* tabview = lv_tabview_create(lv_scr_act(), LV_DIR_TOP, 40);
    buildSystemTab(lv_tabview_add_tab(tabview, LV_SYMBOL_HOME " System"));
    buildNetworkTab(lv_tabview_add_tab(tabview, LV_SYMBOL_WIFI " Network"));
    buildControlsTab(lv_tabview_add_tab(tabview, LV_SYMBOL_KEYBOARD " Controls"));
    buildOverlays();
#ifdef NERD_START_TAB
    // Debug: open on a given tab (0 System, 1 Network, 2 Controls).
    lv_tabview_set_act(tabview, NERD_START_TAB, LV_ANIM_OFF);
#endif
}

void updateStats(const HostStats& s) {
    char a[16], b[16], buf[160];

    // CPU
    animateCpu(static_cast<int32_t>(s.cpu.usage + 0.5f));
    // stdlib snprintf: LVGL's built-in printf has no float support by default.
    // The built-in fonts are ASCII-only, so no degree sign.
    if (s.cpu.has_temp)
        snprintf(buf, sizeof buf, "CPU  %u MHz  %.0f C", static_cast<unsigned>(s.cpu.freq_mhz), s.cpu.temp);
    else
        snprintf(buf, sizeof buf, "CPU  %u MHz", static_cast<unsigned>(s.cpu.freq_mhz));
    lv_label_set_text(cpuInfoLabel, buf);

    for (size_t i = 0; i < kMaxCores; i++) {
        if (i < s.cpu.core_count) {
            lv_obj_clear_flag(coreBars[i], LV_OBJ_FLAG_HIDDEN);
            lv_bar_set_value(coreBars[i], static_cast<int32_t>(s.cpu.cores[i] + 0.5f), LV_ANIM_ON);
        } else {
            lv_obj_add_flag(coreBars[i], LV_OBJ_FLAG_HIDDEN);
        }
    }

    // Memory
    fmtBytes(s.mem.used, a, sizeof a);
    fmtBytes(s.mem.total, b, sizeof b);
    lv_label_set_text_fmt(memLabel, "RAM  %s / %s", a, b);
    lv_bar_set_value(memBar, pct(s.mem.used, s.mem.total), LV_ANIM_ON);

    fmtBytes(s.mem.swap_used, a, sizeof a);
    fmtBytes(s.mem.swap_total, b, sizeof b);
    lv_label_set_text_fmt(swapLabel, "Swap  %s / %s", a, b);
    lv_bar_set_value(swapBar, pct(s.mem.swap_used, s.mem.swap_total), LV_ANIM_ON);

    fmtUptime(s.uptime, a, sizeof a);
    snprintf(buf, sizeof buf, "%s\nUp %s\nLoad %.2f  %.2f  %.2f", s.host, a, s.load[0], s.load[1],
             s.load[2]);
    lv_label_set_text(infoLabel, buf);

    // Network
    lv_label_set_text(netTitle, s.net.iface[0] ? s.net.iface : "Network");
    fmtBytes(s.net.rx_bps, a, sizeof a);
    lv_label_set_text_fmt(rxLabel, LV_SYMBOL_DOWNLOAD " %s/s", a);
    fmtBytes(s.net.tx_bps, a, sizeof a);
    lv_label_set_text_fmt(txLabel, LV_SYMBOL_UPLOAD " %s/s", a);
    onNetSample(static_cast<float>(s.net.rx_bps), static_cast<float>(s.net.tx_bps));

    // Disks
    for (size_t i = 0; i < kMaxDisks; i++) {
        if (i < s.disk_count) {
            const auto& d = s.disks[i];
            fmtBytes(d.used, a, sizeof a);
            fmtBytes(d.total, b, sizeof b);
            lv_label_set_text_fmt(diskLabels[i], LV_SYMBOL_DRIVE " %s  %s / %s", d.mount, a, b);
            lv_bar_set_value(diskBars[i], pct(d.used, d.total), LV_ANIM_ON);
            lv_obj_clear_flag(diskLabels[i], LV_OBJ_FLAG_HIDDEN);
            lv_obj_clear_flag(diskBars[i], LV_OBJ_FLAG_HIDDEN);
        } else {
            lv_obj_add_flag(diskLabels[i], LV_OBJ_FLAG_HIDDEN);
            lv_obj_add_flag(diskBars[i], LV_OBJ_FLAG_HIDDEN);
        }
    }
}

void setButtons(const ButtonDef* defs, size_t count) {
    // Deletes the buttons and their lv_img children, after which the pixels
    // they pointed at can be freed.
    lv_obj_clean(buttonGrid);
    for (auto& slot : slots) {
        slot.img = nullptr;  // already deleted with its button
        freeImage(slot);
    }

    slotCount = count > kMaxButtons ? kMaxButtons : count;
    for (size_t i = 0; i < slotCount; i++) {
        ButtonSlot& slot = slots[i];
        slot.def = defs[i];

        lv_obj_t* btn = lv_btn_create(buttonGrid);
        slot.btn = btn;
        lv_obj_set_grid_cell(btn, LV_GRID_ALIGN_STRETCH, i % 4, 1, LV_GRID_ALIGN_STRETCH, i / 4, 1);
        lv_obj_add_event_cb(btn, onButtonClicked, LV_EVENT_CLICKED,
                            reinterpret_cast<void*>(static_cast<uintptr_t>(i)));
        // Image or icon stacked above the label.
        lv_obj_set_flex_flow(btn, LV_FLEX_FLOW_COLUMN);
        lv_obj_set_flex_align(btn, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
        lv_obj_set_style_pad_ver(btn, 4, 0);
        lv_obj_set_style_pad_row(btn, 4, 0);

        if (slot.def.icon[0] && !slot.def.has_image) {
            lv_obj_t* icon = lv_label_create(btn);
            lv_obj_set_style_text_font(icon, &lv_font_montserrat_28, 0);
            lv_label_set_text(icon, slot.def.icon);
        }

        lv_obj_t* label = lv_label_create(btn);
        lv_label_set_text(label, slot.def.label);
        lv_label_set_long_mode(label, LV_LABEL_LONG_WRAP);
        lv_obj_set_width(label, LV_PCT(100));
        lv_obj_set_style_text_align(label, LV_TEXT_ALIGN_CENTER, 0);
    }

    if (slotCount) lv_obj_add_flag(noButtonsLabel, LV_OBJ_FLAG_HIDDEN);
    else lv_obj_clear_flag(noButtonsLabel, LV_OBJ_FLAG_HIDDEN);
}

void setButtonImage(const char* id, uint16_t w, uint16_t h, size_t offset, size_t total,
                    const uint8_t* data, size_t len) {
    ButtonSlot* slot = nullptr;
    for (size_t i = 0; i < slotCount; i++)
        if (strcmp(slots[i].def.id, id) == 0) slot = &slots[i];
    if (!slot) return;

    if (w == 0 || h == 0 || w > kMaxImageSide || h > kMaxImageSide ||
        total != size_t(w) * h * LV_IMG_PX_SIZE_ALPHA_BYTE)
        return;

    if (offset == 0) {
        freeImage(*slot);
        slot->pixels = static_cast<uint8_t*>(heap_caps_malloc(total, MALLOC_CAP_SPIRAM));
        if (!slot->pixels) slot->pixels = static_cast<uint8_t*>(malloc(total));
        if (!slot->pixels) return;
        slot->dsc = {};
        slot->dsc.header.cf = LV_IMG_CF_TRUE_COLOR_ALPHA;
        slot->dsc.header.w = w;
        slot->dsc.header.h = h;
        slot->dsc.data_size = total;
        slot->dsc.data = slot->pixels;
    }

    // Out-of-order or oversized chunk: drop the partial image.
    if (!slot->pixels || offset != slot->received || offset + len > total) {
        freeImage(*slot);
        return;
    }

    memcpy(slot->pixels + offset, data, len);
    slot->received += len;

    if (slot->received == total) {
        slot->img = lv_img_create(slot->btn);
        lv_img_set_src(slot->img, &slot->dsc);
        lv_obj_move_to_index(slot->img, 0);
    }
}

void setConnected(bool connected) {
    if (connected != waitShown) return;
    waitShown = !connected;
    if (connected) hideWaitScreen();
    else showWaitScreen();
}

void showToast(const char* text, bool ok) {
    lv_label_set_text(toast, text);
    lv_obj_set_style_bg_color(
        toast, ok ? lv_palette_darken(LV_PALETTE_GREEN, 2) : lv_palette_darken(LV_PALETTE_RED, 2), 0);
    lv_obj_clear_flag(toast, LV_OBJ_FLAG_HIDDEN);
    lv_obj_align(toast, LV_ALIGN_BOTTOM_MID, 0, -30);
    lv_timer_reset(toastTimer);
    lv_timer_resume(toastTimer);
}

}  // namespace ui
