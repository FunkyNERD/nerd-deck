# nerd-deck wire protocol (v7)

Transport: USB CDC serial (ESP32-S3 USB-Serial-JTAG, `303a:1001`). Baud is
ignored on native USB.

Framing: one JSON object per line, terminated by `\n`, max 2048 bytes. Every
message has a `type` field. Lines that don't parse are ignored on both ends.
That matters because the ESP32 ROM prints plain text on reset.

Sources of truth: `host/src/protocol.rs` and `firmware/src/protocol.cpp`.
Bump `PROTO_VERSION` / `kProtoVersion` together on breaking changes.

## Host → device

### `hello`
Sent on connect. The device replies with its own `hello`.
```json
{"type":"hello","proto":7,"host":"workstation"}
```

### `stats`
Sent every `stats.interval_ms` (default 1 s). The device treats the host as gone
if nothing arrives for that interval plus 2 s (at least 3 s; see `stats_ms` in
`display`).
```json
{
  "type": "stats",
  "host": "workstation",
  "uptime": 86400,
  "load": [0.42, 0.51, 0.60],
  "cpu":  {"usage": 12.5, "cores": [10.0, 15.0], "freq_mhz": 3400, "temp": 54.0,
           "profile": "balanced"},
  "mem":  {"used": 8589934592, "total": 34359738368, "swap_used": 0, "swap_total": 8589934592},
  "net":  {"iface": "enp5s0", "rx_bps": 125000, "tx_bps": 4000},
  "disks": [{"mount": "/", "used": 100000000000, "total": 500000000000, "removable": false,
              "read_bps": 1200000, "write_bps": 340000}],
  "gpu":  {"usage": 41, "temp": 62.0, "vram_used": 3328000000, "vram_total": 4294967296},
  "battery": {"percent": 62, "charging": true, "state": "charging", "watts": 12.5,
              "minutes": 95, "wh": 56.4, "full_wh": 90.1, "design_wh": 84.3,
              "volts": 12.9, "temp": 31.5, "cycles": 143, "limit": 85,
              "ac": true, "charger_w": 64}
}
```
`load`, `cpu`, `mem` and `gpu`/`top` (the System tab), `net` (Network) and
`disks` (Storage) are left out while the deck has that tab turned off (see
`hidden` under `display`); the host doesn't read them then. `host` and `uptime`
always come: `stats` is also how the device knows the host is there. Added in
firmware 0.11.0, without a protocol change; the host only leaves them out for a
deck that reports `hidden`.
Sizes are bytes, rates are bytes/s, `usage` is a percentage, and `temp` is °C
(left out when no sensor is found). The device shows at most 32 cores and 4 disks;
`removable` marks a USB drive or SD card (protocol v5; false when left out), and
`read_bps` / `write_bps` are its disk's activity (v6).
`cpu.profile` is the power profile, as power-profiles-daemon names it
("power-saver", "balanced", "performance") or else the ACPI platform profile
("quiet", "low-power", ...); left out when the host reports none. Added in
firmware 0.8.0 without a protocol change; older firmware ignores it.
`gpu` (v6) is the discrete GPU, when the host can read one: `usage` percent,
`temp`, and VRAM in bytes. A powered-down GPU (laptops with switchable
graphics) is just `{"asleep": true}`, since reading it would wake it. Without
`gpu`, the host sends `top` instead: the busiest program, as
`{"name": "firefox", "cpu": 38.0, "mem": 2100000000}`, with `cpu` a share of
the whole CPU. The device shows whichever it gets under memory.
`battery` is left out when the host has no battery; with more than one, `percent`
covers them all. The device only shows the battery reading and the Battery tab
when it's present. The tab shows the rest:
- `state`: `charging`, `discharging`, `full`, or `idle` (plugged in but not
  charging, say held at a charge limit).
- `watts`: into the battery while charging, out of it otherwise. `minutes`: to
  empty, or to full (the charge `limit` if there is one) while charging,
  from a smoothed rate.
- `wh`, `full_wh`, `design_wh`: energy now, when full, and when new. Batteries
  that report charge (µAh) are converted at their nominal voltage.
- `volts`, `temp` (°C), `cycles`, `limit` (charging stops at this percentage),
  `ac` (on mains or USB-C power), `charger_w` (the charger's negotiated power,
  from USB-C PD).

Every field after `charging` is v5, and each is left out when the battery
doesn't report it (`state` and `ac` are always there).

