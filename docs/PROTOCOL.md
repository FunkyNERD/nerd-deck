# nerd-deck wire protocol (v2)

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
{"type":"hello","proto":2,"host":"workstation"}
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
  "disks": [{"mount": "/", "used": 100000000000, "total": 500000000000}],
  "battery": {"percent": 85, "charging": true}
}
```
Sizes are bytes, rates are bytes/s, `usage` is a percentage, and `temp` is °C
(left out when no sensor is found). The device shows at most 32 cores and 4 disks.
`battery` is left out when the host has no battery; with more than one, `percent`
covers them all. The device only shows the battery indicator when it's present.

### `buttons`
The layout for the Controls tab: up to 12 buttons, shown 4×3. Sent on connect and
again whenever the device says `hello`. The device then rebuilds the grid and
frees any previous images.
```json
{"type":"buttons","buttons":[
  {"id":"mute","label":"Mute","icon":"\uF026"},
  {"id":"obs","label":"OBS","image":true},
  {"id":"lock","label":"Lock"}
]}
```
- `id`: 1–23 bytes. `label`: up to 31 bytes.
- `icon` (optional): a built-in LVGL symbol, sent as its UTF-8 character. The
  host maps config names to characters (`host/src/icons.rs`).
- `image` (optional): `true` means `image` messages for this button follow.
  It's never set together with `icon`.

### `image`
Pixels for one button, split into chunks that follow the `buttons` message in
order. The image appears once all `total` bytes have arrived. A chunk that arrives
out of order discards that partial image.
```json
{"type":"image","id":"obs","w":40,"h":40,"fmt":"rgb565a8",
 "offset":0,"total":4800,"data":"<base64>"}
```
- `fmt` is always `rgb565a8`: 3 bytes per pixel, row-major. Each pixel is the
  RGB565 colour (big-endian) followed by 8-bit alpha. This is LVGL 8's
  `LV_IMG_CF_TRUE_COLOR_ALPHA` with `LV_COLOR_16_SWAP=1`.
- `total` is always `w * h * 3`. `w` and `h` are at most 64; the host scales to fit 40×40.
- `data` is the base64 of this chunk: at most 1024 decoded bytes, at most 1536 accepted.

### `ack`
The result of a `press`. `msg` is optional.
```json
{"type":"ack","id":"lock","ok":true}
{"type":"ack","id":"nope","ok":false,"msg":"unknown button"}
```

## Device → host

### `hello`
Sent on boot and in reply to the host's `hello`.
```json
{"type":"hello","fw":"0.2.0","proto":2}
```

### `press`
A button was tapped. The host looks the `id` up in its config and runs the
command for it. The device never sends a command string.
```json
{"type":"press","id":"lock"}
```

## Changes

- **v2**: added the `icon` and `image` button fields and the `image` message.
