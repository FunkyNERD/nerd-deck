#include "protocol.hpp"

#include <Arduino.h>
#include <ArduinoJson.h>
#include <mbedtls/base64.h>
#include <math.h>

#include "config.hpp"
#include "settings.hpp"
#include "stats.hpp"
#include "ui/ui.hpp"

// The host reads the board and version of the firmware it bundles from this
// marker in the image (host/build.rs). init() references it so the linker keeps it.
extern "C" const char kFirmwareId[] = "nerd-deck-firmware:" FW_BOARD ":" FW_VERSION ";";

namespace protocol {
namespace {

char lineBuf[kMaxLineLen];
size_t lineLen = 0;
bool overflow = false;

bool everReceived = false;
uint32_t lastRxMs = 0;
uint32_t hostTimeoutMs = kHostTimeoutMs;
// The newest whole stats this poll, drawn at its end.
JsonDocument latestStats;
bool statsPending = false;

void send(const JsonDocument& doc) {
    serializeJson(doc, Serial);
    Serial.write('\n');
}

// The stored settings, as the host's `display` message names them.
void writeDisplay(JsonObject o, const settings::Display& d) {
    using namespace settings;
    // Free custom slots are left out.
    JsonArray tabs = o["tabs"].to<JsonArray>();
    for (uint8_t t : d.tabs)
        if (const char* name = tabName(d, t)) tabs.add(name);
    const char* start = d.start == kStartFirst ? nullptr : tabName(d, d.start);
    o["start"] = start ? start : "first";
    o["brightness"] = d.brightness;
    o["dim_after"] = d.dimAfterS;
    o["off_after"] = d.offAfterS;
    o["temp_warm"] = d.tempWarm;
    o["temp_hot"] = d.tempHot;
    o["flip"] = d.flip;
    JsonArray hidden = o["hidden"].to<JsonArray>();
    for (uint8_t t = 0; t < kTabCount; t++) {
        const char* name = tabName(d, t);
        if (name && d.hidden & (1u << t)) hidden.add(name);
    }
}

void sendDisplay(const settings::Display& d) {
    JsonDocument doc;
    doc["type"] = "display";
    writeDisplay(doc.as<JsonObject>(), d);
    send(doc);
}

void handleStats(JsonObjectConst o) {
    static HostStats s;
    s = {};

    strlcpy(s.host, o["host"] | "", sizeof s.host);
    s.uptime = o["uptime"] | uint64_t(0);
    JsonArrayConst load = o["load"];
    for (size_t i = 0; i < 3; i++) s.load[i] = load[i] | 0.0f;

    JsonObjectConst cpu = o["cpu"];
    s.cpu.usage = cpu["usage"] | 0.0f;
    s.cpu.freq_mhz = cpu["freq_mhz"] | 0u;
    for (JsonVariantConst c : cpu["cores"].as<JsonArrayConst>()) {
        if (s.cpu.core_count >= kMaxCores) break;
        s.cpu.cores[s.cpu.core_count++] = c | 0.0f;
    }
    if (!cpu["temp"].isNull()) {
        s.cpu.has_temp = true;
        s.cpu.temp = cpu["temp"].as<float>();
    }
    strlcpy(s.cpu.profile, cpu["profile"] | "", sizeof s.cpu.profile);

    JsonObjectConst mem = o["mem"];
    s.mem.used = mem["used"] | uint64_t(0);
    s.mem.total = mem["total"] | uint64_t(0);
    s.mem.swap_used = mem["swap_used"] | uint64_t(0);
    s.mem.swap_total = mem["swap_total"] | uint64_t(0);

    JsonObjectConst net = o["net"];
    strlcpy(s.net.iface, net["iface"] | "", sizeof s.net.iface);
    s.net.rx_bps = net["rx_bps"] | uint64_t(0);
    s.net.tx_bps = net["tx_bps"] | uint64_t(0);

    for (JsonObjectConst d : o["disks"].as<JsonArrayConst>()) {
        if (s.disk_count >= kMaxDisks) break;
        auto& disk = s.disks[s.disk_count++];
        strlcpy(disk.mount, d["mount"] | "", sizeof disk.mount);
        disk.used = d["used"] | uint64_t(0);
        disk.total = d["total"] | uint64_t(0);
        disk.removable = d["removable"] | false;
        disk.read_bps = d["read_bps"] | uint64_t(0);
        disk.write_bps = d["write_bps"] | uint64_t(0);
    }

    JsonObjectConst battery = o["battery"];
    if (!battery.isNull()) {
        auto& b = s.battery;
        using State = HostStats::BatteryState;
        b.present = true;
        b.percent = battery["percent"] | 0u;
        b.charging = battery["charging"] | false;
        const char* state = battery["state"] | (b.charging ? "charging" : "discharging");
        b.state = strcmp(state, "charging") == 0 ? State::Charging
                  : strcmp(state, "full") == 0   ? State::Full
                  : strcmp(state, "idle") == 0   ? State::Idle
                                                 : State::Discharging;
        b.watts = battery["watts"] | NAN;
        b.minutes = battery["minutes"] | -1;
        b.wh = battery["wh"] | NAN;
        b.full_wh = battery["full_wh"] | NAN;
        b.design_wh = battery["design_wh"] | NAN;
        b.volts = battery["volts"] | NAN;
        b.temp = battery["temp"] | NAN;
        b.cycles = battery["cycles"] | 0u;
        b.limit = battery["limit"] | 0u;
        b.ac = battery["ac"] | b.charging;
        b.charger_w = battery["charger_w"] | NAN;
    }

    JsonObjectConst gpu = o["gpu"];
    if (!gpu.isNull()) {
        s.gpu.present = true;
        s.gpu.asleep = gpu["asleep"] | false;
        s.gpu.usage = gpu["usage"] | 0u;
        s.gpu.temp = gpu["temp"] | NAN;
        s.gpu.vram_used = gpu["vram_used"] | uint64_t(0);
        s.gpu.vram_total = gpu["vram_total"] | uint64_t(0);
    }
    JsonObjectConst top = o["top"];
    if (!top.isNull()) {
        s.top.present = true;
        strlcpy(s.top.name, top["name"] | "", sizeof s.top.name);
        s.top.cpu = top["cpu"] | 0.0f;
        s.top.mem = top["mem"] | uint64_t(0);
    }

    ui::updateStats(s);
}

void handleButtons(JsonObjectConst o) {
    static ButtonDef defs[kMaxButtons];
    size_t count = 0;
    for (JsonObjectConst b : o["buttons"].as<JsonArrayConst>()) {
        if (count >= kMaxButtons) break;
        strlcpy(defs[count].id, b["id"] | "", sizeof defs[count].id);
        strlcpy(defs[count].label, b["label"] | defs[count].id, sizeof defs[count].label);
        strlcpy(defs[count].icon, b["icon"] | "", sizeof defs[count].icon);
        defs[count].has_image = b["image"] | false;
        defs[count].has_color = !b["color"].isNull();
        defs[count].color = b["color"] | 0u;
        count++;
    }
    ui::setButtons(defs, count);
}

void handleImage(JsonObjectConst o) {
    if (strcmp(o["fmt"] | "", "rgb565a8") != 0) return;

    static uint8_t buf[kMaxImageChunk];
    const char* b64 = o["data"] | "";
    size_t len = 0;
    if (mbedtls_base64_decode(buf, sizeof buf, &len, reinterpret_cast<const unsigned char*>(b64),
                              strlen(b64)) != 0)
        return;

    ui::setButtonImage(o["id"] | "", o["w"] | 0u, o["h"] | 0u, o["offset"] | 0u, o["total"] | 0u,
                       buf, len);
}

void handleAck(JsonObjectConst o) {
    const char* id = o["id"] | "?";
    const bool ok = o["ok"] | false;
    const char* msg = o["msg"] | (ok ? "done" : "failed");
    char text[96];
    snprintf(text, sizeof text, "%s: %s", id, msg);
    ui::showToast(text, ok);
}

void handleNotify(JsonObjectConst o) {
    const char* text = o["text"] | "";
    if (*text) ui::showNotice(text);
}

void handleNotification(JsonObjectConst o) {
    ui::showNotification(o["app"] | "", o["summary"] | "", o["body"] | "", o["critical"] | false);
}

// Missing or unknown fields keep the stored values; tabs left out of `tabs`
// follow in their default order, and `hidden` replaces the tabs turned off.
// Replies with what's now stored.
void handleDisplay(JsonObjectConst o) {
    using namespace settings;
    Display d = load();
    JsonArrayConst tabs = o["tabs"];
    if (!tabs.isNull()) {
        bool placed[kTabCount] = {};
        uint8_t n = 0;
        for (const char* id : tabs) {
            const int8_t t = tabFromName(d, id);
            if (t < 0 || placed[t] || n == kTabCount) continue;
            placed[t] = true;
            d.tabs[n++] = static_cast<uint8_t>(t);
        }
        for (uint8_t t = 0; t < kTabCount; t++)
            if (!placed[t]) d.tabs[n++] = t;
    }
    // Ignored if it would turn every tab off.
    JsonArrayConst hidden = o["hidden"];
    if (!hidden.isNull()) {
        uint8_t mask = 0, named = 0;
        for (uint8_t t = 0; t < kTabCount; t++)
            if (tabName(d, t)) named |= 1u << t;
        for (const char* id : hidden) {
            const int8_t t = tabFromName(d, id);
            if (t >= 0) mask |= 1u << t;
        }
        if (mask != named) d.hidden = mask;
    }
    if (const char* start = o["start"]) {
        const int8_t t = tabFromName(d, start);
        d.start = t >= 0 ? t : kStartFirst;
    }
    if (o["brightness"].is<int>()) d.brightness = constrain(o["brightness"].as<int>(), 5, 100);
    if (o["dim_after"].is<uint32_t>()) d.dimAfterS = o["dim_after"].as<uint32_t>();
    if (o["off_after"].is<uint32_t>()) d.offAfterS = o["off_after"].as<uint32_t>();
    const int warm = o["temp_warm"] | d.tempWarm, hot = o["temp_hot"] | d.tempHot;
    if (warm < hot && warm > 0 && hot <= 150) {
        d.tempWarm = static_cast<uint8_t>(warm);
        d.tempHot = static_cast<uint8_t>(hot);
    }
    if (o["flip"].is<bool>()) d.flip = o["flip"].as<bool>();
    // Not kept: it comes with every connection.
    if (o["stats_ms"].is<uint32_t>())
        hostTimeoutMs = max(kHostTimeoutMs, o["stats_ms"].as<uint32_t>() + kHostTimeoutSlackMs);
    save(d);
    ui::applyDisplay(d);
    sendDisplay(d);
}

// The host's custom tabs, by id (v7). Each gets a slot, kept across restarts
// with its place and on/off, and the reply says where they are.
void handleTabs(JsonObjectConst o) {
    using namespace settings;
    const char* ids[kCustomTabs];
    uint8_t count = 0;
    for (const char* id : o["ids"].as<JsonArrayConst>()) {
        if (count == kCustomTabs) break;
        // Not a built-in name, nor too long to keep.
        if (!id || !*id || strlen(id) >= kCustomIdLen) continue;
        bool builtin = false;
        for (const char* b : kTabIds) builtin |= strcmp(id, b) == 0;
        if (!builtin) ids[count++] = id;
    }
    Display d = load();
    if (assignCustom(d, ids, count)) save(d);
    ui::applyDisplay(d);
    ui::setCustomTabs(ids, count);
    sendDisplay(d);
}

// One custom tab's name, icon, grid and panels (v7).
void handleTab(JsonObjectConst o) {
    static ui::PanelDef panels[kMaxPanels];
    size_t count = 0;
    for (JsonObjectConst p : o["panels"].as<JsonArrayConst>()) {
        if (count == kMaxPanels) break;
        ui::PanelDef& def = panels[count++];
        const char* style = p["style"] | "value";
        def.style = strcmp(style, "status") == 0 ? ui::PanelStyle::Status
                    : strcmp(style, "text") == 0 ? ui::PanelStyle::Text
                                                 : ui::PanelStyle::Value;
        strlcpy(def.title, p["title"] | "", sizeof def.title);
        def.col = p["at"][0] | 0u;
        def.row = p["at"][1] | 0u;
        def.colSpan = p["span"][0] | 1u;
        def.rowSpan = p["span"][1] | 1u;
    }
    ui::setCustomTab(o["id"] | "", o["name"] | "", o["icon"] | "", o["grid"][0] | 1u, o["grid"][1] | 1u, panels,
                     count);
}

// A custom tab's panel has something new to show (v7).
void handlePanel(JsonObjectConst o) {
    const char* state = o["state"] | "";
    const ui::PanelState s = strcmp(state, "ok") == 0     ? ui::PanelState::Ok
                             : strcmp(state, "warn") == 0  ? ui::PanelState::Warn
                             : strcmp(state, "err") == 0   ? ui::PanelState::Err
                             : strcmp(state, "stale") == 0 ? ui::PanelState::Stale
                                                           : ui::PanelState::None;
    ui::setPanel(o["tab"] | "", o["i"] | 0u, o["text"] | "", o["sub"] | "", s);
}

void handleLine(const char* line, size_t len) {
    JsonDocument doc;
    if (deserializeJson(doc, line, len)) return;  // ignore garbage / partial lines

    everReceived = true;
    lastRxMs = millis();

    JsonObjectConst o = doc.as<JsonObjectConst>();
    const char* type = o["type"] | "";
    if (strcmp(type, "stats") == 0) {
        handleStats(o);
    } else if (strcmp(type, "buttons") == 0) {
        handleButtons(o);
    } else if (strcmp(type, "image") == 0) {
        handleImage(o);
    } else if (strcmp(type, "ack") == 0) {
        handleAck(o);
    } else if (strcmp(type, "notify") == 0) {
        handleNotify(o);
    } else if (strcmp(type, "display") == 0) {
        handleDisplay(o);
    } else if (strcmp(type, "notification") == 0) {
        handleNotification(o);
    } else if (strcmp(type, "panel") == 0) {
        handlePanel(o);
    } else if (strcmp(type, "tab") == 0) {
        handleTab(o);
    } else if (strcmp(type, "tabs") == 0) {
        handleTabs(o);
    } else if (strcmp(type, "hello") == 0) {
        sendHello();
    }
    // TODO: "brightness", ... as the protocol grows.
}

}  // namespace

void init() {
    asm volatile("" : : "r"(kFirmwareId));
    sendHello();
}

// A whole line has arrived. Stats are only kept: when they come faster than
// the UI can draw them, only the newest one each poll is worth drawing.
void lineDone(const char* line, size_t len) {
    // A line that starts like a message shows the host is there, even when a
    // flood has broken the rest of it.
    static constexpr char kMessage[] = "{\"type\":\"";
    if (len >= sizeof kMessage - 1 && memcmp(line, kMessage, sizeof kMessage - 1) == 0) {
        everReceived = true;
        lastRxMs = millis();
    }
    static constexpr char kStats[] = "{\"type\":\"stats\"";
    if (len >= sizeof kStats - 1 && memcmp(line, kStats, sizeof kStats - 1) == 0) {
        // A flood loses bytes (the USB driver drops what its queue can't
        // hold), so keep only stats that are whole.
        JsonDocument doc;
        if (deserializeJson(doc, line, len)) return;
        latestStats = std::move(doc);
        statsPending = true;
        return;
    }
    handleLine(line, len);
}

void poll() {
    // Read in chunks, which is far quicker than a byte at a time, and for at
    // most kPollBudgetMs, so the UI still gets its turn while data keeps coming.
    const uint32_t started = millis();
    uint8_t chunk[256];
    while (millis() - started < kPollBudgetMs) {
        const int ready = Serial.available();
        if (ready <= 0) break;
        const size_t n = Serial.read(chunk, min(static_cast<size_t>(ready), sizeof chunk));
        if (n == 0) break;
        for (size_t i = 0; i < n; i++) {
            const char c = static_cast<char>(chunk[i]);
            if (c == '\n') {
                if (!overflow && lineLen > 0) lineDone(lineBuf, lineLen);
                lineLen = 0;
                overflow = false;
            } else if (c == '\r') {
                continue;
            } else if (lineLen < kMaxLineLen) {
                lineBuf[lineLen++] = c;
            } else {
                overflow = true;
            }
        }
    }
    if (statsPending) {
        statsPending = false;
        handleStats(latestStats.as<JsonObjectConst>());
    }
}

bool isConnected() { return everReceived && millis() - lastRxMs < hostTimeoutMs; }

void sendHello() {
    JsonDocument doc;
    doc["type"] = "hello";
    doc["fw"] = FW_VERSION;
    doc["proto"] = kProtoVersion;
    doc["board"] = FW_BOARD;
    uint16_t w, h;
    ui::buttonTileSize(&w, &h);
    JsonArray tile = doc["tile"].to<JsonArray>();
    tile.add(w);
    tile.add(h);
    // This deck's own settings: the host shows and edits them, never overwrites them.
    writeDisplay(doc["display"].to<JsonObject>(), settings::load());
    send(doc);
}

void sendImageResult(const char* id, bool ok) {
    JsonDocument doc;
    doc["type"] = "image";
    doc["id"] = id;
    doc["ok"] = ok;
    send(doc);
}

void sendPress(const char* id) {
    JsonDocument doc;
    doc["type"] = "press";
    doc["id"] = id;
    send(doc);
}

}  // namespace protocol
