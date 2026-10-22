# nerd-deck wire protocol (v5)

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
{"type":"hello","proto":5,"host":"workstation"}
```

### `stats`
Sent every `stats.interval_ms` (default 1 s). The device treats the host as gone
if nothing arrives for 3 s.
```json
{
  "type": "stats",
  "host": "workstation",
  "uptime": 86400,
  "load": [0.42, 0.51, 0.60],
  "cpu":  {"usage": 12.5, "cores": [10.0, 15.0], "freq_mhz": 3400, "temp": 54.0},
  "mem":  {"used": 8589934592, "total": 34359738368, "swap_used": 0, "swap_total": 8589934592},
  "net":  {"iface": "enp5s0", "rx_bps": 125000, "tx_bps": 4000},
  "disks": [{"mount": "/", "used": 100000000000, "total": 500000000000, "removable": false}],
  "battery": {"percent": 62, "charging": true, "state": "charging", "watts": 12.5,
              "minutes": 95, "wh": 56.4, "full_wh": 90.1, "design_wh": 84.3,
              "volts": 12.9, "temp": 31.5, "cycles": 143, "limit": 85,
              "ac": true, "charger_w": 64}
}
```
Sizes are bytes, rates are bytes/s, `usage` is a percentage, and `temp` is °C
(left out when no sensor is found). The device shows at most 32 cores and 4 disks;
`removable` marks a USB drive or SD card (protocol v5; false when left out).
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

## Device → host

### `hello`
Sent on boot and in reply to the host's `hello`. `board` identifies the
hardware the firmware was built for; the host only installs its bundled firmware
on a matching board. Firmware before v3 doesn't send it, and only ever ran on
`wt32-sc01-plus`. `tile` (v5) is a Controls button's face in pixels, inside its
border: the size button images are made to fill.
```json
{"type":"hello","fw":"0.5.0","proto":5,"board":"wt32-sc01-plus","tile":[108,81]}
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
- **v3**: the device's `hello` adds `board`, so the host only installs its bundled firmware
  on matching hardware. Also adds the `notify` message.
- **v4**: adds the `notification` message.
- **v5**: the device's `hello` adds `tile`, and button images fill it. The
  device answers each `image` with its own, so the host paces them. Buttons
  add `color`. `stats`
  adds battery power, time left, capacity and health for the battery screen.
