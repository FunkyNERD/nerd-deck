# nerd-deck wire protocol (v1)

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
{"type":"hello","proto":1,"host":"workstation"}
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
  "disks": [{"mount": "/", "used": 100000000000, "total": 500000000000}]
}
```
Sizes are bytes, rates are bytes/s, `usage` is a percentage, and `temp` is °C
(left out when no sensor is found). The device shows at most 32 cores and 4 disks.

### `buttons`
The layout for the Controls tab: up to 12 buttons, shown 4×3. Sent on connect and
again whenever the device says `hello`.
```json
{"type":"buttons","buttons":[{"id":"lock","label":"Lock"}]}
```

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
{"type":"hello","fw":"0.1.0","proto":1}
```

### `press`
A button was tapped. The host looks the `id` up in its config and runs the
command for it. The device never sends a command string.
```json
{"type":"press","id":"lock"}
```
