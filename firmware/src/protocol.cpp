#include "protocol.hpp"

#include <Arduino.h>
#include <ArduinoJson.h>
#include <mbedtls/base64.h>

#include "config.hpp"
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

void send(const JsonDocument& doc) {
    serializeJson(doc, Serial);
    Serial.write('\n');
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
    }

    JsonObjectConst battery = o["battery"];
    if (!battery.isNull()) {
        s.battery.present = true;
        s.battery.percent = battery["percent"] | 0u;
        s.battery.charging = battery["charging"] | false;
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
    } else if (strcmp(type, "notification") == 0) {
        handleNotification(o);
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

void poll() {
    while (Serial.available() > 0) {
        const int c = Serial.read();
        if (c < 0) break;
        if (c == '\n') {
            if (!overflow && lineLen > 0) handleLine(lineBuf, lineLen);
            lineLen = 0;
            overflow = false;
        } else if (c == '\r') {
            continue;
        } else if (lineLen < kMaxLineLen) {
            lineBuf[lineLen++] = static_cast<char>(c);
        } else {
            overflow = true;
        }
    }
}

bool isConnected() { return everReceived && millis() - lastRxMs < kHostTimeoutMs; }

void sendHello() {
    JsonDocument doc;
    doc["type"] = "hello";
    doc["fw"] = FW_VERSION;
    doc["proto"] = kProtoVersion;
    doc["board"] = FW_BOARD;
    send(doc);
}

void sendPress(const char* id) {
    JsonDocument doc;
    doc["type"] = "press";
    doc["id"] = id;
    send(doc);
}

}  // namespace protocol
