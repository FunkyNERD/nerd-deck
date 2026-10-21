# nerd-deck

A USB desk display for a Linux PC: live CPU, memory, network and disk stats on a
WT32-SC01 Plus, plus a page of touch buttons that run commands on the PC.

```
┌──────────────── PC (Linux) ────────────────┐        USB-C        ┌──── WT32-SC01 Plus (ESP32-S3) ────┐
│ host/  (Rust)                              │  newline-delimited  │ firmware/  (PlatformIO / Arduino) │
│  collector ─ sysinfo → stats ──────────────┼──── JSON ─────────▶ │  protocol → ui (LVGL tabs)        │
│  actions   ◀─ press{id} → sh -c <command>  │ ◀────────────────── │  Controls tab buttons             │
└────────────────────────────────────────────┘                     └───────────────────────────────────┘
```

The message format is in [docs/PROTOCOL.md](docs/PROTOCOL.md).

```bash
git clone git@github.com:FunkyNERD/nerd-deck.git
```

## Hardware

WT32-SC01 Plus: ESP32-S3-WROVER-N16R2 (16 MB flash, 2 MB PSRAM), 3.5" 480×320
ST7796 over an 8-bit parallel bus, and an FT6336U capacitive touch controller. The
USB-C port goes to the S3's native USB, so the board enumerates as `303a:1001`
with no USB-UART bridge. Pin mapping: `firmware/src/board/wt32_sc01_plus.hpp`.

## Firmware

Stack: LovyanGFX for the display and touch, LVGL 8.3 for the UI, and ArduinoJson
for the protocol.

```bash
cd firmware && pio run -t upload
```

Stop the host service first (`systemctl --user stop nerd-deck`), because it
holds the serial port. If upload doesn't start, hold **BOOT** while pressing
**RST** to enter the bootloader.

Debug build flags:

```bash
PLATFORMIO_BUILD_FLAGS="-DNERD_PERF -DNERD_START_TAB=1" pio run -t upload
```

- `NERD_PERF` prints a `perf fps=... busy=...` line every second. The host logs
  it at `RUST_LOG=debug`.
- `NERD_START_TAB=n` opens on tab `n` (0 System, 1 Network, 2 Controls).

`Serial` carries the protocol. The host skips lines that aren't JSON, so a short
plain-text line like the perf output is harmless. For anything chattier, use a
UART on the extension header.

| File | Purpose |
|---|---|
| `src/main.cpp` | setup/loop |
| `src/display.cpp` | panel + touch → LVGL drivers |
| `src/protocol.cpp` | line framing, JSON decode, `hello`/`press` |
| `src/ui/ui.cpp` | System / Network / Controls tabs, status banner, toasts |
| `src/config.hpp` | version, limits, timeouts |

## Host app

Build requirements: the Rust toolchain plus `libudev-dev`, which `serialport`
needs for USB discovery.

```bash
cd host
cargo build --release
cp config.example.toml ~/.config/nerd-deck/config.toml
cargo run -- --list-ports   # check the device is visible
cargo run -- --dump         # print one stats sample
cargo run                   # connect and stream (RUST_LOG=debug for more)
```

The app finds the device by VID/PID, sends `hello` and the button layout, then
streams stats. It reconnects automatically if the device is unplugged or reset.
Buttons are defined only in the host config. The device sends just a button `id`,
so it can't trigger anything that isn't in that file.

Each button can show a built-in symbol (`icon = "mute"`) or a picture
(`image = "~/icons/obs.png"`, PNG, JPEG or BMP) above its label. The host scales
images to fit 40×40 and sends them to the device each time it sends the button
layout. See `host/config.example.toml` for the icon names.

### Installing

```bash
host/install.sh
```

This builds a release binary and installs it to `~/.local/bin/nerd-deck`. It
creates `~/.config/nerd-deck/config.toml` from the example (an existing config is
never overwritten), installs the udev rule from `host/packaging/` (asks for sudo),
and enables a systemd user service that runs with your graphical session.
Re-running it upgrades in place.

| Option | Effect |
|---|---|
| `--no-udev` | skip the udev rule; no sudo needed |
| `--no-service` | don't install or enable the service |
| `--uninstall` | remove the binary, service and udev rule (the config is kept) |

Stop the service before flashing firmware, because it holds the serial port:

```bash
systemctl --user stop nerd-deck
```

Logs: `journalctl --user -u nerd-deck -f`

## Next steps

- Add GPU stats (NVML for NVIDIA, `/sys/class/drm` for AMD).
- Add a host-driven `brightness` message, and a screen-off timer once the host
  has been gone for a while.
- Add multiple button pages.
- Report IP addresses on the Network tab.

## License

MIT. See [LICENSE](LICENSE).
