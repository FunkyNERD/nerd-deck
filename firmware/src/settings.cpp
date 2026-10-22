#include "settings.hpp"

#include <Preferences.h>
#include <string.h>

namespace settings {

namespace {

constexpr const char* kNamespace = "nerd-deck";
constexpr const char* kKey = "display";
// Bump when Display's layout changes; a stored copy of another layout is ignored.
constexpr uint8_t kLayout = 2;

struct Stored {
    uint8_t layout;
    Display d;
};

Display defaults() {
    Display d{};
    for (uint8_t i = 0; i < kTabCount; i++) d.tabs[i] = i;
    d.start = kStartFirst;
    d.brightness = 80;
    d.dimAfterS = 0;
    d.offAfterS = 0;
    return d;
}

// Every tab exactly once.
bool valid(const Display& d) {
    bool seen[kTabCount] = {};
    for (uint8_t t : d.tabs) {
        if (t >= kTabCount || seen[t]) return false;
        seen[t] = true;
    }
    return d.start >= kStartFirst && d.start < kTabCount && d.brightness >= 5 && d.brightness <= 100;
}

}  // namespace

int8_t tabFromName(const char* id) {
    for (uint8_t i = 0; i < kTabCount; i++)
        if (strcmp(id, kTabIds[i]) == 0) return static_cast<int8_t>(i);
    return -1;
}

Display load() {
    Preferences prefs;
    Stored s{};
    if (prefs.begin(kNamespace, true)) {
        const size_t n = prefs.getBytes(kKey, &s, sizeof s);
        prefs.end();
        if (n == sizeof s && s.layout == kLayout && valid(s.d)) return s.d;
    }
    return defaults();
}

void save(const Display& d) {
    Preferences prefs;
    if (!prefs.begin(kNamespace, false)) return;
    Stored now{};
    now.layout = kLayout;
    now.d = d;
    Stored old{};
    // Flash wears; the host sends these on every connect.
    if (prefs.getBytes(kKey, &old, sizeof old) != sizeof old || memcmp(&old, &now, sizeof now) != 0)
        prefs.putBytes(kKey, &now, sizeof now);
    prefs.end();
}

}  // namespace settings
