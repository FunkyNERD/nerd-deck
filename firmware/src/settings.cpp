#include "settings.hpp"

#include <Preferences.h>
#include <string.h>

namespace settings {

namespace {

constexpr const char* kNamespace = "nerd-deck";
constexpr const char* kKey = "display";
// Bump when Display's layout changes; a stored copy of another layout is ignored.
constexpr uint8_t kLayout = 5;

struct Stored {
    uint8_t layout;
    Display d;
};

// Layout 4, from before tabs could be turned off: taken over as it is, with
// every tab on.
struct DisplayV4 {
    uint8_t tabs[kTabCount];
    int8_t start;
    uint8_t brightness;
    uint32_t dimAfterS;
    uint32_t offAfterS;
    uint8_t tempWarm, tempHot;
    bool flip;
};
struct StoredV4 {
    uint8_t layout;
    DisplayV4 d;
};
constexpr size_t kStoredMax = sizeof(Stored) > sizeof(StoredV4) ? sizeof(Stored) : sizeof(StoredV4);

Display defaults() {
    Display d{};
    for (uint8_t i = 0; i < kTabCount; i++) d.tabs[i] = i;
    d.start = kStartFirst;
    d.brightness = 80;
    d.dimAfterS = 0;
    d.offAfterS = 0;
    d.tempWarm = 70;
    d.tempHot = 85;
    d.flip = false;
    d.hidden = 0;
    return d;
}

// Every tab exactly once.
bool valid(const Display& d) {
    bool seen[kTabCount] = {};
    for (uint8_t t : d.tabs) {
        if (t >= kTabCount || seen[t]) return false;
        seen[t] = true;
    }
    return d.start >= kStartFirst && d.start < kTabCount && d.brightness >= 5 && d.brightness <= 100 &&
           d.tempWarm < d.tempHot && (d.hidden & ~kAllTabs) == 0 && d.hidden != kAllTabs;
}

}  // namespace

int8_t tabFromName(const char* id) {
    for (uint8_t i = 0; i < kTabCount; i++)
        if (strcmp(id, kTabIds[i]) == 0) return static_cast<int8_t>(i);
    return -1;
}

Display load() {
    Preferences prefs;
    uint8_t buf[kStoredMax] = {};
    size_t n = 0;
    if (prefs.begin(kNamespace, true)) {
        n = prefs.getBytes(kKey, buf, sizeof buf);
        prefs.end();
    }
    if (n == sizeof(Stored) && buf[0] == kLayout) {
        Stored s;
        memcpy(&s, buf, sizeof s);
        if (valid(s.d)) return s.d;
    } else if (n == sizeof(StoredV4) && buf[0] == 4) {
        StoredV4 old;
        memcpy(&old, buf, sizeof old);
        Display d{};
        memcpy(d.tabs, old.d.tabs, sizeof d.tabs);
        d.start = old.d.start;
        d.brightness = old.d.brightness;
        d.dimAfterS = old.d.dimAfterS;
        d.offAfterS = old.d.offAfterS;
        d.tempWarm = old.d.tempWarm;
        d.tempHot = old.d.tempHot;
        d.flip = old.d.flip;
        d.hidden = 0;
        if (valid(d)) return d;
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
    // Flash wears, and the host sends a display message on every connect.
    if (prefs.getBytes(kKey, &old, sizeof old) != sizeof old || memcmp(&old, &now, sizeof now) != 0)
        prefs.putBytes(kKey, &now, sizeof now);
    prefs.end();
}

}  // namespace settings
