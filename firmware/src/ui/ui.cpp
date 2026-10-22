#include "ui.hpp"

#include <ctype.h>
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

// Palette, shared with the waiting screen.
constexpr uint32_t kBg = 0x0B0E13;
constexpr uint32_t kCard = 0x151B24;
constexpr uint32_t kCardBorder = 0x232C38;
constexpr uint32_t kTrack = 0x1E2530;
constexpr uint32_t kText = 0xECEFF4;
constexpr uint32_t kTextDim = 0x9AA4B2;
constexpr uint32_t kTextFaint = 0x8792A2;
constexpr uint32_t kCyan = 0x26C6DA;
constexpr uint32_t kGreen = 0x66BB6A;
constexpr uint32_t kOrange = 0xFFA726;
constexpr uint32_t kRed = 0xEF5350;
constexpr uint32_t kStripe[] = {kCyan, kGreen, kOrange};

constexpr lv_coord_t kHeaderH = 44;  // 3 px stripe + tab pills
constexpr lv_coord_t kGap = 8;

const lv_color_t kRxColor = lv_color_hex(kCyan);
const lv_color_t kTxColor = lv_color_hex(kOrange);

// Header
lv_obj_t* batteryBox;
lv_obj_t* batteryBolt;
lv_obj_t* batteryFill;
lv_obj_t* batteryLabel;

// System tab
lv_obj_t* cpuArc;
lv_obj_t* cpuLabel;
lv_obj_t* cpuInfoLabel;
lv_obj_t* coreBars[kMaxCores];
lv_obj_t* loadLabel;
lv_obj_t* memBar;
lv_obj_t* memLabel;
lv_obj_t* swapBar;
lv_obj_t* swapLabel;
lv_obj_t* hostLabel;
lv_obj_t* uptimeLabel;

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
lv_obj_t* diskUseLabels[kMaxDisks];

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
constexpr uint32_t kWaitMarkColors[] = {kCyan, kGreen, kOrange};
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
lv_obj_t* toastDot;
lv_obj_t* toastLabel;
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

// Green, then orange from 75%, red from 90%.
lv_color_t fillColor(uint8_t p) { return lv_color_hex(p >= 90 ? kRed : p >= 75 ? kOrange : kGreen); }

void noScroll(lv_obj_t* obj) { lv_obj_clear_flag(obj, LV_OBJ_FLAG_SCROLLABLE); }

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

lv_obj_t* makeLabel(lv_obj_t* parent, const lv_font_t* font, uint32_t color, const char* text) {
    lv_obj_t* l = lv_label_create(parent);
    lv_obj_set_style_text_font(l, font, 0);
    lv_obj_set_style_text_color(l, lv_color_hex(color), 0);
    lv_label_set_text(l, text);
    return l;
}

// Small spaced-out heading at the top-left of a card.
lv_obj_t* makeCaption(lv_obj_t* parent, const char* text) {
    lv_obj_t* l = makeLabel(parent, &lv_font_montserrat_12, kTextDim, text);
    lv_obj_set_style_text_letter_space(l, 2, 0);
    return l;
}

lv_obj_t* makeCard(lv_obj_t* parent, lv_coord_t x, lv_coord_t y, lv_coord_t w, lv_coord_t h) {
    lv_obj_t* c = makeRect(parent, w, h, kCard, 10);
    lv_obj_set_style_border_color(c, lv_color_hex(kCardBorder), 0);
    lv_obj_set_style_border_width(c, 1, 0);
    lv_obj_set_style_pad_hor(c, 12, 0);
    lv_obj_set_style_pad_ver(c, 8, 0);
    lv_obj_set_pos(c, x, y);
    noScroll(c);
    return c;
}

