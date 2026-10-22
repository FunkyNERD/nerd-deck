#include "ui.hpp"

#include <ctype.h>
#include <esp_heap_caps.h>
#include <math.h>
#include <stdarg.h>
#include <lvgl.h>
#include <stdio.h>
#include <string.h>

#include "config.hpp"
#include "display.hpp"
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

constexpr lv_coord_t kGap = 8;
constexpr lv_coord_t kHeaderH = 44;  // 3 px stripe + tab pills
constexpr lv_coord_t kBodyW = kScreenWidth - 2 * kGap;  // a tab's content box
constexpr lv_coord_t kBodyH = 265;

const lv_color_t kRxColor = lv_color_hex(kCyan);
const lv_color_t kTxColor = lv_color_hex(kOrange);

// Header: one pill per tab, icon only but for the open tab's, which also
// shows its name. The Battery tab is left out on hosts without a battery.
enum Tab : uint8_t { kTabSystem, kTabNetwork, kTabStorage, kTabBattery, kTabControls, kTabCount };
constexpr const char* kTabIcons[kTabCount] = {"" /* the logo, drawn */, LV_SYMBOL_WIFI, LV_SYMBOL_DRIVE,
                                              LV_SYMBOL_BATTERY_FULL, LV_SYMBOL_KEYBOARD};
// An open tab's icon colour. System's is the logo's three (kStripe).
constexpr uint32_t kTabColors[kTabCount] = {kCyan, kCyan, kOrange, kGreen, 0x42A5F5};
constexpr const char* kTabNames[kTabCount] = {"System", "Network", "Storage", "Battery", "Controls"};
lv_obj_t* tabview;
static_assert(kTabCount == settings::kTabCount, "Tab follows settings::kTabIds");
lv_obj_t* tabPages[kTabCount];
lv_obj_t* tabPills[kTabCount];
lv_obj_t* tabPillIcons[kTabCount];
lv_obj_t* tabPillNames[kTabCount];
// System's icon is the logo's three bars, drawn: the fonts have no chart.
lv_obj_t* logoBars[3];
uint16_t currentTab = kTabSystem;
bool hasBattery = false;
// The tab at each position, and each tab's position: the tabview's pages and
// the header's pills are both kept in this order, so swipes follow it.
uint8_t tabAt[kTabCount] = {kTabSystem, kTabNetwork, kTabStorage, kTabBattery, kTabControls};
uint8_t tabPos[kTabCount] = {0, 1, 2, 3, 4};
int8_t startTab = settings::kStartFirst;
// Once someone picks a tab, the start tab no longer moves them.
bool userNavigated = false;

// Power saving: the backlight dims when untouched for a while, and goes off
// when the host has been away long enough.
constexpr uint8_t kDimPercent = 20;
uint8_t brightness = 80;  // percent, as set
uint32_t dimAfterMs = 0;  // 0 = never
uint32_t offAfterMs = 0;  // on the waiting screen; 0 = never
uint8_t litPercent = 0xFF;  // what the backlight is at now
lv_obj_t* wakeCatcher;     // takes the tap that wakes a dimmed screen

// °C from which the CPU temperature shows orange, and red.
uint8_t tempWarm = 70, tempHot = 85;
lv_obj_t* batteryBox;
lv_obj_t* batteryBolt;
lv_obj_t* batteryFill;
lv_obj_t* batteryLabel;

// System tab
lv_obj_t* cpuArc;
lv_obj_t* cpuLabel;
// Under the gauge: the clock on the left, the temperature on the right, so
// neither shifts as the other changes width.
lv_obj_t* cpuFreqLabel;
lv_obj_t* cpuTempLabel;
lv_obj_t* coreBars[kMaxCores];
lv_obj_t* loadLabel;
lv_obj_t* memBar;
lv_obj_t* memLabel;
// Swap is a note at the end of the memory bar: on most PCs it stays at 0.
lv_obj_t* swapBar;
lv_obj_t* swapLabel;
// Under memory: the GPU, or the busiest program on PCs without one to read.
lv_obj_t* slotCaption;
lv_obj_t* slotValue;  // top right
lv_obj_t* slotName;   // bottom left: the program
lv_obj_t* slotBar;    // bottom left: GPU usage
lv_obj_t* slotSub;    // bottom right
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

// Storage tab: a card per disk, two by two, or full width for two or fewer.
struct DiskCard {
    lv_obj_t* card;
    lv_obj_t* icon;
    lv_obj_t* mount;
    lv_obj_t* tag;  // "USB", on removable drives
    lv_obj_t* percent;
    lv_obj_t* usage;
    lv_obj_t* bar;
    lv_obj_t* free;
    lv_obj_t* io;  // read and write rates, beside `free`
};
DiskCard diskCards[kMaxDisks];
lv_obj_t* noDisksLabel;
uint8_t diskLayout = 0xFF;  // disk count the cards are laid out for
bool diskGrid = false;      // two by two, so the cards are half width

// Controls tab
lv_obj_t* buttonGrid;
lv_obj_t* noButtonsLabel;
struct ButtonSlot {
    ButtonDef def;
    lv_obj_t* btn;
    lv_obj_t* label;
    lv_obj_t* img;      // created once all pixels are in
    lv_img_dsc_t dsc;   // points at `pixels`
    uint8_t* pixels;
    size_t received;
};
ButtonSlot slots[kMaxButtons];
size_t slotCount = 0;
lv_coord_t tileW = 0, tileH = 0;  // a button's face, once laid out

// Battery tab.
constexpr size_t kPowerHistory = 120;      // points on the power chart
constexpr uint32_t kPowerSampleMs = 5000;  // averaged into one point: 10 minutes shown
constexpr lv_coord_t kPowerChartMax = 1000;
lv_obj_t* batPercent;
lv_obj_t* batStatus;
lv_obj_t* batBar;
lv_obj_t* batLimitMark;
lv_obj_t* batLimitLabel;
lv_obj_t* batSource;
lv_obj_t* batWatts;
lv_obj_t* batWattsSub;
lv_obj_t* batPeak;
lv_obj_t* batChart;
lv_chart_series_t* batSeries;
lv_obj_t* batHealth;
lv_obj_t* batCapacity;
lv_obj_t* batVolts;
lv_obj_t* batTemp;
float powerHist[kPowerHistory];
lv_coord_t powerPoints[kPowerHistory];
size_t powerCount = 0;  // points recorded so far, up to kPowerHistory
float powerSum = 0;
uint32_t powerSamples = 0, powerSampleStart = 0;

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

// Notification card: dims the screen and slides down from the top; tap to
// dismiss. A critical notification takes over the whole screen instead.
constexpr lv_coord_t kNotifW = 456;
constexpr lv_coord_t kNotifPadH = 16;
constexpr lv_coord_t kNotifPadTop = 14;
constexpr lv_coord_t kNotifPadBottom = 16;
constexpr lv_coord_t kNotifTile = 44;
constexpr lv_coord_t kNotifTextW = kNotifW - 2 * kNotifPadH - 2 - kNotifTile - 14;
constexpr lv_coord_t kNotifTimeW = kNotifW - 2 * kNotifPadH - 2;
constexpr lv_coord_t kNotifY = 10;
constexpr lv_coord_t kNotifHiddenY = -170;
constexpr lv_opa_t kNotifScrimOpa = 140;  // 55%
constexpr uint32_t kNotifMs = 8000;
lv_obj_t* notifScrim;
lv_obj_t* notifCard;
lv_obj_t* notifApp;
lv_obj_t* notifSummary;
lv_obj_t* notifBody;
lv_obj_t* notifTimeBar;  // shrinks to nothing as the card's time runs out

// Critical takeover.
constexpr lv_coord_t kAlertDisc = 64;
constexpr lv_coord_t kAlertTextW = 416;
lv_obj_t* alertScreen;
lv_obj_t* alertRings[2];  // pulse out from behind the icon, half a beat apart
lv_obj_t* alertText;
lv_obj_t* alertApp;
lv_obj_t* alertSummary;
lv_obj_t* alertBody;
lv_obj_t* alertButton;
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

