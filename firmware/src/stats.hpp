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
    } disks[kMaxDisks];
    uint8_t disk_count;
};

struct ButtonDef {
    char id[24];
    char label[32];
    char icon[8];    // UTF-8 LVGL symbol, or empty
    bool has_image;  // pixels follow in `image` messages
};
