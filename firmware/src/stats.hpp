#pragma once

#include "config.hpp"

// Decoded `stats` message from the host. See docs/PROTOCOL.md.
struct HostStats {
    char host[32];
    uint64_t uptime;
    float load[3];

    struct {
        float usage;
        float cores[kMaxCores];
        uint8_t core_count;
        uint32_t freq_mhz;
        bool has_temp;
        float temp;
        char profile[24];  // power profile, e.g. "balanced"; empty if unknown
    } cpu;

    struct {
        uint64_t used, total;
        uint64_t swap_used, swap_total;
    } mem;

    struct {
        char iface[16];
        uint64_t rx_bps, tx_bps;
    } net;

    struct Disk {
        char mount[24];
        uint64_t used, total;
        bool removable;  // a USB drive or SD card
        uint64_t read_bps, write_bps;
    } disks[kMaxDisks];
    uint8_t disk_count;

    // The discrete GPU (v6); otherwise `top`.
    struct {
        bool present;
        bool asleep;  // powered down; the rest is unknown
        uint8_t usage;
        float temp;  // NAN when unknown
        uint64_t vram_used, vram_total;
    } gpu;

    // The busiest program, on hosts without a GPU to show (v6).
    struct {
        bool present;
        char name[24];
        float cpu;  // percent of the whole CPU
        uint64_t mem;
    } top;

    enum class BatteryState : uint8_t { Discharging, Charging, Full, Idle };

    struct {
        bool present;  // false on hosts without a battery
        uint8_t percent;
        bool charging;
        // Protocol v5. Unknown values: NAN for floats, -1 minutes, 0 otherwise.
        BatteryState state;
        float watts;      // into the battery while charging, out of it otherwise
        int32_t minutes;  // to empty, or to full / the charge limit
        float wh, full_wh, design_wh;
        float volts;
        float temp;
        uint16_t cycles;
        uint8_t limit;  // charging stops here, percent
        bool ac;
        float charger_w;
    } battery;
};

struct ButtonDef {
    char id[24];
    char label[32];
    char icon[8];    // UTF-8 LVGL symbol, or empty
    bool has_image;  // pixels follow in `image` messages
    bool has_color;
    uint32_t color;  // label colour, 0xRRGGBB, when has_color
};