// Shows the open tab's pill as selected, with its name.
void styleTabPills() {
    for (uint16_t i = 0; i < kTabCount; i++) {
        const bool open = i == currentTab;
        lv_obj_set_style_bg_opa(tabPills[i], open ? LV_OPA_COVER : LV_OPA_TRANSP, 0);
        lv_obj_set_style_border_opa(tabPills[i], open ? LV_OPA_COVER : LV_OPA_TRANSP, 0);
        if (i == kTabSystem) {
            for (size_t b = 0; b < 3; b++)
                lv_obj_set_style_bg_color(logoBars[b], lv_color_hex(open ? kStripe[b] : kTextFaint), 0);
        } else {
            lv_obj_set_style_text_color(tabPillIcons[i], lv_color_hex(open ? kTabColors[i] : kTextFaint), 0);
        }
        if (open) lv_obj_clear_flag(tabPillNames[i], LV_OBJ_FLAG_HIDDEN);
        else lv_obj_add_flag(tabPillNames[i], LV_OBJ_FLAG_HIDDEN);
    }
}

void openTab(uint16_t tab, lv_anim_enable_t anim) {
    currentTab = tab;
    lv_tabview_set_act(tabview, tabPos[tab], anim);
    styleTabPills();
}

// The tab to open on: the start tab, or the first in order, but never a
// Battery tab that isn't shown.
uint16_t startingTab() {
    const auto shown = [](int t) { return t >= 0 && (t != kTabBattery || hasBattery); };
    if (shown(startTab)) return startTab;
    return shown(tabAt[0]) ? tabAt[0] : tabAt[1];
}

void onTabPill(lv_event_t* e) {
    userNavigated = true;
    openTab(static_cast<uint16_t>(reinterpret_cast<uintptr_t>(lv_event_get_user_data(e))), LV_ANIM_ON);
}

// After a swipe. Without a battery, its tab is skipped in the swipe's direction.
void onTabSwiped(lv_event_t*) {
    const uint16_t pos = lv_tabview_get_tab_act(tabview);
    uint16_t tab = tabAt[pos];
    if (tab == currentTab) return;
    userNavigated = true;
    if (tab == kTabBattery && !hasBattery) {
        const int step = pos > tabPos[currentTab] ? 1 : -1;
        const int next = pos + step;
        tab = tabAt[next >= 0 && next < kTabCount ? next : pos - step];
    }
    if (tabPos[tab] != pos) openTab(tab, LV_ANIM_ON);
    else {
        currentTab = tab;
        styleTabPills();
    }
}

// Puts the pages and pills in `order`, keeping the open tab open.
void setTabOrder(const uint8_t* order) {
    for (uint8_t pos = 0; pos < kTabCount; pos++) {
        tabAt[pos] = order[pos];
        tabPos[order[pos]] = pos;
        lv_obj_move_to_index(tabPages[order[pos]], pos);
        lv_obj_move_to_index(tabPills[order[pos]], pos);
    }
    lv_obj_update_layout(tabview);
    lv_tabview_set_act(tabview, tabPos[currentTab], LV_ANIM_OFF);
}

// Sets the backlight for how long the screen has been untouched.
void powerTick(lv_timer_t*) {
    const uint32_t idle = lv_disp_get_inactive_time(nullptr);
    uint8_t target = brightness;
    if (offAfterMs && waitShown && idle >= offAfterMs) target = 0;
    else if (dimAfterMs && idle >= dimAfterMs) target = brightness < kDimPercent ? brightness : kDimPercent;
    if (target != litPercent) {
        litPercent = target;
        display::setBrightness(static_cast<uint8_t>(target * 255 / 100));
    }
    if (target < brightness) lv_obj_clear_flag(wakeCatcher, LV_OBJ_FLAG_HIDDEN);
    else lv_obj_add_flag(wakeCatcher, LV_OBJ_FLAG_HIDDEN);
}

// Wakes a dimmed or dark screen, as a touch would.
void wake() {
    lv_disp_trig_activity(nullptr);
    powerTick(nullptr);
}

