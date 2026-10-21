#include "ui.hpp"

#include <esp_heap_caps.h>
#include <lvgl.h>
#include <stdio.h>
#include <string.h>

#include "protocol.hpp"

namespace ui {
namespace {

constexpr size_t kNetHistory = 60;
constexpr lv_coord_t kNetChartMax = 1000;

const lv_color_t kRxColor = lv_palette_main(LV_PALETTE_CYAN);
const lv_color_t kTxColor = lv_palette_main(LV_PALETTE_ORANGE);

// System tab
lv_obj_t* cpuArc;
lv_obj_t* cpuLabel;
lv_obj_t* cpuInfoLabel;
lv_obj_t* coreChart;
lv_chart_series_t* coreSeries;
lv_obj_t* memBar;
lv_obj_t* memLabel;
lv_obj_t* swapBar;
lv_obj_t* swapLabel;
lv_obj_t* infoLabel;

// Network tab
lv_obj_t* netTitle;
lv_obj_t* rxLabel;
lv_obj_t* txLabel;
lv_obj_t* netChart;
lv_chart_series_t* rxSeries;
lv_chart_series_t* txSeries;
float rxHist[kNetHistory];
float txHist[kNetHistory];
lv_coord_t rxPoints[kNetHistory];
lv_coord_t txPoints[kNetHistory];
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

// Overlays
lv_obj_t* statusBanner;
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
    return bar;
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

    // Per-core usage across the bottom.
    coreChart = lv_chart_create(tab);
    lv_obj_set_size(coreChart, LV_PCT(100), 80);
    lv_obj_align(coreChart, LV_ALIGN_BOTTOM_MID, 0, 0);
    lv_chart_set_type(coreChart, LV_CHART_TYPE_BAR);
    lv_chart_set_range(coreChart, LV_CHART_AXIS_PRIMARY_Y, 0, 100);
    lv_chart_set_div_line_count(coreChart, 0, 0);
    // LVGL 8.3 bar charts divide by (point_cnt - 1), so never go below 2 points.
    lv_chart_set_point_count(coreChart, 2);
    coreSeries = lv_chart_add_series(coreChart, lv_palette_main(LV_PALETTE_GREEN),
                                     LV_CHART_AXIS_PRIMARY_Y);
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

    netChart = lv_chart_create(tab);
    lv_obj_set_size(netChart, LV_PCT(100), 120);
    lv_obj_set_pos(netChart, 0, 24);
    lv_chart_set_type(netChart, LV_CHART_TYPE_LINE);
    lv_chart_set_point_count(netChart, kNetHistory);
    lv_chart_set_range(netChart, LV_CHART_AXIS_PRIMARY_Y, 0, kNetChartMax);
    lv_chart_set_div_line_count(netChart, 3, 0);
    lv_obj_set_style_size(netChart, 0, LV_PART_INDICATOR);  // hide point markers
    rxSeries = lv_chart_add_series(netChart, kRxColor, LV_CHART_AXIS_PRIMARY_Y);
    txSeries = lv_chart_add_series(netChart, kTxColor, LV_CHART_AXIS_PRIMARY_Y);
    lv_chart_set_ext_y_array(netChart, rxSeries, rxPoints);
    lv_chart_set_ext_y_array(netChart, txSeries, txPoints);

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

void buildOverlays() {
    statusBanner = lv_label_create(lv_layer_top());
    lv_label_set_text(statusBanner, LV_SYMBOL_USB "  Waiting for host...");
    lv_obj_set_width(statusBanner, LV_PCT(100));
    lv_obj_set_style_text_align(statusBanner, LV_TEXT_ALIGN_CENTER, 0);
    lv_obj_set_style_bg_color(statusBanner, lv_palette_darken(LV_PALETTE_RED, 2), 0);
    lv_obj_set_style_bg_opa(statusBanner, LV_OPA_COVER, 0);
    lv_obj_set_style_pad_ver(statusBanner, 4, 0);
    lv_obj_align(statusBanner, LV_ALIGN_BOTTOM_MID, 0, 0);

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

void pushNetHistory(float rx, float tx) {
    memmove(rxHist, rxHist + 1, (kNetHistory - 1) * sizeof(float));
    memmove(txHist, txHist + 1, (kNetHistory - 1) * sizeof(float));
    rxHist[kNetHistory - 1] = rx;
    txHist[kNetHistory - 1] = tx;

    // lv_coord_t is 16-bit, so normalise to the current peak instead of plotting raw rates.
    float peak = 1.0f;
    for (size_t i = 0; i < kNetHistory; i++) {
        if (rxHist[i] > peak) peak = rxHist[i];
        if (txHist[i] > peak) peak = txHist[i];
    }
    for (size_t i = 0; i < kNetHistory; i++) {
        rxPoints[i] = static_cast<lv_coord_t>(rxHist[i] / peak * kNetChartMax);
        txPoints[i] = static_cast<lv_coord_t>(txHist[i] / peak * kNetChartMax);
    }
    lv_chart_refresh(netChart);
}

}  // namespace

void init() {
    lv_obj_t* tabview = lv_tabview_create(lv_scr_act(), LV_DIR_TOP, 40);
    buildSystemTab(lv_tabview_add_tab(tabview, LV_SYMBOL_HOME " System"));
    buildNetworkTab(lv_tabview_add_tab(tabview, LV_SYMBOL_WIFI " Network"));
    buildControlsTab(lv_tabview_add_tab(tabview, LV_SYMBOL_KEYBOARD " Controls"));
    buildOverlays();
}

void updateStats(const HostStats& s) {
    char a[16], b[16], buf[160];

    // CPU
    lv_arc_set_value(cpuArc, static_cast<int16_t>(s.cpu.usage));
    lv_label_set_text_fmt(cpuLabel, "%d%%", static_cast<int>(s.cpu.usage));
    // stdlib snprintf: LVGL's built-in printf has no float support by default.
    // The built-in fonts are ASCII-only, so no degree sign.
    if (s.cpu.has_temp)
        snprintf(buf, sizeof buf, "CPU  %u MHz  %.0f C", static_cast<unsigned>(s.cpu.freq_mhz), s.cpu.temp);
    else
        snprintf(buf, sizeof buf, "CPU  %u MHz", static_cast<unsigned>(s.cpu.freq_mhz));
    lv_label_set_text(cpuInfoLabel, buf);

    if (s.cpu.core_count > 0) {
        const uint16_t points = s.cpu.core_count < 2 ? 2 : s.cpu.core_count;
        lv_chart_set_point_count(coreChart, points);
        for (uint16_t i = 0; i < points; i++) {
            const float v = i < s.cpu.core_count ? s.cpu.cores[i] : 0.0f;
            lv_chart_set_value_by_id(coreChart, coreSeries, i, static_cast<lv_coord_t>(v));
        }
        lv_chart_refresh(coreChart);
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
    pushNetHistory(static_cast<float>(s.net.rx_bps), static_cast<float>(s.net.tx_bps));

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
    const bool hidden = lv_obj_has_flag(statusBanner, LV_OBJ_FLAG_HIDDEN);
    if (connected && !hidden) lv_obj_add_flag(statusBanner, LV_OBJ_FLAG_HIDDEN);
    else if (!connected && hidden) lv_obj_clear_flag(statusBanner, LV_OBJ_FLAG_HIDDEN);
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