### `buttons`
The layout for the Controls tab: up to 12 buttons, shown 4×3. Sent whenever the
device says `hello` (which it does in reply to the host's), and when the config
is reloaded. The device then rebuilds the grid and frees any previous images.
While the deck has Controls turned off (`hidden`), the host holds them back,
and sends them when it's turned on.
```json
{"type":"buttons","buttons":[
  {"id":"mute","label":"Mute","icon":"\uF026"},
  {"id":"obs","label":"OBS","image":true,"color":16777215},
  {"id":"lock","label":"Lock"}
]}
```
- `id`: 1–23 bytes. `label`: up to 31 bytes.
- `icon` (optional): a built-in LVGL symbol, sent as its UTF-8 character. The
  host maps config names to characters (`host/src/icons.rs`).
- `image` (optional): `true` means `image` messages for this button follow.
  It's never set together with `icon`.
- `color` (optional, v5): the label's colour as a number, 0xRRGGBB. The device's
  own text colour when left out.

### `image`
Pixels for one button, split into chunks that follow the `buttons` message in
order. The image appears once all `total` bytes have arrived. A chunk that arrives
out of order discards that partial image.
```json
{"type":"image","id":"obs","w":108,"h":81,"fmt":"rgb565a8",
 "offset":0,"total":26244,"data":"<base64>"}
```
- `fmt` is always `rgb565a8`: 3 bytes per pixel, row-major. Each pixel is the
  RGB565 colour (big-endian) followed by 8-bit alpha. This is LVGL 8's
  `LV_IMG_CF_TRUE_COLOR_ALPHA` with `LV_COLOR_16_SWAP=1`.
- `total` is always `w * h * 3`. `w` and `h` are at most 160 (64 before v5).
- When the device's `hello` gives a `tile`, the host scales the image to cover
  it and crops the rest, and the device shows it filling the whole button, with
  no label, like a Stream Deck key. Without one (firmware before v5) the host
  scales it to fit 40×40 and the device shows it above the label.
- `data` is the base64 of this chunk: at most 1024 decoded bytes, at most 1536 accepted.

### `ack`
The result of a `press`. `msg` is optional.
```json
{"type":"ack","id":"lock","ok":true}
{"type":"ack","id":"nope","ok":false,"msg":"unknown button"}
```

### `notify`
A notice to show briefly, such as an available update. The device shows `text` in
the toast pill with a cyan dot for 8 s. Older firmware ignores it, like any unknown type.
```json
{"type":"notify","text":"Update available: nerd-deck 0.3.0"}
```

### `notification`
A desktop notification, such as new mail. The device dims the screen and slides
a card down from the top with `app`, `summary` and up to two lines of `body`
(left out when empty or turned off), for 8 s or until tapped. A `critical` one
takes over the whole screen instead, with a pulsing warning icon, and stays
until its Dismiss button is tapped. A newer notification replaces the one
showing. The host sends ASCII only, since the device's fonts have nothing else.
```json
{"type":"notification","app":"Geary","summary":"Re: Saturday's race","body":"Sounds good, see you at 9?"}
{"type":"notification","app":"Thunderbird","summary":"Server certificate expired","critical":true}
```

### `display`
Settings the device keeps in flash, so they apply from power-on, before the host
connects. Added in v6. Each device keeps its own: from firmware 0.10.0 it reports
them in its `hello`, and the host sends only the fields someone changes (in the
settings window), plus `stats_ms` after each `hello` and config reload. Before,
the host sent them all from its config after every `hello`. The device answers
with its own `display`.
```json
{"type":"display","tabs":["controls","system","network","storage","battery"],"hidden":["storage"],"start":"first","brightness":80,"dim_after":300,"off_after":600,"temp_warm":70,"temp_hot":85,"flip":false,"stats_ms":1000}
```
- `tabs`: the tab order, by `system`, `network`, `storage`, `battery` and
  `controls`. The pills in the header and swiping both follow it. Tabs left out
  follow in that order; unknown names are skipped. Battery only shows when
  `stats` has a battery.
- `hidden`: tabs turned off, by the same names. They're left out of the header
  and swipes, and keep their place in `tabs` for when they're turned back on.
  The whole list replaces the stored one; `[]` turns every tab on. One that
  would turn every tab off is ignored. Added in firmware
  0.11.0, without a protocol change; older firmware ignores it, and leaves it
  out of its own `display`.
- `start`: the tab the device opens on, or `first` for the first shown in
  `tabs`. If that tab is off, or it's `battery` and there's no battery, the
  first shown tab instead. It only moves the device to that tab until someone
  picks one.
- `brightness`: backlight percent, 5-100.
- `dim_after`: seconds untouched before the backlight dims to 20%; 0 never
  dims. The tap that wakes it doesn't press anything.
- `off_after`: seconds on the waiting screen (the host away) before the
  backlight turns off; 0 never does. A touch, a notification or the host coming
  back wakes it.
- `temp_warm`, `temp_hot`: °C from which the System tab shows the CPU
  temperature orange, and red. `temp_warm` must be below `temp_hot`.
- `flip`: turn the picture and touch upside down, for the USB cable on the
  other side. Added in firmware 0.7.0, without a protocol change; older
  firmware ignores it.
- `stats_ms`: milliseconds between `stats` messages. The device takes the host
  for gone when nothing arrives for this long plus 2 s (at least 3 s), instead
  of a fixed 3 s. Not kept in flash. Added in firmware 0.9.0, without a
  protocol change; older firmware ignores it.

Fields left out keep the device's stored values.

The host installs firmware around the image's NVS partition, where these are
kept, so they survive an update.

Custom tabs (v7) count as tabs here, by their ids: in `tabs`, `hidden` and
`start`, and in the device's own `display`, which lists those it has a slot
for.

### `tabs`
The host's custom tabs, by id: up to 3 (more are left out), each 1-15 bytes
and not a built-in tab's name. Sent after the device's `hello`, and when the
config is reloaded, to devices that say `proto` 7 or more. The device gives
each a slot it keeps in flash with its place in the order and whether it's
off, so a tab that comes back keeps them; a tab no longer listed loses its
slot and its page. It answers with its `display`, where the tabs now are. A
`tab` for each follows.
```json
{"type":"tabs","ids":["infra","markets"]}
```

### `tab`
One custom tab: its header pill and its grid of panels. Replaces what the
tab showed; its panels show `--` until a `panel` arrives.
```json
{"type":"tab","id":"infra","name":"Infra","icon":"\uF00B","grid":[2,2],
 "panels":[{"style":"status","title":"Git","at":[0,0],"span":[1,1]},
           {"style":"value","title":"Load","at":[1,0],"span":[1,1]},
           {"style":"value","title":"Bitcoin","at":[0,1],"span":[2,1]}]}
```
- `name`: up to 15 bytes. `icon` (optional): a built-in LVGL symbol, as for
  buttons; a list icon when left out.
- `grid`: columns and rows, 1-4 by 1-3, sharing the tab's page evenly.
- `panels`: up to 12. `at` is a panel's top-left cell (column, row), and
  `span` the columns and rows it covers; the device keeps them inside the
  grid. `title` is up to 23 bytes.
- `style`: how the device draws it. `value` is a number or a short text on one
  line, as big as it fits, or longer text wrapped under the title, with a note
  under it; `status` is the same with a coloured dot beside the title; `text`
  is wrapped text.

### `panel`
What one panel shows: `i` is its place in its tab's `panels`. The host sends
it when that changes.
```json
{"type":"panel","tab":"infra","i":0,"text":"200","sub":"84 ms","state":"ok"}
```
- `text`: up to 255 bytes, ASCII. `sub` (optional): the note under it, up to
  31 bytes; `text` panels have none.
- `state` (optional): `ok`, `warn`, `err`, or `stale` (couldn't be refreshed:
  the last text, greyed, with the reason in `sub`). A status panel's dot takes
  its colour (green, orange, red, grey); a value is orange for `warn` and red
  for `err`.

## Device → host

### `hello`
Sent on boot and in reply to the host's `hello`. `board` identifies the
hardware the firmware was built for; the host only installs firmware
on a matching board. Firmware before v3 doesn't send it, and only ever ran on
`wt32-sc01-plus`. `tile` (v5) is a Controls button's face in pixels, inside its
border: the size button images are made to fill.
```json
{"type":"hello","fw":"0.11.0","proto":7,"board":"wt32-sc01-plus","tile":[108,81],
 "display":{"tabs":["system","network","storage","battery","controls"],"hidden":[],"start":"first",
            "brightness":80,"dim_after":0,"off_after":300,"temp_warm":75,"temp_hot":90,"flip":false}}
```
`display` holds the settings the device keeps, with every field of the host's
`display` message but `stats_ms`. Added in firmware 0.10.0, without a protocol
change; the host only lets someone change them on a device that reports them.

### `display`
The device's settings after a host `display` message, the same as `display` in
its `hello`. Added in firmware 0.10.0.
```json
{"type":"display","tabs":["system","network","storage","battery","controls"],"hidden":[],"start":"first","brightness":40,"dim_after":0,"off_after":300,"temp_warm":75,"temp_hot":90,"flip":false}
```

### `image`
An image from the host has arrived (`ok`), or was dropped because a chunk went
missing. Added in v5. The host waits for it before sending the next image, and
sends a dropped one again.
```json
{"type":"image","id":"obs","ok":true}
```

### `press`
A button was tapped. The host looks the `id` up in its config and runs the
command for it. The device never sends a command string.
```json
{"type":"press","id":"lock"}
```

## Changes

- **v2**: added the `icon` and `image` button fields and the `image` message.
- **v3**: the device's `hello` adds `board`, so the host only installs firmware
  on matching hardware. Also adds the `notify` message.
- **v4**: adds the `notification` message.
- **v5**: the device's `hello` adds `tile`, and button images fill it. The
  device answers each `image` with its own, so the host paces them. Buttons
  add `color`. `stats`
  adds battery power, time left, capacity and health for the battery screen.
- **v6**: adds the `display` message: tab order, start tab, brightness and
  power saving.
- **v7**: adds custom tabs: the `tabs`, `tab` and `panel` messages, and their
  ids in `display`.
