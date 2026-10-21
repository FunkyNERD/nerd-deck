//! Names for LVGL's built-in symbols (lvgl/src/font/lv_symbol_def.h, v8.3).
//! These glyphs are in every built-in Montserrat font on the device, so a
//! button `icon` is just this character sent as text.

pub const ICONS: &[(&str, char)] = &[
    ("bullet", '\u{2022}'),
    ("audio", '\u{F001}'),
    ("video", '\u{F008}'),
    ("list", '\u{F00B}'),
    ("ok", '\u{F00C}'),
    ("close", '\u{F00D}'),
    ("power", '\u{F011}'),
    ("settings", '\u{F013}'),
    ("home", '\u{F015}'),
    ("download", '\u{F019}'),
    ("drive", '\u{F01C}'),
    ("refresh", '\u{F021}'),
    ("mute", '\u{F026}'),
    ("volume_mid", '\u{F027}'),
    ("volume_max", '\u{F028}'),
    ("image", '\u{F03E}'),
    ("tint", '\u{F043}'),
    ("prev", '\u{F048}'),
    ("play", '\u{F04B}'),
    ("pause", '\u{F04C}'),
    ("stop", '\u{F04D}'),
    ("next", '\u{F051}'),
    ("eject", '\u{F052}'),
    ("left", '\u{F053}'),
    ("right", '\u{F054}'),
    ("plus", '\u{F067}'),
    ("minus", '\u{F068}'),
    ("eye_open", '\u{F06E}'),
    ("eye_close", '\u{F070}'),
    ("warning", '\u{F071}'),
    ("shuffle", '\u{F074}'),
    ("up", '\u{F077}'),
    ("down", '\u{F078}'),
    ("loop", '\u{F079}'),
    ("directory", '\u{F07B}'),
    ("upload", '\u{F093}'),
    ("call", '\u{F095}'),
    ("cut", '\u{F0C4}'),
    ("copy", '\u{F0C5}'),
    ("save", '\u{F0C7}'),
    ("bars", '\u{F0C9}'),
    ("envelope", '\u{F0E0}'),
    ("charge", '\u{F0E7}'),
    ("paste", '\u{F0EA}'),
    ("bell", '\u{F0F3}'),
    ("keyboard", '\u{F11C}'),
    ("gps", '\u{F124}'),
    ("file", '\u{F158}'),
    ("wifi", '\u{F1EB}'),
    ("battery_full", '\u{F240}'),
    ("battery_3", '\u{F241}'),
    ("battery_2", '\u{F242}'),
    ("battery_1", '\u{F243}'),
    ("battery_empty", '\u{F244}'),
    ("usb", '\u{F287}'),
    ("bluetooth", '\u{F293}'),
    ("trash", '\u{F2ED}'),
    ("edit", '\u{F304}'),
    ("backspace", '\u{F55A}'),
    ("sd_card", '\u{F7C2}'),
    ("new_line", '\u{F8A2}'),
];

pub fn lookup(name: &str) -> Option<char> {
    ICONS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

pub fn names() -> impl Iterator<Item = &'static str> {
    ICONS.iter().map(|(n, _)| *n)
}

#[cfg(test)]
mod tests {
    #[test]
    fn lookup_works() {
        assert_eq!(super::lookup("mute"), Some('\u{F026}'));
        assert_eq!(super::lookup("nope"), None);
    }
}
