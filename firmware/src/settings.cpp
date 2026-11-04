#include "settings.hpp"

#include <Preferences.h>
#include <string.h>

namespace settings {

namespace {

constexpr const char* kNamespace = "nerd-deck";
constexpr const char* kKey = "display";
// Bump when Display's layout changes; a stored copy of another layout is
// ignored, unless load() takes it over.
constexpr uint8_t kLayout = 6;

struct Stored {
    uint8_t layout;
    Display d;
};

// Layouts 4 (before tabs could be turned off) and 5 (before custom tabs):
// taken over as they are, the custom slots after the built-in tabs.
struct DisplayV4 {
    uint8_t tabs[kBuiltinTabs];
    int8_t start;
    uint8_t brightness;
    uint32_t dimAfterS;
    uint32_t offAfterS;
    uint8_t tempWarm, tempHot;
    bool flip;
};
// Spelt out rather than derived from DisplayV4: `hidden` sat in its padding.
struct DisplayV5 {
    uint8_t tabs[kBuiltinTabs];
    int8_t start;
    uint8_t brightness;
    uint32_t dimAfterS;
    uint32_t offAfterS;
    uint8_t tempWarm, tempHot;
    bool flip;
    uint8_t hidden;
};
template <typename D>
struct StoredOld {
    uint8_t layout;
    D d;
};

constexpr size_t kStoredMax = sizeof(Stored);
static_assert(sizeof(StoredOld<DisplayV4>) <= kStoredMax && sizeof(StoredOld<DisplayV5>) <= kStoredMax,
              "the buffer holds every layout");
static_assert(sizeof(StoredOld<DisplayV5>) == 24, "layout 5 as firmware 0.11.0 stored it");

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

template <typename D>
Display fromOld(const D& old, uint8_t hidden) {
    Display d = defaults();
    memcpy(d.tabs, old.tabs, sizeof old.tabs);
    d.start = old.start;
    d.brightness = old.brightness;
    d.dimAfterS = old.dimAfterS;
    d.offAfterS = old.offAfterS;
    d.tempWarm = old.tempWarm;
    d.tempHot = old.tempHot;
    d.flip = old.flip;
    d.hidden = hidden;
    return d;
}

// Every tab exactly once, and custom ids terminated.
bool valid(const Display& d) {
    bool seen[kTabCount] = {};
    for (uint8_t t : d.tabs) {
        if (t >= kTabCount || seen[t]) return false;
        seen[t] = true;
    }
    for (const auto& id : d.custom)
        if (!memchr(id, '\0', sizeof id)) return false;
    return d.start >= kStartFirst && d.start < kTabCount && d.brightness >= 5 && d.brightness <= 100 &&
           d.tempWarm < d.tempHot;
}

}  // namespace

int8_t tabFromName(const Display& d, const char* id) {
    if (!id || !*id) return -1;
    for (uint8_t i = 0; i < kBuiltinTabs; i++)
        if (strcmp(id, kTabIds[i]) == 0) return static_cast<int8_t>(i);
    for (uint8_t i = 0; i < kCustomTabs; i++)
        if (strcmp(id, d.custom[i]) == 0) return static_cast<int8_t>(kBuiltinTabs + i);
    return -1;
}

const char* tabName(const Display& d, uint8_t t) {
    if (t < kBuiltinTabs) return kTabIds[t];
    if (t >= kTabCount) return nullptr;
    const char* id = d.custom[t - kBuiltinTabs];
    return *id ? id : nullptr;
}

bool assignCustom(Display& d, const char* const* ids, uint8_t count) {
    if (count > kCustomTabs) count = kCustomTabs;
    const Display before = d;
    // Free the slots whose tab is gone, forgetting its settings.
    for (uint8_t s = 0; s < kCustomTabs; s++) {
        bool kept = false;
        for (uint8_t i = 0; i < count; i++) kept |= strcmp(d.custom[s], ids[i]) == 0;
        if (kept || !d.custom[s][0]) continue;
        const uint8_t t = kBuiltinTabs + s;
        d.custom[s][0] = '\0';
        d.hidden &= ~(1u << t);
        if (d.start == t) d.start = kStartFirst;
    }
    // New ones take the free slots.
    for (uint8_t i = 0; i < count; i++) {
        if (tabFromName(d, ids[i]) >= 0) continue;
        for (uint8_t s = 0; s < kCustomTabs; s++) {
            if (d.custom[s][0]) continue;
            strlcpy(d.custom[s], ids[i], kCustomIdLen);
            break;
        }
    }
    return memcmp(&before, &d, sizeof d) != 0;
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
    } else if (n == sizeof(StoredOld<DisplayV5>) && buf[0] == 5) {
        StoredOld<DisplayV5> old;
        memcpy(&old, buf, sizeof old);
        const Display d = fromOld(old.d, old.d.hidden);
        if (valid(d)) return d;
    } else if (n == sizeof(StoredOld<DisplayV4>) && buf[0] == 4) {
        StoredOld<DisplayV4> old;
        memcpy(&old, buf, sizeof old);
        const Display d = fromOld(old.d, 0);
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