void buildPower() {
    // Over everything while dimmed, so the tap that wakes the screen doesn't
    // also press whatever is under it.
    wakeCatcher = lv_obj_create(lv_layer_sys());
    lv_obj_remove_style_all(wakeCatcher);
    lv_obj_set_size(wakeCatcher, kScreenWidth, kScreenHeight);
    lv_obj_add_flag(wakeCatcher, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_add_flag(wakeCatcher, LV_OBJ_FLAG_HIDDEN);
    lv_timer_create(powerTick, 200, nullptr);
}

// Tab pills on the left of the header, the stripe above them, and the battery
// reading on the right. The tabview's own buttons are unused (zero height):
// its content fills the screen, and each tab's top padding clears the header.
void buildHeader() {
    lv_obj_add_event_cb(tabview, onTabSwiped, LV_EVENT_VALUE_CHANGED, nullptr);

    for (size_t i = 0; i < 3; i++) {
        lv_obj_t* r = makeRect(tabview, kScreenWidth / 3, 3, kStripe[i], 0);
        lv_obj_add_flag(r, LV_OBJ_FLAG_FLOATING);
        lv_obj_set_pos(r, i * (kScreenWidth / 3), 0);
    }

    lv_obj_t* row = lv_obj_create(tabview);
    lv_obj_remove_style_all(row);
    lv_obj_add_flag(row, LV_OBJ_FLAG_FLOATING);
    lv_obj_set_size(row, LV_SIZE_CONTENT, 34);
    lv_obj_set_pos(row, kGap, 7);
    lv_obj_set_flex_flow(row, LV_FLEX_FLOW_ROW);
    lv_obj_set_style_pad_column(row, 4, 0);
    noScroll(row);
    for (uint16_t i = 0; i < kTabCount; i++) {
        lv_obj_t* pill = lv_obj_create(row);
        lv_obj_remove_style_all(pill);
        lv_obj_set_size(pill, LV_SIZE_CONTENT, LV_PCT(100));
        lv_obj_set_style_min_width(pill, 52, 0);
        lv_obj_set_style_pad_hor(pill, 14, 0);
        lv_obj_set_style_radius(pill, LV_RADIUS_CIRCLE, 0);
        lv_obj_set_style_bg_color(pill, lv_color_hex(kCard), 0);
        lv_obj_set_style_border_color(pill, lv_color_hex(kCardBorder), 0);
        lv_obj_set_style_border_width(pill, 1, 0);
        lv_obj_set_flex_flow(pill, LV_FLEX_FLOW_ROW);
        lv_obj_set_flex_align(pill, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
        lv_obj_set_style_pad_column(pill, 8, 0);
        noScroll(pill);
        lv_obj_add_flag(pill, LV_OBJ_FLAG_CLICKABLE);
        lv_obj_set_ext_click_area(pill, 5);  // the header's full height
        lv_obj_add_event_cb(pill, onTabPill, LV_EVENT_CLICKED, reinterpret_cast<void*>(static_cast<uintptr_t>(i)));
        tabPills[i] = pill;
        if (i == kTabSystem) {
            // The logo's proportions: bars 30, 56 and 40 tall, 14 wide, 6 apart.
            lv_obj_t* logo = lv_obj_create(pill);
            lv_obj_remove_style_all(logo);
            lv_obj_set_size(logo, 16, 16);
            lv_obj_clear_flag(logo, LV_OBJ_FLAG_CLICKABLE);
            constexpr lv_coord_t kBarH[] = {9, 16, 11};
            for (size_t b = 0; b < 3; b++) {
                logoBars[b] = makeRect(logo, 4, kBarH[b], kStripe[b], 1);
                lv_obj_set_pos(logoBars[b], b * 6, 16 - kBarH[b]);
            }
            tabPillIcons[i] = logo;
        } else {
            tabPillIcons[i] = makeLabel(pill, &lv_font_montserrat_16, kTextFaint, kTabIcons[i]);
        }
        tabPillNames[i] = makeLabel(pill, &lv_font_montserrat_14, kText, kTabNames[i]);
    }
    lv_obj_add_flag(tabPills[kTabBattery], LV_OBJ_FLAG_HIDDEN);  // until the host reports a battery
    styleTabPills();

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

    cpuFreqLabel = makeLabel(cpuCard, &lv_font_montserrat_12, kTextDim, "");
    lv_obj_align(cpuFreqLabel, LV_ALIGN_BOTTOM_LEFT, 0, 0);
    cpuTempLabel = makeLabel(cpuCard, &lv_font_montserrat_12, kTextDim, "");
    lv_obj_align(cpuTempLabel, LV_ALIGN_BOTTOM_RIGHT, 0, 0);

    // Right column: memory, the GPU (or busiest program), host and uptime.
    const lv_coord_t colX = 150 + kGap, colW = 464 - colX;
    const lv_coord_t halfW = (colW - kGap) / 2;

    // A card with a caption, a value top right, and a row along the bottom.
    auto rowCard = [&](lv_coord_t y, const char* caption, lv_obj_t** captionLabel, lv_obj_t** value) {
        lv_obj_t* card = makeCard(tab, colX, y, colW, 48);
        lv_obj_t* c = makeCaption(card, caption);
        if (captionLabel) *captionLabel = c;
        *value = makeLabel(card, &lv_font_montserrat_14, kText, "");
        lv_label_set_recolor(*value, true);
        lv_obj_align(*value, LV_ALIGN_TOP_RIGHT, 0, -2);
        lv_obj_t* row = lv_obj_create(card);
        lv_obj_remove_style_all(row);
        lv_obj_set_size(row, LV_PCT(100), 16);
        lv_obj_align(row, LV_ALIGN_BOTTOM_LEFT, 0, 2);
        lv_obj_set_flex_flow(row, LV_FLEX_FLOW_ROW);
        lv_obj_set_flex_align(row, LV_FLEX_ALIGN_START, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
        lv_obj_set_style_pad_column(row, 10, 0);
        noScroll(row);
        return row;
    };

    lv_obj_t* memRow = rowCard(0, "MEMORY", nullptr, &memLabel);
    memBar = makeBar(memRow, 10, 6, kGreen);
    lv_obj_set_flex_grow(memBar, 1);
    swapLabel = makeLabel(memRow, &lv_font_montserrat_12, kTextFaint, "");
    swapBar = makeBar(memRow, 28, 4, kGreen);

    lv_obj_t* slotRow = rowCard(48 + kGap, "GPU", &slotCaption, &slotValue);
    slotName = makeLabel(slotRow, &lv_font_montserrat_14, kText, "");
    lv_label_set_long_mode(slotName, LV_LABEL_LONG_DOT);
    lv_obj_set_flex_grow(slotName, 1);
    slotBar = makeBar(slotRow, 10, 6, kCyan);
    lv_obj_set_flex_grow(slotBar, 1);
    slotSub = makeLabel(slotRow, &lv_font_montserrat_12, kTextFaint, "");

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
    const lv_coord_t chartY = 60 + kGap, chartH = kBodyH - chartY;
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
}

// Lays the disk cards out for `count` disks: two by two for three or four,
// one per row otherwise.
void layoutDisks(uint8_t count) {
    if (count == diskLayout) return;
    diskLayout = count;
    const bool grid = count > 2;
    diskGrid = grid;
    const lv_coord_t w = grid ? (kBodyW - kGap) / 2 : kBodyW, h = (kBodyH - kGap) / 2;
    for (size_t i = 0; i < kMaxDisks; i++) {
        DiskCard& d = diskCards[i];
        if (i >= count) {
            lv_obj_add_flag(d.card, LV_OBJ_FLAG_HIDDEN);
            continue;
        }
        lv_obj_clear_flag(d.card, LV_OBJ_FLAG_HIDDEN);
        lv_obj_set_size(d.card, w, h);
        lv_obj_set_pos(d.card, grid ? (i % 2) * (w + kGap) : 0, (grid ? i / 2 : i) * (h + kGap));
        lv_obj_set_width(d.mount, w - 2 - 24 - 20 - 44);  // leaves room for the icon and the tag
    }
    if (count) lv_obj_add_flag(noDisksLabel, LV_OBJ_FLAG_HIDDEN);
    else lv_obj_clear_flag(noDisksLabel, LV_OBJ_FLAG_HIDDEN);
}

void buildStorageTab(lv_obj_t* tab) {
    noScroll(tab);
    for (DiskCard& d : diskCards) {
        d.card = makeCard(tab, 0, 0, kBodyW, 100);
        lv_obj_set_style_pad_ver(d.card, 10, 0);
        d.icon = makeLabel(d.card, &lv_font_montserrat_14, kTextDim, LV_SYMBOL_DRIVE);
        lv_obj_set_pos(d.icon, 0, 0);
        d.mount = makeLabel(d.card, &lv_font_montserrat_14, kText, "");
        lv_label_set_long_mode(d.mount, LV_LABEL_LONG_DOT);
        lv_obj_set_pos(d.mount, 22, 0);
        d.tag = makeLabel(d.card, &lv_font_montserrat_12, kTextDim, "USB");
        lv_obj_set_style_border_color(d.tag, lv_color_hex(kCardBorder), 0);
        lv_obj_set_style_border_width(d.tag, 1, 0);
        lv_obj_set_style_radius(d.tag, 6, 0);
        lv_obj_set_style_pad_hor(d.tag, 6, 0);
        lv_obj_set_style_pad_ver(d.tag, 1, 0);
        lv_obj_align(d.tag, LV_ALIGN_TOP_RIGHT, 0, -1);
        d.percent = makeLabel(d.card, &lv_font_montserrat_28, kText, "");
        lv_obj_set_pos(d.percent, 0, 22);
        d.usage = makeLabel(d.card, &lv_font_montserrat_12, kTextDim, "");
        lv_obj_align(d.usage, LV_ALIGN_TOP_RIGHT, 0, 36);
        d.bar = makeBar(d.card, LV_PCT(100), 8, kGreen);
        lv_obj_set_style_radius(d.bar, 4, LV_PART_MAIN);
        lv_obj_set_style_radius(d.bar, 4, LV_PART_INDICATOR);
        lv_obj_set_pos(d.bar, 0, 60);
        d.free = makeLabel(d.card, &lv_font_montserrat_12, kTextFaint, "");
        lv_obj_set_pos(d.free, 0, 76);
        d.io = makeLabel(d.card, &lv_font_montserrat_12, kTextFaint, "");
        lv_label_set_recolor(d.io, true);
        lv_obj_align(d.io, LV_ALIGN_TOP_RIGHT, 0, 76);
    }
    noDisksLabel = makeLabel(tab, &lv_font_montserrat_14, kTextDim, "No disks to show");
    lv_obj_center(noDisksLabel);
    layoutDisks(0);
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

// A full-face image darkens while pressed; the button's own pressed colours
// are hidden under it.
void onImagePress(lv_event_t* e) {
    const auto idx = reinterpret_cast<uintptr_t>(lv_event_get_user_data(e));
    if (idx >= slotCount || !slots[idx].img) return;
    const bool down = lv_event_get_code(e) == LV_EVENT_PRESSED;
    lv_obj_set_style_img_recolor(slots[idx].img, lv_color_black(), 0);
    lv_obj_set_style_img_recolor_opa(slots[idx].img, down ? LV_OPA_40 : LV_OPA_TRANSP, 0);
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
    lv_obj_align(ver, LV_ALIGN_BOTTOM_LEFT, 12, -8);

    // A quiet byline; "NERD" a shade brighter, echoing the wordmark.
    lv_obj_t* byline = makeLabel(waitScreen, &lv_font_montserrat_12, kTextFaint, "by Funky#C3CAD5 NERD#");
    lv_label_set_recolor(byline, true);
    lv_obj_align(byline, LV_ALIGN_BOTTOM_RIGHT, -12, -8);
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

void notifYExec(void* card, int32_t y) { lv_obj_set_y(static_cast<lv_obj_t*>(card), y); }

void notifTimeExec(void* bar, int32_t w) { lv_obj_set_width(static_cast<lv_obj_t*>(bar), w); }

void opaExec(void* obj, int32_t v) {
    lv_obj_set_style_opa(static_cast<lv_obj_t*>(obj), static_cast<lv_opa_t>(v), 0);
}

void scrimExec(void* scrim, int32_t v) {
    lv_obj_set_style_bg_opa(static_cast<lv_obj_t*>(scrim), static_cast<lv_opa_t>(v), 0);
}

// Fades and slides up into place.
void riseExec(void* obj, int32_t v) {
    auto* o = static_cast<lv_obj_t*>(obj);
    lv_obj_set_style_opa(o, static_cast<lv_opa_t>(v * LV_OPA_COVER / 1000), 0);
    lv_obj_set_style_translate_y(o, static_cast<lv_coord_t>(10 - v * 10 / 1000), 0);
}

// One ring: grows from the disc's edge to 1.8x as it fades.
void ringExec(void* ring, int32_t v) {
    auto* r = static_cast<lv_obj_t*>(ring);
    const lv_coord_t size = kAlertDisc + kAlertDisc * 8 * v / 10000;
    lv_obj_set_size(r, size, size);
    lv_obj_set_style_border_opa(r, static_cast<lv_opa_t>(LV_OPA_60 * (1000 - v) / 1000), 0);
}

void animate(void* var, lv_anim_exec_xcb_t exec, int32_t from, int32_t to, uint32_t ms,
             lv_anim_path_cb_t path = lv_anim_path_ease_out, uint32_t delay = 0,
             lv_anim_ready_cb_t ready = nullptr) {
    lv_anim_del(var, exec);
    lv_anim_t a;
    lv_anim_init(&a);
    lv_anim_set_var(&a, var);
    lv_anim_set_exec_cb(&a, exec);
    lv_anim_set_values(&a, from, to);
    lv_anim_set_time(&a, ms);
    lv_anim_set_delay(&a, delay);
    lv_anim_set_path_cb(&a, path);
    if (ready) lv_anim_set_ready_cb(&a, ready);
    lv_anim_start(&a);
}

void dismissNotification() {
    if (lv_obj_has_flag(notifCard, LV_OBJ_FLAG_HIDDEN)) return;
    lv_anim_del(notifTimeBar, notifTimeExec);
    animate(notifCard, notifYExec, lv_obj_get_y(notifCard), kNotifHiddenY, 250, lv_anim_path_ease_in,
            0, [](lv_anim_t*) {
                lv_obj_add_flag(notifCard, LV_OBJ_FLAG_HIDDEN);
                lv_obj_add_flag(notifScrim, LV_OBJ_FLAG_HIDDEN);
            });
    animate(notifScrim, scrimExec, lv_obj_get_style_bg_opa(notifScrim, 0), LV_OPA_TRANSP, 250);
}

void dismissAlert() {
    if (lv_obj_has_flag(alertScreen, LV_OBJ_FLAG_HIDDEN)) return;
    animate(alertScreen, opaExec, lv_obj_get_style_opa(alertScreen, 0), LV_OPA_TRANSP, 200,
            lv_anim_path_ease_in, 0, [](lv_anim_t*) {
                lv_obj_add_flag(alertScreen, LV_OBJ_FLAG_HIDDEN);
                for (lv_obj_t* r : alertRings) lv_anim_del(r, ringExec);
            });
}

// Height for up to two lines of `text`, for a LONG_DOT label to cut at.
lv_coord_t twoLineHeight(const char* text, const lv_font_t* font, lv_coord_t width) {
    lv_point_t size;
    lv_txt_get_size(&size, text, font, 0, 0, width, LV_TEXT_FLAG_NONE);
    const lv_coord_t twoLines = 2 * lv_font_get_line_height(font);
    return size.y < twoLines ? size.y : twoLines;
}

void setUpper(lv_obj_t* label, const char* text) {
    char buf[40];
    size_t i = 0;
    for (; text[i] && i < sizeof buf - 1; i++) buf[i] = static_cast<char>(toupper(text[i]));
    buf[i] = '\0';
    lv_label_set_text(label, buf);
}

void buildNotificationCard() {
    notifScrim = makeRect(lv_layer_top(), kScreenWidth, kScreenHeight, kBg, 0);
    lv_obj_set_style_bg_opa(notifScrim, LV_OPA_TRANSP, 0);
    lv_obj_add_flag(notifScrim, LV_OBJ_FLAG_CLICKABLE | LV_OBJ_FLAG_HIDDEN);
    lv_obj_add_event_cb(notifScrim, [](lv_event_t*) { dismissNotification(); }, LV_EVENT_CLICKED,
                        nullptr);

    notifCard = lv_obj_create(lv_layer_top());
    lv_obj_remove_style_all(notifCard);
    lv_obj_set_size(notifCard, kNotifW, LV_SIZE_CONTENT);
    lv_obj_set_style_bg_color(notifCard, lv_color_hex(kCard), 0);
    lv_obj_set_style_bg_opa(notifCard, LV_OPA_COVER, 0);
    lv_obj_set_style_border_color(notifCard, lv_color_hex(0x1D5A63), 0);
    lv_obj_set_style_border_width(notifCard, 1, 0);
    lv_obj_set_style_radius(notifCard, 14, 0);
    lv_obj_set_style_pad_hor(notifCard, kNotifPadH, 0);
    lv_obj_set_style_pad_top(notifCard, kNotifPadTop, 0);
    lv_obj_set_style_pad_bottom(notifCard, kNotifPadBottom, 0);
    lv_obj_set_style_pad_column(notifCard, 14, 0);
    lv_obj_set_flex_flow(notifCard, LV_FLEX_FLOW_ROW);
    lv_obj_set_flex_align(notifCard, LV_FLEX_ALIGN_START, LV_FLEX_ALIGN_START, LV_FLEX_ALIGN_START);
    lv_obj_align(notifCard, LV_ALIGN_TOP_MID, 0, kNotifHiddenY);
    noScroll(notifCard);
    lv_obj_add_flag(notifCard, LV_OBJ_FLAG_HIDDEN);
    lv_obj_add_event_cb(notifCard, [](lv_event_t*) { dismissNotification(); }, LV_EVENT_CLICKED,
                        nullptr);

    lv_obj_t* tile = makeRect(notifCard, kNotifTile, kNotifTile, 0x12333A, 12);
    lv_obj_center(makeLabel(tile, &lv_font_montserrat_20, kCyan, LV_SYMBOL_BELL));

    lv_obj_t* col = lv_obj_create(notifCard);
    lv_obj_remove_style_all(col);
    lv_obj_set_size(col, kNotifTextW, LV_SIZE_CONTENT);
    lv_obj_set_flex_flow(col, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_style_pad_row(col, 4, 0);
    lv_obj_clear_flag(col, LV_OBJ_FLAG_CLICKABLE);
    noScroll(col);

    notifApp = makeLabel(col, &lv_font_montserrat_12, kCyan, "");
    lv_obj_set_style_text_letter_space(notifApp, 2, 0);
    lv_obj_set_width(notifApp, kNotifTextW);
    lv_label_set_long_mode(notifApp, LV_LABEL_LONG_DOT);
    notifSummary = makeLabel(col, &lv_font_montserrat_20, kText, "");
    lv_obj_set_size(notifSummary, kNotifTextW, lv_font_get_line_height(&lv_font_montserrat_20));
    lv_label_set_long_mode(notifSummary, LV_LABEL_LONG_DOT);
    notifBody = makeLabel(col, &lv_font_montserrat_14, 0xC3CAD5, "");
    lv_obj_set_width(notifBody, kNotifTextW);
    lv_label_set_long_mode(notifBody, LV_LABEL_LONG_DOT);

    notifTimeBar = makeRect(notifCard, kNotifTimeW, 3, kCyan, 2);
    lv_obj_add_flag(notifTimeBar, LV_OBJ_FLAG_FLOATING);
    lv_obj_align(notifTimeBar, LV_ALIGN_BOTTOM_LEFT, 0, kNotifPadBottom - 7);
}

void buildAlertScreen() {
    alertScreen = makeRect(lv_layer_top(), kScreenWidth, kScreenHeight, 0x140A0C, 0);
    lv_obj_set_style_opa(alertScreen, LV_OPA_TRANSP, 0);  // faded in by showAlert
    lv_obj_add_flag(alertScreen, LV_OBJ_FLAG_HIDDEN);
    noScroll(alertScreen);
    makeRect(alertScreen, kScreenWidth, 3, kRed, 0);  // stripe, like the header's

    lv_obj_t* col = lv_obj_create(alertScreen);
    lv_obj_remove_style_all(col);
    lv_obj_set_size(col, kScreenWidth, LV_SIZE_CONTENT);
    lv_obj_set_flex_flow(col, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_flex_align(col, LV_FLEX_ALIGN_START, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
    lv_obj_set_style_pad_row(col, 14, 0);
    lv_obj_add_flag(col, LV_OBJ_FLAG_OVERFLOW_VISIBLE);
    lv_obj_clear_flag(col, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_center(col);

    // Rings behind the icon disc; they spill outside its box as they grow.
    lv_obj_t* icon = lv_obj_create(col);
    lv_obj_remove_style_all(icon);
    lv_obj_set_size(icon, kAlertDisc, kAlertDisc);
    lv_obj_add_flag(icon, LV_OBJ_FLAG_OVERFLOW_VISIBLE);
    lv_obj_clear_flag(icon, LV_OBJ_FLAG_CLICKABLE);
    for (lv_obj_t*& ring : alertRings) {
        ring = lv_obj_create(icon);
        lv_obj_remove_style_all(ring);
        lv_obj_set_size(ring, kAlertDisc, kAlertDisc);
        lv_obj_set_style_radius(ring, LV_RADIUS_CIRCLE, 0);
        lv_obj_set_style_border_color(ring, lv_color_hex(kRed), 0);
        lv_obj_set_style_border_width(ring, 2, 0);
        lv_obj_clear_flag(ring, LV_OBJ_FLAG_CLICKABLE);
        lv_obj_align(ring, LV_ALIGN_CENTER, 0, 0);
    }
    lv_obj_t* disc = makeRect(icon, kAlertDisc, kAlertDisc, 0x3A1C1F, LV_RADIUS_CIRCLE);
    lv_obj_center(makeLabel(disc, &lv_font_montserrat_28, kRed, LV_SYMBOL_WARNING));

    alertText = lv_obj_create(col);
    lv_obj_remove_style_all(alertText);
    lv_obj_set_size(alertText, kAlertTextW, LV_SIZE_CONTENT);
    lv_obj_set_flex_flow(alertText, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_flex_align(alertText, LV_FLEX_ALIGN_START, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
    lv_obj_set_style_pad_row(alertText, 6, 0);
    lv_obj_clear_flag(alertText, LV_OBJ_FLAG_CLICKABLE);

    alertApp = makeLabel(alertText, &lv_font_montserrat_12, 0xF28B87, "");
    lv_obj_set_style_text_letter_space(alertApp, 3, 0);
    alertSummary = makeLabel(alertText, &lv_font_montserrat_28, kText, "");
    lv_obj_set_size(alertSummary, kAlertTextW, lv_font_get_line_height(&lv_font_montserrat_28));
    lv_label_set_long_mode(alertSummary, LV_LABEL_LONG_DOT);
    lv_obj_set_style_text_align(alertSummary, LV_TEXT_ALIGN_CENTER, 0);
    alertBody = makeLabel(alertText, &lv_font_montserrat_14, 0xC3CAD5, "");
    lv_obj_set_width(alertBody, 380);
    lv_label_set_long_mode(alertBody, LV_LABEL_LONG_DOT);
    lv_obj_set_style_text_align(alertBody, LV_TEXT_ALIGN_CENTER, 0);

    alertButton = lv_obj_create(col);
    lv_obj_remove_style_all(alertButton);
    lv_obj_set_size(alertButton, 200, 44);
    lv_obj_set_style_radius(alertButton, LV_RADIUS_CIRCLE, 0);
    lv_obj_set_style_border_color(alertButton, lv_color_hex(kRed), 0);
    lv_obj_set_style_border_width(alertButton, 1, 0);
    lv_obj_set_style_bg_color(alertButton, lv_color_hex(0x3A1C1F), 0);
    lv_obj_set_style_bg_opa(alertButton, LV_OPA_COVER, LV_STATE_PRESSED);
    lv_obj_center(makeLabel(alertButton, &lv_font_montserrat_14, kText, "Dismiss"));
    lv_obj_add_event_cb(alertButton, [](lv_event_t*) { dismissAlert(); }, LV_EVENT_CLICKED, nullptr);
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

    buildNotificationCard();
    buildAlertScreen();
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

// The Battery tab comes and goes with the host's battery; if it goes while
// open, System opens instead.
void showBatteryTab(bool show) {
    if (show == hasBattery) return;
    hasBattery = show;
    if (show) lv_obj_clear_flag(tabPills[kTabBattery], LV_OBJ_FLAG_HIDDEN);
    else lv_obj_add_flag(tabPills[kTabBattery], LV_OBJ_FLAG_HIDDEN);
    if (!userNavigated) openTab(startingTab(), LV_ANIM_OFF);  // it may be the start tab
    else if (!show && currentTab == kTabBattery) openTab(startingTab(), LV_ANIM_OFF);
}

void updateBattery(const HostStats& s) {
    showBatteryTab(s.battery.present);
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


// ---- Battery screen ----

// Like lv_label_set_text_fmt, but with C's printf, which can format floats
// (LVGL's own can't, as configured).
void setTextf(lv_obj_t* label, const char* fmt, ...) {
    char buf[96];
    va_list args;
    va_start(args, fmt);
    vsnprintf(buf, sizeof buf, fmt, args);
    va_end(args);
    lv_label_set_text(label, buf);
}

// "1 h 05 min" or "25 min".
void fmtMinutes(int32_t minutes, char* out, size_t n) {
    if (minutes >= 60) snprintf(out, n, "%d h %02d min", static_cast<int>(minutes / 60), static_cast<int>(minutes % 60));
    else snprintf(out, n, "%d min", static_cast<int>(minutes));
}

// One labelled value in the details card.
lv_obj_t* makeDetail(lv_obj_t* parent, lv_coord_t x, lv_coord_t y, const char* caption) {
    lv_obj_t* c = makeCaption(parent, caption);
    lv_obj_set_pos(c, x, y);
    lv_obj_t* v = makeLabel(parent, &lv_font_montserrat_14, kText, "--");
    lv_label_set_recolor(v, true);
    lv_obj_set_pos(v, x, y + 17);
    return v;
}

void buildBatteryTab(lv_obj_t* tab) {
    noScroll(tab);
    constexpr lv_coord_t top = 0;
    constexpr lv_coord_t leftW = 172, rightX = leftW + kGap, rightW = kBodyW - rightX;
    constexpr lv_coord_t bodyH = kBodyH;

    // Charge: percentage, what it's doing, a bar with the charge limit marked,
    // and where the power comes from.
    lv_obj_t* charge = makeCard(tab, 0, top, leftW, bodyH);
    makeCaption(charge, "CHARGE");
    batPercent = makeLabel(charge, &lv_font_montserrat_40, kText, "--");
    lv_obj_set_pos(batPercent, 0, 20);
    batStatus = makeLabel(charge, &lv_font_montserrat_12, kTextDim, "");
    lv_obj_set_width(batStatus, leftW - 26);
    lv_label_set_long_mode(batStatus, LV_LABEL_LONG_WRAP);
    lv_obj_set_pos(batStatus, 0, 70);
    const lv_coord_t barW = leftW - 26;
    batBar = makeBar(charge, barW, 10, kGreen);
    lv_obj_set_pos(batBar, 0, 112);
    batLimitMark = makeRect(charge, 2, 18, kText, 1);
    lv_obj_set_pos(batLimitMark, 0, 108);
    batLimitLabel = makeLabel(charge, &lv_font_montserrat_12, kTextFaint, "");
    lv_obj_set_pos(batLimitLabel, 0, 132);
    lv_obj_t* source = makeCaption(charge, "POWER SOURCE");
    lv_obj_set_pos(source, 0, 182);
    batSource = makeLabel(charge, &lv_font_montserrat_14, kText, "--");
    lv_obj_set_width(batSource, barW);
    lv_label_set_long_mode(batSource, LV_LABEL_LONG_DOT);
    lv_obj_set_pos(batSource, 0, 200);

    // Power now, and over the last ten minutes.
    constexpr lv_coord_t powerH = 158;
    lv_obj_t* power = makeCard(tab, rightX, top, rightW, powerH);
    makeCaption(power, "POWER");
    batPeak = makeLabel(power, &lv_font_montserrat_12, kTextFaint, "");
    lv_obj_align(batPeak, LV_ALIGN_TOP_RIGHT, 0, 0);
    batWatts = makeLabel(power, &lv_font_montserrat_28, kCyan, "--");
    lv_obj_set_pos(batWatts, 0, 18);
    batWattsSub = makeLabel(power, &lv_font_montserrat_12, kTextDim, "");
    lv_obj_align_to(batWattsSub, batWatts, LV_ALIGN_OUT_RIGHT_BOTTOM, 8, -4);

    batChart = lv_chart_create(power);
    lv_obj_set_size(batChart, rightW - 26, powerH - 2 - 16 - 56);
    lv_obj_align(batChart, LV_ALIGN_BOTTOM_LEFT, 0, 0);
    lv_obj_set_style_bg_opa(batChart, LV_OPA_TRANSP, 0);
    lv_obj_set_style_border_width(batChart, 0, 0);
    lv_obj_set_style_radius(batChart, 0, 0);
    lv_obj_set_style_pad_all(batChart, 0, 0);
    lv_obj_set_style_pad_ver(batChart, 4, 0);
    lv_obj_set_style_line_color(batChart, lv_color_hex(kTrack), LV_PART_MAIN);
    lv_obj_set_style_line_width(batChart, 2, LV_PART_ITEMS);
    lv_obj_set_style_size(batChart, 0, LV_PART_INDICATOR);
    lv_chart_set_type(batChart, LV_CHART_TYPE_LINE);
    lv_chart_set_point_count(batChart, kPowerHistory);
    lv_chart_set_range(batChart, LV_CHART_AXIS_PRIMARY_Y, 0, kPowerChartMax);
    lv_chart_set_div_line_count(batChart, 3, 0);
    batSeries = lv_chart_add_series(batChart, lv_color_hex(kCyan), LV_CHART_AXIS_PRIMARY_Y);
    for (lv_coord_t& p : powerPoints) p = LV_CHART_POINT_NONE;
    lv_chart_set_ext_y_array(batChart, batSeries, powerPoints);

    // Details, two by two.
    const lv_coord_t detailsY = top + powerH + kGap;
    lv_obj_t* details = makeCard(tab, rightX, detailsY, rightW, bodyH - detailsY);
    const lv_coord_t col = (rightW - 26) / 2;
    batHealth = makeDetail(details, 0, 0, "HEALTH");
    batCapacity = makeDetail(details, col, 0, "CAPACITY");
    batVolts = makeDetail(details, 0, 42, "VOLTAGE");
    batTemp = makeDetail(details, col, 42, "TEMPERATURE");
}

// One chart point per kPowerSampleMs: the average power over it.
void recordPower(float watts) {
    const uint32_t now = lv_tick_get();
    if (!powerSamples) powerSampleStart = now;
    powerSum += watts;
    powerSamples++;
    if (now - powerSampleStart < kPowerSampleMs) return;

    const float avg = powerSum / powerSamples;
    powerSum = 0;
    powerSamples = 0;
    memmove(powerHist, powerHist + 1, (kPowerHistory - 1) * sizeof powerHist[0]);
    powerHist[kPowerHistory - 1] = avg;
    if (powerCount < kPowerHistory) powerCount++;

    float peak = 5.0f;  // W; quieter than this still reads as low
    for (size_t i = kPowerHistory - powerCount; i < kPowerHistory; i++)
        if (powerHist[i] > peak) peak = powerHist[i];
    peak *= 1.15f;
    for (size_t i = 0; i < kPowerHistory; i++)
        powerPoints[i] = i < kPowerHistory - powerCount
                             ? LV_CHART_POINT_NONE
                             : static_cast<lv_coord_t>(powerHist[i] / peak * kPowerChartMax);
    lv_chart_refresh(batChart);
    setTextf(batPeak, "%.0f W  " LV_SYMBOL_BULLET "  10 min", static_cast<double>(peak));
}

void updateBatteryTab(const HostStats& s) {
    using State = HostStats::BatteryState;
    const auto& b = s.battery;
    if (!b.present) return;
    if (!isnan(b.watts)) recordPower(b.watts);

    const uint8_t p = b.percent > 100 ? 100 : b.percent;
    const uint32_t level = p <= 10 ? kRed : p <= 20 ? kOrange : kGreen;
    lv_label_set_text_fmt(batPercent, "%u%%", static_cast<unsigned>(p));
    lv_bar_set_value(batBar, p, LV_ANIM_ON);
    lv_obj_set_style_bg_color(batBar, lv_color_hex(level), LV_PART_INDICATOR);

    char t[24], buf[64];
    const bool timed = b.minutes >= 0;
    if (timed) fmtMinutes(b.minutes, t, sizeof t);
    switch (b.state) {
        case State::Charging:
            if (timed && b.limit) snprintf(buf, sizeof buf, "Charging, %u%% in %s", static_cast<unsigned>(b.limit), t);
            else if (timed) snprintf(buf, sizeof buf, "Charging, full in %s", t);
            else snprintf(buf, sizeof buf, "Charging");
            break;
        case State::Discharging:
            if (timed) snprintf(buf, sizeof buf, "On battery, %s left", t);
            else snprintf(buf, sizeof buf, "On battery");
            break;
        case State::Full:
            snprintf(buf, sizeof buf, "Fully charged");
            break;
        case State::Idle:
            if (b.limit && p + 1 >= b.limit) snprintf(buf, sizeof buf, "Held at the %u%% limit", static_cast<unsigned>(b.limit));
            else snprintf(buf, sizeof buf, "Plugged in, not charging");
            break;
    }
    lv_label_set_text(batStatus, buf);

    if (b.limit) {
        lv_obj_clear_flag(batLimitMark, LV_OBJ_FLAG_HIDDEN);
        lv_obj_set_x(batLimitMark, lv_obj_get_width(batBar) * b.limit / 100 - 1);
        lv_label_set_text_fmt(batLimitLabel, "Charging stops at %u%%", static_cast<unsigned>(b.limit));
    } else {
        lv_obj_add_flag(batLimitMark, LV_OBJ_FLAG_HIDDEN);
        lv_label_set_text(batLimitLabel, "");
    }

    if (!b.ac) lv_label_set_text(batSource, LV_SYMBOL_BATTERY_FULL " Battery");
    else if (!isnan(b.charger_w)) setTextf(batSource, LV_SYMBOL_USB " Charger, %.0f W", static_cast<double>(b.charger_w));
    else lv_label_set_text(batSource, LV_SYMBOL_CHARGE " Mains");

    const bool charging = b.state == State::Charging;
    const uint32_t flow = charging ? kCyan : kOrange;
    if (isnan(b.watts)) {
        lv_label_set_text(batWatts, "--");
        lv_label_set_text(batWattsSub, "");
    } else {
        setTextf(batWatts, "%.1f W", static_cast<double>(b.watts));
        lv_label_set_text(batWattsSub, charging ? "into the battery" : b.watts < 0.05f ? "" : "from the battery");
    }
    lv_obj_set_style_text_color(batWatts, lv_color_hex(flow), 0);
    lv_obj_align_to(batWattsSub, batWatts, LV_ALIGN_OUT_RIGHT_BOTTOM, 8, -4);
    lv_chart_set_series_color(batChart, batSeries, lv_color_hex(flow));

    const auto faint = static_cast<unsigned>(kTextFaint);
    if (!isnan(b.full_wh) && !isnan(b.design_wh) && b.design_wh > 0) {
        float health = b.full_wh / b.design_wh * 100;
        if (health > 100) health = 100;
        if (b.cycles) setTextf(batHealth, "%.0f%% #%06X %u cycles#", static_cast<double>(health), faint, b.cycles);
        else setTextf(batHealth, "%.0f%%", static_cast<double>(health));
    } else {
        lv_label_set_text(batHealth, "--");
    }
    if (!isnan(b.wh) && !isnan(b.full_wh))
        setTextf(batCapacity, "%.0f #%06X of# %.0f Wh", static_cast<double>(b.wh), faint, static_cast<double>(b.full_wh));
    else lv_label_set_text(batCapacity, "--");
    if (!isnan(b.volts)) setTextf(batVolts, "%.1f V", static_cast<double>(b.volts));
    else lv_label_set_text(batVolts, "--");
    if (!isnan(b.temp)) setTextf(batTemp, "%.1f C", static_cast<double>(b.temp));
    else lv_label_set_text(batTemp, "--");
}

}  // namespace

void init() {
    lv_obj_set_style_bg_color(lv_scr_act(), lv_color_hex(kBg), 0);

    tabview = lv_tabview_create(lv_scr_act(), LV_DIR_TOP, 0);  // the header is drawn by buildHeader()
    lv_obj_set_style_bg_color(tabview, lv_color_hex(kBg), 0);
    lv_obj_set_style_bg_opa(tabview, LV_OPA_COVER, 0);
    lv_obj_set_style_bg_opa(lv_tabview_get_content(tabview), LV_OPA_TRANSP, 0);

    // In Tab order.
    lv_obj_t** tabs = tabPages;
    for (uint16_t i = 0; i < kTabCount; i++) {
        tabs[i] = lv_tabview_add_tab(tabview, kTabNames[i]);
        lv_obj_set_style_pad_all(tabs[i], kGap, 0);
        lv_obj_set_style_pad_top(tabs[i], kHeaderH + 3, 0);
    }
    buildHeader();
    buildSystemTab(tabs[kTabSystem]);
    buildNetworkTab(tabs[kTabNetwork]);
    buildStorageTab(tabs[kTabStorage]);
    buildBatteryTab(tabs[kTabBattery]);
    buildControlsTab(tabs[kTabControls]);
    buildOverlays();
    buildPower();
#ifdef NERD_START_TAB
    // Debug: open on a given tab (0 System, 1 Network, 2 Storage, 3 Battery, 4 Controls).
    userNavigated = true;
    openTab(NERD_START_TAB, LV_ANIM_OFF);
#endif
}

void applyDisplay(const settings::Display& d) {
    setTabOrder(d.tabs);
    startTab = d.start;
    if (!userNavigated) openTab(startingTab(), LV_ANIM_OFF);
    brightness = d.brightness;
    dimAfterMs = d.dimAfterS * 1000;
    offAfterMs = d.offAfterS * 1000;
    tempWarm = d.tempWarm;
    tempHot = d.tempHot;
    powerTick(nullptr);
}

void updateStats(const HostStats& s) {
    char a[16], b[16], buf[160];

    updateBattery(s);
    updateBatteryTab(s);

    // CPU
    animateCpu(static_cast<int32_t>(s.cpu.usage + 0.5f));
    // setTextf: LVGL's built-in printf has no float support by default.
    setTextf(cpuFreqLabel, "%.1f GHz", s.cpu.freq_mhz / 1000.0);
    // The built-in fonts are ASCII-only, so no degree sign.
    if (s.cpu.has_temp) {
        setTextf(cpuTempLabel, "%.0f C", static_cast<double>(s.cpu.temp));
        const uint32_t color = s.cpu.temp >= tempHot ? kRed : s.cpu.temp >= tempWarm ? kOrange : kTextDim;
        lv_obj_set_style_text_color(cpuTempLabel, lv_color_hex(color), 0);
    } else {
        lv_label_set_text(cpuTempLabel, "");
    }

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

    // Swap: just how much is used, and of what once it's in use.
    fmtBytes(s.mem.swap_used, a, sizeof a);
    fmtBytes(s.mem.swap_total, b, sizeof b);
    if (!s.mem.swap_total) lv_label_set_text(swapLabel, "no swap");
    else if (!s.mem.swap_used) lv_label_set_text_fmt(swapLabel, "swap %s", a);
    else lv_label_set_text_fmt(swapLabel, "swap %s / %s", a, b);
    if (s.mem.swap_total) lv_obj_clear_flag(swapBar, LV_OBJ_FLAG_HIDDEN);
    else lv_obj_add_flag(swapBar, LV_OBJ_FLAG_HIDDEN);
    lv_bar_set_value(swapBar, pct(s.mem.swap_used, s.mem.swap_total), LV_ANIM_ON);

    // The GPU, or else the busiest program.
    const bool gpu = s.gpu.present;
    lv_label_set_text(slotCaption, gpu ? "GPU" : "BUSIEST");
    if (gpu) {
        lv_obj_add_flag(slotName, LV_OBJ_FLAG_HIDDEN);
        lv_obj_clear_flag(slotBar, LV_OBJ_FLAG_HIDDEN);
        lv_obj_set_style_text_font(slotSub, &lv_font_montserrat_12, 0);
        lv_obj_set_style_text_color(slotSub, lv_color_hex(kTextFaint), 0);
        if (s.gpu.asleep) {
            lv_label_set_text_fmt(slotValue, "#%06X asleep#", static_cast<unsigned>(kTextFaint));
            lv_bar_set_value(slotBar, 0, LV_ANIM_ON);
            lv_label_set_text(slotSub, "powered down");
        } else {
            // The temperature takes the CPU's colours.
            const float t = s.gpu.temp;
            const uint32_t tc = isnan(t) ? kTextFaint : t >= tempHot ? kRed : t >= tempWarm ? kOrange : kTextFaint;
            if (isnan(t)) lv_label_set_text_fmt(slotValue, "%u%%", static_cast<unsigned>(s.gpu.usage));
            else
                setTextf(slotValue, "%u%%  #%06X %.0f C#", static_cast<unsigned>(s.gpu.usage),
                         static_cast<unsigned>(tc), static_cast<double>(t));
            lv_bar_set_value(slotBar, s.gpu.usage, LV_ANIM_ON);
            if (s.gpu.vram_total) {
                fmtBytes(s.gpu.vram_used, a, sizeof a);
                fmtBytes(s.gpu.vram_total, b, sizeof b);
                lv_label_set_text_fmt(slotSub, "VRAM %s / %s", a, b);
            } else {
                lv_label_set_text(slotSub, "");
            }
        }
    } else {
        lv_obj_clear_flag(slotName, LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(slotBar, LV_OBJ_FLAG_HIDDEN);
        lv_obj_set_style_text_font(slotSub, &lv_font_montserrat_14, 0);
        lv_obj_set_style_text_color(slotSub, lv_color_hex(kCyan), 0);
        if (s.top.present) {
            fmtBytes(s.top.mem, a, sizeof a);
            lv_label_set_text_fmt(slotValue, "#%06X %s#", static_cast<unsigned>(kTextFaint), a);
            lv_label_set_text(slotName, s.top.name);
            setTextf(slotSub, "%.0f%% CPU", static_cast<double>(s.top.cpu));
        } else {
            lv_label_set_text(slotValue, "");
            lv_label_set_text(slotName, "--");
            lv_label_set_text(slotSub, "");
        }
    }

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
    layoutDisks(s.disk_count);
    for (size_t i = 0; i < s.disk_count; i++) {
        const auto& d = s.disks[i];
        DiskCard& c = diskCards[i];
        const uint8_t p = pct(d.used, d.total);
        lv_label_set_text(c.icon, d.removable ? LV_SYMBOL_USB : LV_SYMBOL_DRIVE);
        lv_label_set_text(c.mount, d.mount);
        if (d.removable) lv_obj_clear_flag(c.tag, LV_OBJ_FLAG_HIDDEN);
        else lv_obj_add_flag(c.tag, LV_OBJ_FLAG_HIDDEN);
        lv_label_set_text_fmt(c.percent, "%u%%", static_cast<unsigned>(p));
        lv_obj_set_style_text_color(c.percent, fillColor(p), 0);
        fmtBytes(d.used, a, sizeof a);
        fmtBytes(d.total, b, sizeof b);
        lv_label_set_text_fmt(c.usage, "%s of %s", a, b);
        lv_obj_set_style_bg_color(c.bar, fillColor(p), LV_PART_INDICATOR);
        lv_bar_set_value(c.bar, p, LV_ANIM_ON);
        fmtBytes(d.total > d.used ? d.total - d.used : 0, a, sizeof a);
        lv_label_set_text_fmt(c.free, "%s free", a);
        char r[16], w[16];
        fmtBytes(d.read_bps, r, sizeof r);
        fmtBytes(d.write_bps, w, sizeof w);
        // Half-width cards leave out "/s" to fit beside the free space.
        const char* per = diskGrid ? "" : "/s";
        lv_label_set_text_fmt(c.io, "#%06X " LV_SYMBOL_DOWNLOAD " %s%s#  #%06X " LV_SYMBOL_UPLOAD " %s%s#",
                              static_cast<unsigned>(kCyan), r, per, static_cast<unsigned>(kOrange), w, per);
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

        lv_obj_t* label =
            makeLabel(btn, &lv_font_montserrat_14, slot.def.has_color ? slot.def.color : kText, slot.def.label);
        lv_label_set_long_mode(label, LV_LABEL_LONG_WRAP);
        lv_obj_set_width(label, LV_PCT(100));
        lv_obj_set_style_text_align(label, LV_TEXT_ALIGN_CENTER, 0);
        slot.label = label;

        if (slot.def.has_image) {
            // Round the image's corners with the button's.
            lv_obj_set_style_clip_corner(btn, true, 0);
            static const lv_event_code_t kPressEvents[] = {LV_EVENT_PRESSED, LV_EVENT_RELEASED,
                                                           LV_EVENT_PRESS_LOST};
            for (lv_event_code_t code : kPressEvents)
                lv_obj_add_event_cb(btn, onImagePress, code, reinterpret_cast<void*>(static_cast<uintptr_t>(i)));
        }
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
        if (!slot->pixels) {
            protocol::sendImageResult(id, false);
            return;
        }
        slot->dsc = {};
        slot->dsc.header.cf = LV_IMG_CF_TRUE_COLOR_ALPHA;
        slot->dsc.header.w = w;
        slot->dsc.header.h = h;
        slot->dsc.data_size = total;
        slot->dsc.data = slot->pixels;
    }

    // Out-of-order or oversized chunk: drop the partial image.
    if (!slot->pixels || offset != slot->received || offset + len > total) {
        // Tell the host once, so it can send the image again.
        if (slot->pixels) protocol::sendImageResult(id, false);
        freeImage(*slot);
        return;
    }

    memcpy(slot->pixels + offset, data, len);
    slot->received += len;

    if (slot->received == total) {
        protocol::sendImageResult(id, true);
        slot->img = lv_img_create(slot->btn);
        lv_img_set_src(slot->img, &slot->dsc);
        lv_obj_move_to_index(slot->img, 0);
        // Sized to the face (hosts from protocol v5): the image is the
        // button, like a Stream Deck key. Smaller ones sit above the label.
        uint16_t faceW, faceH;
        buttonTileSize(&faceW, &faceH);
        if (w + 2 >= faceW && h + 2 >= faceH) {
            lv_obj_set_style_pad_all(slot->btn, 0, 0);
            lv_obj_add_flag(slot->img, LV_OBJ_FLAG_FLOATING);
            lv_obj_center(slot->img);
            // The label runs across the bottom on a dark band, readable over
            // any picture, as Stream Deck shows titles.
            lv_obj_t* label = slot->label;
            lv_obj_add_flag(label, LV_OBJ_FLAG_FLOATING);
            lv_obj_set_width(label, LV_PCT(100));
            lv_label_set_long_mode(label, LV_LABEL_LONG_DOT);
            lv_obj_set_style_text_font(label, &lv_font_montserrat_12, 0);
            lv_obj_set_style_bg_color(label, lv_color_black(), 0);
            lv_obj_set_style_bg_opa(label, LV_OPA_50, 0);
            lv_obj_set_style_pad_ver(label, 3, 0);
            lv_obj_set_style_pad_hor(label, 4, 0);
            lv_obj_align(label, LV_ALIGN_BOTTOM_MID, 0, 0);
            lv_obj_move_foreground(label);
        }
    }
}

void buttonTileSize(uint16_t* w, uint16_t* h) {
    if (!tileW) {
        lv_obj_update_layout(buttonGrid);
        const lv_coord_t gap = lv_obj_get_style_pad_column(buttonGrid, 0);
        // Four columns, three rows, less each button's 1 px border.
        tileW = (lv_obj_get_content_width(buttonGrid) - 3 * gap) / 4 - 2;
        tileH = (lv_obj_get_content_height(buttonGrid) - 2 * gap) / 3 - 2;
    }
    *w = static_cast<uint16_t>(tileW);
    *h = static_cast<uint16_t>(tileH);
}

void setConnected(bool connected) {
    if (connected != waitShown) return;
    waitShown = !connected;
    wake();  // the away timer starts now; a returning host lights the screen
    if (connected) {
        hideWaitScreen();
    } else {
        dismissNotification();
        dismissAlert();
        showWaitScreen();
    }
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

namespace {

void showCard(const char* app, const char* summary, const char* body) {
    setUpper(notifApp, app);
    lv_label_set_text(notifSummary, summary);
    if (*summary) lv_obj_clear_flag(notifSummary, LV_OBJ_FLAG_HIDDEN);
    else lv_obj_add_flag(notifSummary, LV_OBJ_FLAG_HIDDEN);
    lv_label_set_text(notifBody, body);
    if (*body) {
        lv_obj_set_height(notifBody, twoLineHeight(body, &lv_font_montserrat_14, kNotifTextW));
        lv_obj_clear_flag(notifBody, LV_OBJ_FLAG_HIDDEN);
    } else {
        lv_obj_add_flag(notifBody, LV_OBJ_FLAG_HIDDEN);
    }

    // Dim the screen and slide the card down (or back, if it was leaving).
    if (lv_obj_has_flag(notifCard, LV_OBJ_FLAG_HIDDEN)) {
        lv_obj_set_y(notifCard, kNotifHiddenY);
        lv_obj_clear_flag(notifCard, LV_OBJ_FLAG_HIDDEN);
        lv_obj_clear_flag(notifScrim, LV_OBJ_FLAG_HIDDEN);
    }
    lv_obj_update_layout(notifCard);
    animate(notifScrim, scrimExec, lv_obj_get_style_bg_opa(notifScrim, 0), kNotifScrimOpa, 250);
    animate(notifCard, notifYExec, lv_obj_get_y(notifCard), kNotifY, 320);
    animate(notifTimeBar, notifTimeExec, kNotifTimeW, 0, kNotifMs, lv_anim_path_linear, 0,
            [](lv_anim_t*) { dismissNotification(); });
}

void showAlert(const char* app, const char* summary, const char* body) {
    setUpper(alertApp, app);
    lv_label_set_text(alertSummary, summary);
    lv_label_set_text(alertBody, body);
    if (*body) {
        lv_obj_set_height(alertBody, twoLineHeight(body, &lv_font_montserrat_14, 380));
        lv_obj_clear_flag(alertBody, LV_OBJ_FLAG_HIDDEN);
    } else {
        lv_obj_add_flag(alertBody, LV_OBJ_FLAG_HIDDEN);
    }

    // The card would only sit under the takeover, running out unseen.
    dismissNotification();

    // Fade in, then the text and button rise into place while the rings pulse.
    lv_obj_clear_flag(alertScreen, LV_OBJ_FLAG_HIDDEN);
    lv_obj_update_layout(alertScreen);
    animate(alertScreen, opaExec, lv_obj_get_style_opa(alertScreen, 0), LV_OPA_COVER, 220);
    riseExec(alertText, 0);
    riseExec(alertButton, 0);
    animate(alertText, riseExec, 0, 1000, 400, lv_anim_path_ease_out, 120);
    animate(alertButton, riseExec, 0, 1000, 400, lv_anim_path_ease_out, 200);
    for (size_t i = 0; i < 2; i++) {
        if (lv_anim_get(alertRings[i], ringExec)) continue;  // already pulsing
        ringExec(alertRings[i], 1000);  // invisible until its first beat
        lv_anim_t a;
        lv_anim_init(&a);
        lv_anim_set_var(&a, alertRings[i]);
        lv_anim_set_exec_cb(&a, ringExec);
        lv_anim_set_values(&a, 0, 1000);
        lv_anim_set_time(&a, 1400);
        lv_anim_set_delay(&a, 700 * i);
        lv_anim_set_path_cb(&a, lv_anim_path_ease_out);
        lv_anim_set_repeat_count(&a, LV_ANIM_REPEAT_INFINITE);
        lv_anim_start(&a);
    }
}

}  // namespace

void showNotification(const char* app, const char* summary, const char* body, bool critical) {
    wake();
    if (critical) showAlert(app, summary, body);
    else showCard(app, summary, body);
}

}  // namespace ui