// Thin rounded bar: track plus coloured fill.
lv_obj_t* makeBar(lv_obj_t* parent, lv_coord_t w, lv_coord_t h, uint32_t color) {
    lv_obj_t* bar = lv_bar_create(parent);
    lv_obj_remove_style_all(bar);
    lv_obj_set_size(bar, w, h);
    lv_bar_set_range(bar, 0, 100);
    lv_obj_set_style_bg_color(bar, lv_color_hex(kTrack), LV_PART_MAIN);
    lv_obj_set_style_bg_opa(bar, LV_OPA_COVER, LV_PART_MAIN);
    lv_obj_set_style_radius(bar, 3, LV_PART_MAIN);
    lv_obj_set_style_bg_color(bar, lv_color_hex(color), LV_PART_INDICATOR);
    lv_obj_set_style_bg_opa(bar, LV_OPA_COVER, LV_PART_INDICATOR);
    lv_obj_set_style_radius(bar, 3, LV_PART_INDICATOR);
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

// Tab pills on the left of the header, the stripe above them, and the battery
// on the right.
void styleHeader(lv_obj_t* tabview) {
    lv_obj_t* btns = lv_tabview_get_tab_btns(tabview);
    lv_obj_set_width(btns, 340);
    lv_obj_set_style_bg_opa(btns, LV_OPA_TRANSP, 0);
    lv_obj_set_style_border_width(btns, 0, 0);
    lv_obj_set_style_pad_top(btns, 7, 0);
    lv_obj_set_style_pad_bottom(btns, 3, 0);
    lv_obj_set_style_pad_left(btns, 8, 0);
    lv_obj_set_style_pad_right(btns, 0, 0);
    lv_obj_set_style_pad_column(btns, 4, 0);

    lv_obj_set_style_radius(btns, LV_RADIUS_CIRCLE, LV_PART_ITEMS);
    lv_obj_set_style_bg_opa(btns, LV_OPA_TRANSP, LV_PART_ITEMS);
    lv_obj_set_style_border_width(btns, 0, LV_PART_ITEMS);
    lv_obj_set_style_text_color(btns, lv_color_hex(kTextFaint), LV_PART_ITEMS);
    lv_obj_set_style_text_font(btns, &lv_font_montserrat_14, LV_PART_ITEMS);
    lv_obj_set_style_outline_width(btns, 0, LV_PART_ITEMS | LV_STATE_FOCUS_KEY);
    const lv_style_selector_t checked = LV_PART_ITEMS | LV_STATE_CHECKED;
    lv_obj_set_style_bg_color(btns, lv_color_hex(kCard), checked);
    lv_obj_set_style_bg_opa(btns, LV_OPA_COVER, checked);
    lv_obj_set_style_border_color(btns, lv_color_hex(kCardBorder), checked);
    lv_obj_set_style_border_width(btns, 1, checked);
    lv_obj_set_style_border_side(btns, LV_BORDER_SIDE_FULL, checked);
    lv_obj_set_style_text_color(btns, lv_color_hex(kText), checked);

    for (size_t i = 0; i < 3; i++) {
        lv_obj_t* r = makeRect(tabview, kScreenWidth / 3, 3, kStripe[i], 0);
        lv_obj_add_flag(r, LV_OBJ_FLAG_FLOATING);
        lv_obj_set_pos(r, i * (kScreenWidth / 3), 0);
    }

    // Battery: bolt (while charging), outline with fill, percentage. Hidden
    // unless the host reports a battery.
    batteryBox = lv_obj_create(tabview);
    lv_obj_remove_style_all(batteryBox);
    lv_obj_add_flag(batteryBox, LV_OBJ_FLAG_FLOATING);
    lv_obj_set_size(batteryBox, LV_SIZE_CONTENT, 34);
    lv_obj_set_flex_flow(batteryBox, LV_FLEX_FLOW_ROW);
    lv_obj_set_flex_align(batteryBox, LV_FLEX_ALIGN_START, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
    lv_obj_set_style_pad_column(batteryBox, 5, 0);
    lv_obj_align(batteryBox, LV_ALIGN_TOP_RIGHT, -10, 7);
    lv_obj_clear_flag(batteryBox, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_add_flag(batteryBox, LV_OBJ_FLAG_HIDDEN);

    batteryBolt = makeLabel(batteryBox, &lv_font_montserrat_12, kGreen, LV_SYMBOL_CHARGE);

    lv_obj_t* body = lv_obj_create(batteryBox);
    lv_obj_remove_style_all(body);
    lv_obj_set_size(body, LV_SIZE_CONTENT, LV_SIZE_CONTENT);
    lv_obj_set_flex_flow(body, LV_FLEX_FLOW_ROW);
    lv_obj_set_flex_align(body, LV_FLEX_ALIGN_START, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
    lv_obj_t* shell = makeRect(body, 22, 12, kBg, 3);
    lv_obj_set_style_border_color(shell, lv_color_hex(kTextFaint), 0);
    lv_obj_set_style_border_width(shell, 2, 0);
    lv_obj_set_style_pad_all(shell, 1, 0);
    batteryFill = makeRect(shell, LV_PCT(100), LV_PCT(100), kGreen, 1);
    makeRect(body, 2, 5, kTextFaint, 1);  // terminal nub

    batteryLabel = makeLabel(batteryBox, &lv_font_montserrat_12, kText, "");
}

void buildSystemTab(lv_obj_t* tab) {
    noScroll(tab);

    // CPU gauge card.
    lv_obj_t* cpuCard = makeCard(tab, 0, 0, 150, 160);
    cpuArc = lv_arc_create(cpuCard);
    lv_obj_set_size(cpuArc, 116, 116);
    lv_obj_align(cpuArc, LV_ALIGN_TOP_MID, 0, -2);
    lv_arc_set_rotation(cpuArc, 135);
    lv_arc_set_bg_angles(cpuArc, 0, 270);
    lv_arc_set_range(cpuArc, 0, 100);
    lv_arc_set_value(cpuArc, 0);
    lv_obj_remove_style(cpuArc, nullptr, LV_PART_KNOB);
    lv_obj_clear_flag(cpuArc, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_set_style_pad_all(cpuArc, 0, 0);
    lv_obj_set_style_bg_opa(cpuArc, LV_OPA_TRANSP, 0);
    lv_obj_set_style_arc_width(cpuArc, 10, LV_PART_MAIN);
    lv_obj_set_style_arc_width(cpuArc, 10, LV_PART_INDICATOR);
    lv_obj_set_style_arc_color(cpuArc, lv_color_hex(kTrack), LV_PART_MAIN);
    lv_obj_set_style_arc_color(cpuArc, lv_color_hex(kCyan), LV_PART_INDICATOR);
    lv_obj_set_style_arc_rounded(cpuArc, true, LV_PART_MAIN);
    lv_obj_set_style_arc_rounded(cpuArc, true, LV_PART_INDICATOR);

    cpuLabel = makeLabel(cpuArc, &lv_font_montserrat_28, kText, "--%");
    lv_obj_align(cpuLabel, LV_ALIGN_CENTER, 0, -6);
    lv_obj_align(makeCaption(cpuArc, "CPU"), LV_ALIGN_CENTER, 0, 18);

    cpuInfoLabel = makeLabel(cpuCard, &lv_font_montserrat_12, kTextDim, "");
    lv_obj_align(cpuInfoLabel, LV_ALIGN_BOTTOM_MID, 0, 0);

    // Right column: memory, swap, host and uptime.
    const lv_coord_t colX = 150 + kGap, colW = 464 - colX;
    const lv_coord_t halfW = (colW - kGap) / 2;

    auto memCard = [&](lv_coord_t y, const char* caption, lv_obj_t** label, lv_obj_t** bar) {
        lv_obj_t* card = makeCard(tab, colX, y, colW, 48);
        makeCaption(card, caption);
        *label = makeLabel(card, &lv_font_montserrat_14, kText, "");
        lv_label_set_recolor(*label, true);
        lv_obj_align(*label, LV_ALIGN_TOP_RIGHT, 0, -2);
        *bar = makeBar(card, LV_PCT(100), 6, kGreen);
        lv_obj_align(*bar, LV_ALIGN_BOTTOM_LEFT, 0, 0);
    };
    memCard(0, "RAM", &memLabel, &memBar);
    memCard(48 + kGap, "SWAP", &swapLabel, &swapBar);

    auto infoCard = [&](lv_coord_t x, const char* caption) {
        lv_obj_t* card = makeCard(tab, x, 2 * (48 + kGap), halfW, 48);
        makeCaption(card, caption);
        lv_obj_t* value = makeLabel(card, &lv_font_montserrat_14, kText, "");
        lv_obj_set_width(value, LV_PCT(100));
        lv_label_set_long_mode(value, LV_LABEL_LONG_DOT);
        lv_obj_align(value, LV_ALIGN_BOTTOM_LEFT, 0, 0);
        return value;
    };
    hostLabel = infoCard(colX, "HOST");
    uptimeLabel = infoCard(colX + halfW + kGap, "UPTIME");

    // Per-core usage: one vertical bar per core, shown as the host reports
    // them. Separate bars (not lv_chart) so each animates. Load averages sit
    // in the header, next to the bars they relate to.
    const lv_coord_t coresY = 160 + kGap;
    lv_obj_t* coresCard = makeCard(tab, 0, coresY, 464, 265 - coresY);
    makeCaption(coresCard, "CORES");
    loadLabel = makeLabel(coresCard, &lv_font_montserrat_12, kTextFaint, "");
    lv_label_set_recolor(loadLabel, true);
    lv_obj_align(loadLabel, LV_ALIGN_TOP_RIGHT, 0, 0);

    lv_obj_t* coreRow = lv_obj_create(coresCard);
    lv_obj_remove_style_all(coreRow);
    lv_obj_set_size(coreRow, LV_PCT(100), 265 - coresY - 2 - 16 - 21);
    lv_obj_align(coreRow, LV_ALIGN_BOTTOM_LEFT, 0, 2);
    lv_obj_set_flex_flow(coreRow, LV_FLEX_FLOW_ROW);
    lv_obj_set_style_pad_column(coreRow, 4, 0);
    noScroll(coreRow);
    for (size_t i = 0; i < kMaxCores; i++) {
        lv_obj_t* bar = makeBar(coreRow, 4, LV_PCT(100), kCyan);
        lv_obj_set_flex_grow(bar, 1);
        lv_obj_add_flag(bar, LV_OBJ_FLAG_HIDDEN);
        coreBars[i] = bar;
    }
}

void buildNetworkTab(lv_obj_t* tab) {
    noScroll(tab);

    // Download / upload rate cards.
    const lv_coord_t statW = (464 - kGap) / 2;
    auto rateCard = [&](lv_coord_t x, const char* caption, uint32_t color) {
        lv_obj_t* card = makeCard(tab, x, 0, statW, 60);
        lv_label_set_recolor(makeCaption(card, caption), true);
        lv_obj_t* value = makeLabel(card, &lv_font_montserrat_20, color, "--");
        lv_obj_align(value, LV_ALIGN_BOTTOM_LEFT, 0, 2);
        return value;
    };
    rxLabel = rateCard(0, "#26C6DA " LV_SYMBOL_DOWNLOAD "#  DOWN", kCyan);
    txLabel = rateCard(statW + kGap, "#FFA726 " LV_SYMBOL_UPLOAD "#  UP", kOrange);

    // Graph card: interface name and peak scale above the scrolling chart.
    const lv_coord_t chartY = 60 + kGap, chartH = 112;
    lv_obj_t* chartCard = makeCard(tab, 0, chartY, 464, chartH);
    netTitle = makeCaption(chartCard, "NETWORK");
    netPeakLabel = makeLabel(chartCard, &lv_font_montserrat_12, kTextFaint, "");
    lv_obj_align(netPeakLabel, LV_ALIGN_TOP_RIGHT, 0, 0);

    netClip = lv_obj_create(chartCard);
    lv_obj_remove_style_all(netClip);
    lv_obj_set_size(netClip, LV_PCT(100), chartH - 2 - 16 - 19);
    lv_obj_align(netClip, LV_ALIGN_BOTTOM_LEFT, 0, 0);
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
    lv_obj_set_style_line_color(netChart, lv_color_hex(kTrack), LV_PART_MAIN);
    lv_obj_set_style_line_width(netChart, 2, LV_PART_ITEMS);
    lv_chart_set_type(netChart, LV_CHART_TYPE_LINE);
    lv_chart_set_point_count(netChart, kNetHistory + 1);
    lv_chart_set_range(netChart, LV_CHART_AXIS_PRIMARY_Y, 0, kNetChartMax);
    lv_chart_set_div_line_count(netChart, 3, 0);
    lv_obj_set_style_size(netChart, 0, LV_PART_INDICATOR);  // hide point markers
    rxSeries = lv_chart_add_series(netChart, kRxColor, LV_CHART_AXIS_PRIMARY_Y);
    txSeries = lv_chart_add_series(netChart, kTxColor, LV_CHART_AXIS_PRIMARY_Y);
    lv_chart_set_ext_y_array(netChart, rxSeries, rxPoints);
    lv_chart_set_ext_y_array(netChart, txSeries, txPoints);

    // Disks: two per row; each bar is coloured by how full it is.
    const lv_coord_t disksY = chartY + chartH + kGap;
    lv_obj_t* disksCard = makeCard(tab, 0, disksY, 464, 265 - disksY);
    const lv_coord_t diskW = (464 - 2 - 24 - 20) / 2;
    for (size_t i = 0; i < kMaxDisks; i++) {
        const lv_coord_t x = (i % 2) * (diskW + 20), y = (i / 2) * 32;
        diskLabels[i] = makeLabel(disksCard, &lv_font_montserrat_12, kText, "");
        lv_obj_set_width(diskLabels[i], diskW / 2);
        lv_label_set_long_mode(diskLabels[i], LV_LABEL_LONG_DOT);
        lv_obj_set_pos(diskLabels[i], x, y);
        diskUseLabels[i] = makeLabel(disksCard, &lv_font_montserrat_12, kTextDim, "");
        lv_obj_set_width(diskUseLabels[i], diskW);
        lv_obj_set_style_text_align(diskUseLabels[i], LV_TEXT_ALIGN_RIGHT, 0);
        lv_obj_set_pos(diskUseLabels[i], x, y);
        diskBars[i] = makeBar(disksCard, diskW, 6, kGreen);
        lv_obj_set_pos(diskBars[i], x, y + 19);
        lv_obj_add_flag(diskLabels[i], LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(diskUseLabels[i], LV_OBJ_FLAG_HIDDEN);
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
    lv_obj_set_style_pad_gap(buttonGrid, kGap, 0);

    noButtonsLabel = makeLabel(tab, &lv_font_montserrat_14, kTextDim,
                               "No buttons yet.\nAdd [[button]] entries to the host config.");
    lv_obj_set_style_text_align(noButtonsLabel, LV_TEXT_ALIGN_CENTER, 0);
    lv_obj_center(noButtonsLabel);
}

void buildWaitScreen() {
    waitScreen = lv_obj_create(lv_layer_top());
    lv_obj_remove_style_all(waitScreen);
    lv_obj_set_size(waitScreen, LV_PCT(100), LV_PCT(100));
    lv_obj_set_style_bg_color(waitScreen, lv_color_hex(kBg), 0);
    lv_obj_set_style_bg_opa(waitScreen, LV_OPA_COVER, 0);
    noScroll(waitScreen);  // stays clickable, so it swallows touches meant for the tabs below

    // Three-colour stripe along the top edge.
    for (size_t i = 0; i < 3; i++)
        lv_obj_set_pos(makeRect(waitScreen, kScreenWidth / 3, 3, kStripe[i], 0),
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
    snprintf(word, sizeof word, "nerd#%06X -#deck", static_cast<unsigned>(kCyan));
    waitWord = makeLabel(col, &lv_font_montserrat_40, kText, word);
    lv_label_set_recolor(waitWord, true);

    // The built-in fonts are ASCII plus LVGL's symbols, so the separator is LV_SYMBOL_BULLET.
    waitTag = makeLabel(col, &lv_font_montserrat_12, kTextDim,
                            "SYSTEM STATS  " LV_SYMBOL_BULLET "  TOUCH CONTROLS");
    lv_obj_set_style_text_letter_space(waitTag, 3, 0);

    // Status pill with a pulsing dot.
    waitPill = lv_obj_create(waitScreen);
    lv_obj_remove_style_all(waitPill);
    lv_obj_set_size(waitPill, LV_SIZE_CONTENT, LV_SIZE_CONTENT);
    lv_obj_set_style_bg_color(waitPill, lv_color_hex(kCard), 0);
    lv_obj_set_style_bg_opa(waitPill, LV_OPA_COVER, 0);
    lv_obj_set_style_border_color(waitPill, lv_color_hex(kCardBorder), 0);
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
    waitRing = makeRect(dot, 8, 8, kCyan, LV_RADIUS_CIRCLE);
    lv_obj_center(waitRing);
    lv_obj_center(makeRect(dot, 8, 8, kCyan, LV_RADIUS_CIRCLE));

    makeLabel(waitPill, &lv_font_montserrat_12, 0xC3CAD5, "Waiting for host");

    lv_obj_t* ver = makeLabel(waitScreen, &lv_font_montserrat_12, kTextFaint, "fw " FW_VERSION);
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

    // Toast: a pill like the waiting screen's, with a green or red dot.
    toast = lv_obj_create(lv_layer_top());
    lv_obj_remove_style_all(toast);
    lv_obj_set_size(toast, LV_SIZE_CONTENT, LV_SIZE_CONTENT);
    lv_obj_set_style_bg_color(toast, lv_color_hex(kCard), 0);
    lv_obj_set_style_bg_opa(toast, LV_OPA_COVER, 0);
    lv_obj_set_style_border_width(toast, 1, 0);
    lv_obj_set_style_radius(toast, LV_RADIUS_CIRCLE, 0);
    lv_obj_set_style_pad_hor(toast, 16, 0);
    lv_obj_set_style_pad_ver(toast, 8, 0);
    lv_obj_set_style_pad_column(toast, 10, 0);
    lv_obj_set_flex_flow(toast, LV_FLEX_FLOW_ROW);
    lv_obj_set_flex_align(toast, LV_FLEX_ALIGN_START, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
    lv_obj_align(toast, LV_ALIGN_BOTTOM_MID, 0, -16);
    lv_obj_clear_flag(toast, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_add_flag(toast, LV_OBJ_FLAG_HIDDEN);
    toastDot = makeRect(toast, 8, 8, kGreen, LV_RADIUS_CIRCLE);
    toastLabel = makeLabel(toast, &lv_font_montserrat_12, kText, "");

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
    lv_label_set_text_fmt(netPeakLabel, "peak %s/s", buf);

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

void updateBattery(const HostStats& s) {
    if (!s.battery.present) {
        lv_obj_add_flag(batteryBox, LV_OBJ_FLAG_HIDDEN);
        return;
    }
    const uint8_t p = s.battery.percent > 100 ? 100 : s.battery.percent;
    const lv_color_t color = lv_color_hex(p <= 10 ? kRed : p <= 20 ? kOrange : kGreen);
    lv_obj_set_width(batteryFill, LV_PCT(p));
    lv_obj_set_style_bg_color(batteryFill, color, 0);
    lv_obj_set_style_text_color(batteryBolt, color, 0);
    if (s.battery.charging) lv_obj_clear_flag(batteryBolt, LV_OBJ_FLAG_HIDDEN);
    else lv_obj_add_flag(batteryBolt, LV_OBJ_FLAG_HIDDEN);
    lv_label_set_text_fmt(batteryLabel, "%u%%", static_cast<unsigned>(p));
    lv_obj_clear_flag(batteryBox, LV_OBJ_FLAG_HIDDEN);
}

}  // namespace

void init() {
    lv_obj_set_style_bg_color(lv_scr_act(), lv_color_hex(kBg), 0);

    lv_obj_t* tabview = lv_tabview_create(lv_scr_act(), LV_DIR_TOP, kHeaderH);
    lv_obj_set_style_bg_color(tabview, lv_color_hex(kBg), 0);
    lv_obj_set_style_bg_opa(tabview, LV_OPA_COVER, 0);
    lv_obj_set_style_bg_opa(lv_tabview_get_content(tabview), LV_OPA_TRANSP, 0);

    lv_obj_t* tabs[] = {
        lv_tabview_add_tab(tabview, LV_SYMBOL_HOME " System"),
        lv_tabview_add_tab(tabview, LV_SYMBOL_WIFI " Network"),
        lv_tabview_add_tab(tabview, LV_SYMBOL_KEYBOARD " Controls"),
    };
    for (lv_obj_t* tab : tabs) {
        lv_obj_set_style_pad_all(tab, kGap, 0);
        lv_obj_set_style_pad_top(tab, 3, 0);
    }
    styleHeader(tabview);
    buildSystemTab(tabs[0]);
    buildNetworkTab(tabs[1]);
    buildControlsTab(tabs[2]);
    buildOverlays();
#ifdef NERD_START_TAB
    // Debug: open on a given tab (0 System, 1 Network, 2 Controls).
    lv_tabview_set_act(tabview, NERD_START_TAB, LV_ANIM_OFF);
#endif
}

void updateStats(const HostStats& s) {
    char a[16], b[16], buf[160];

    updateBattery(s);

    // CPU
    animateCpu(static_cast<int32_t>(s.cpu.usage + 0.5f));
    // stdlib snprintf: LVGL's built-in printf has no float support by default.
    // The built-in fonts are ASCII-only, so no degree sign.
    if (s.cpu.has_temp)
        snprintf(buf, sizeof buf, "%.1f GHz  " LV_SYMBOL_BULLET "  %.0f C", s.cpu.freq_mhz / 1000.0, s.cpu.temp);
    else
        snprintf(buf, sizeof buf, "%.1f GHz", s.cpu.freq_mhz / 1000.0);
    lv_label_set_text(cpuInfoLabel, buf);

    for (size_t i = 0; i < kMaxCores; i++) {
        if (i < s.cpu.core_count) {
            const int32_t v = static_cast<int32_t>(s.cpu.cores[i] + 0.5f);
            lv_obj_clear_flag(coreBars[i], LV_OBJ_FLAG_HIDDEN);
            lv_obj_set_style_bg_color(coreBars[i], lv_color_hex(v >= 75 ? kOrange : kCyan), LV_PART_INDICATOR);
            lv_bar_set_value(coreBars[i], v, LV_ANIM_ON);
        } else {
            lv_obj_add_flag(coreBars[i], LV_OBJ_FLAG_HIDDEN);
        }
    }
    snprintf(buf, sizeof buf, "load #%06X %.2f#  %.2f  %.2f   " LV_SYMBOL_BULLET "   %u threads",
             static_cast<unsigned>(kText), s.load[0], s.load[1], s.load[2],
             static_cast<unsigned>(s.cpu.core_count));
    lv_label_set_text(loadLabel, buf);

    // Memory: used in full colour, total dimmed.
    fmtBytes(s.mem.used, a, sizeof a);
    fmtBytes(s.mem.total, b, sizeof b);
    lv_label_set_text_fmt(memLabel, "%s #%06X / %s#", a, static_cast<unsigned>(kTextFaint), b);
    lv_bar_set_value(memBar, pct(s.mem.used, s.mem.total), LV_ANIM_ON);

    fmtBytes(s.mem.swap_used, a, sizeof a);
    fmtBytes(s.mem.swap_total, b, sizeof b);
    lv_label_set_text_fmt(swapLabel, "%s #%06X / %s#", a, static_cast<unsigned>(kTextFaint), b);
    lv_bar_set_value(swapBar, pct(s.mem.swap_used, s.mem.swap_total), LV_ANIM_ON);

    lv_label_set_text(hostLabel, s.host[0] ? s.host : "-");
    fmtUptime(s.uptime, a, sizeof a);
    lv_label_set_text(uptimeLabel, a);

    // Network
    if (s.net.iface[0]) {
        char iface[sizeof s.net.iface];
        for (size_t i = 0; i < sizeof iface; i++) iface[i] = toupper(static_cast<unsigned char>(s.net.iface[i]));
        lv_label_set_text(netTitle, iface);
    } else {
        lv_label_set_text(netTitle, "NETWORK");
    }
    fmtBytes(s.net.rx_bps, a, sizeof a);
    lv_label_set_text_fmt(rxLabel, "%s/s", a);
    fmtBytes(s.net.tx_bps, a, sizeof a);
    lv_label_set_text_fmt(txLabel, "%s/s", a);
    onNetSample(static_cast<float>(s.net.rx_bps), static_cast<float>(s.net.tx_bps));

    // Disks
    for (size_t i = 0; i < kMaxDisks; i++) {
        if (i < s.disk_count) {
            const auto& d = s.disks[i];
            const uint8_t p = pct(d.used, d.total);
            fmtBytes(d.used, a, sizeof a);
            fmtBytes(d.total, b, sizeof b);
            lv_label_set_text(diskLabels[i], d.mount);
            lv_label_set_text_fmt(diskUseLabels[i], "%s / %s", a, b);
            lv_obj_set_style_bg_color(diskBars[i], fillColor(p), LV_PART_INDICATOR);
            lv_bar_set_value(diskBars[i], p, LV_ANIM_ON);
            lv_obj_clear_flag(diskLabels[i], LV_OBJ_FLAG_HIDDEN);
            lv_obj_clear_flag(diskUseLabels[i], LV_OBJ_FLAG_HIDDEN);
            lv_obj_clear_flag(diskBars[i], LV_OBJ_FLAG_HIDDEN);
        } else {
            lv_obj_add_flag(diskLabels[i], LV_OBJ_FLAG_HIDDEN);
            lv_obj_add_flag(diskUseLabels[i], LV_OBJ_FLAG_HIDDEN);
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
        lv_obj_set_style_bg_color(btn, lv_color_hex(kCard), 0);
        lv_obj_set_style_border_color(btn, lv_color_hex(kCardBorder), 0);
        lv_obj_set_style_border_width(btn, 1, 0);
        lv_obj_set_style_radius(btn, 10, 0);
        lv_obj_set_style_shadow_width(btn, 0, 0);
        lv_obj_set_style_bg_color(btn, lv_color_hex(0x12303A), LV_STATE_PRESSED);
        lv_obj_set_style_border_color(btn, lv_color_hex(kCyan), LV_STATE_PRESSED);
        lv_obj_set_style_color_filter_opa(btn, LV_OPA_TRANSP, LV_STATE_PRESSED);
        lv_obj_set_style_outline_width(btn, 0, LV_STATE_FOCUS_KEY);
        // Image or icon stacked above the label.
        lv_obj_set_flex_flow(btn, LV_FLEX_FLOW_COLUMN);
        lv_obj_set_flex_align(btn, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
        lv_obj_set_style_pad_ver(btn, 4, 0);
        lv_obj_set_style_pad_row(btn, 6, 0);

        if (slot.def.icon[0] && !slot.def.has_image)
            makeLabel(btn, &lv_font_montserrat_28, kCyan, slot.def.icon);

        lv_obj_t* label = makeLabel(btn, &lv_font_montserrat_14, kText, slot.def.label);
        lv_label_set_long_mode(label, LV_LABEL_LONG_WRAP);
        lv_obj_set_width(label, LV_PCT(100));
        lv_obj_set_style_text_align(label, LV_TEXT_ALIGN_CENTER, 0);
    }

    // Faint placeholders fill the rest of the grid.
    for (size_t i = slotCount; slotCount && i < kMaxButtons; i++) {
        lv_obj_t* empty = makeRect(buttonGrid, 0, 0, 0x0F141B, 10);
        lv_obj_set_style_border_color(empty, lv_color_hex(0x171D26), 0);
        lv_obj_set_style_border_width(empty, 1, 0);
        lv_obj_set_grid_cell(empty, LV_GRID_ALIGN_STRETCH, i % 4, 1, LV_GRID_ALIGN_STRETCH, i / 4, 1);
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

namespace {

void showPill(const char* text, uint32_t dot, uint32_t border, uint32_t ms) {
    lv_label_set_text(toastLabel, text);
    lv_obj_set_style_bg_color(toastDot, lv_color_hex(dot), 0);
    lv_obj_set_style_border_color(toast, lv_color_hex(border), 0);
    lv_obj_clear_flag(toast, LV_OBJ_FLAG_HIDDEN);
    lv_timer_set_period(toastTimer, ms);
    lv_timer_reset(toastTimer);
    lv_timer_resume(toastTimer);
}

}  // namespace

void showToast(const char* text, bool ok) {
    showPill(text, ok ? kGreen : kRed, ok ? 0x2E6B3A : 0x7A2E2E, 1500);
}

void showNotice(const char* text) {
    // Up longer than a button result: nobody is looking at the screen for it.
    showPill(text, kCyan, 0x1D5A63, 8000);
}

}  // namespace ui
