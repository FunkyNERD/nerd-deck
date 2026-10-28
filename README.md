# nerd-deck

A USB desk display for a Linux PC: live CPU, memory, network and disk stats on a
3.5" ESP32 touch display, plus a page of touch buttons that run commands on the PC.

| | |
|---|---|
| ![System tab: CPU gauge, RAM and swap, host, uptime and per-core load](docs/images/system.jpg) | ![Network tab: download and upload rates, traffic graph and disk usage](docs/images/network.jpg) |
| **System:** CPU, memory, uptime and per-core load, with the laptop's battery in the corner. | **Network:** live traffic and disks, including drives as you plug them in. |
| ![Controls tab: a grid of touch buttons with icons](docs/images/controls.jpg) | ![Waiting screen: the nerd-deck logo with a "Waiting for host" status](docs/images/waiting.jpg) |
| **Controls:** buttons that run commands on the PC. | **Waiting:** shown until the host app connects. |

```
┌──────────────── PC (Linux) ────────────────┐        USB-C        ┌──────── 3.5" ESP32 display ───────┐
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

Two boards, both 3.5" 480×320 ST7796 panels with FT6336 capacitive touch. On
both, the USB-C port goes to the chip's native USB, so the board enumerates as
`303a:1001` with no USB-UART bridge.

| Board | Build environment | Chip | Display bus | Pin mapping |
|---|---|---|---|---|
| WT32-SC01 Plus | `wt32-sc01-plus` | ESP32-S3-WROVER-N16R2 (16 MB flash, 2 MB PSRAM) | 8-bit parallel | `firmware/src/board/wt32_sc01_plus.hpp` |
| Waveshare ESP32-C5-Touch-LCD-3.5 | `waveshare-c5-touch-35` | ESP32-C5-WROOM-1 (32 MB flash, 8 MB PSRAM) | SPI | `firmware/src/board/waveshare_c5_touch_35.hpp` |

On the Waveshare board the backlight and the LCD and touch resets go through a
CH32V003 I/O expander on the touch controller's I2C bus. Its C5 needs Arduino
core 3.x, which its build environment gets from the pioarduino platform.

Each board is a build environment in `firmware/platformio.ini`, named after the
board, that sets `FW_BOARD`. The deck reports it in `hello`, each release has a
firmware file per board, and the app only installs a deck's own board's
firmware. To add a board, add a pin mapping and a build environment, and the
board and its chip to `BOARDS` in `host/src/firmware.rs`.

## Firmware

Stack: LovyanGFX for the display and touch, LVGL 8.3 for the UI, and ArduinoJson
for the protocol.

```bash
cd firmware && pio run -e wt32-sc01-plus -t upload
cd firmware && pio run -e waveshare-c5-touch-35 -t upload
```

Stop the host service first (`systemctl --user stop nerd-deck`), because it
holds the serial port. If upload doesn't start, hold **BOOT** while pressing
**RST** to enter the bootloader.

You don't normally need to flash by hand: the host app downloads the firmware
from the GitHub release and installs it on the device (see
[Firmware updates](#firmware-updates)). Each build also writes
`.pio/build/<board>/firmware-merged.bin` (bootloader, partitions and app in one
image, via `merge_firmware.py`), which is what each release publishes.

Debug build flags:

```bash
PLATFORMIO_BUILD_FLAGS="-DNERD_PERF -DNERD_START_TAB=1" pio run -e wt32-sc01-plus -t upload
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
| `src/ui/ui.cpp` | System / Network / Storage / Battery / Controls tabs, waiting-for-host screen, toasts |
| `src/config.hpp` | version, limits, timeouts |

## Host app

Build requirements: the Rust toolchain, `libudev-dev` (which `serialport` needs
for USB discovery) and `libfontconfig1-dev` (for the settings window).

```bash
cd host
cargo build --release
cargo run -- --list-ports   # check the device is visible
cargo run -- --dump         # print one stats sample
cargo run                   # connect and stream (RUST_LOG=debug for more)
```

On first start the app writes `~/.config/nerd-deck/config.toml` from
`config.example.toml` (built into the binary). It never overwrites an existing
config. The settings window (below) edits it for you; if you edit it by hand,
choose **Reload config** in the tray (or restart the service).

The app finds the device by VID/PID, sends `hello` and the button layout, then
streams stats. Other ESP32 boards share that VID/PID, so a board that has never
been a deck gets only the `hello`; if it doesn't answer within 10 seconds, the
app leaves it alone until it's unplugged. Decks seen before are tried first. It reconnects automatically if the device is unplugged or reset.
Buttons are defined only in the host config. The device sends just a button `id`,
so it can't trigger anything that isn't in that file.

Each button can show a built-in symbol above its label (`icon = "mute"`), or be
a picture (`image = "~/icons/obs.png"`, PNG, JPEG or BMP). A picture fills the
whole button, like a Stream Deck key: the host scales it to cover the button's
face (the device reports the size) and crops the rest, so square images of 256
px or more look best, and the label runs across the bottom on a dark band. Any
button's label can have its own colour (`color = "#FFA726"`). See
`host/config.example.toml` for the icon names.

### Tabs

The header has a pill per tab: System, Network, Storage, Battery and Controls.
The open tab's pill shows its name; the others show just an icon. Tap one, or
swipe between them. The order, and the tab the deck opens on, are yours to set
in the settings window, while the deck is connected. Each deck keeps its own.

**System** shows the CPU, memory (with swap as a note beside it), uptime and
per-core load. Under memory is the GPU, if the PC has one it can read (NVIDIA
through `nvidia-smi`, or AMD): usage, temperature and VRAM. A laptop's
discrete GPU shows as asleep while it's powered down, and nerd-deck leaves it
that way. Without one, the busiest program takes its place.

**Storage** shows each disk on a card: how full it is, used of total, what's
free, and how fast it's reading and writing. USB drives and SD cards are
marked as such.

**Battery**, on a laptop only, shows the charge and what it's doing (time to full or empty, or held at a charge limit),
power in or out with a chart of the last 10 minutes, the power source (with a
USB-C charger's wattage), and health, capacity, voltage and temperature. It
reads `/sys/class/power_supply`; what shows depends on what the battery
reports. The header's battery reading on the right shows the charge from every
tab.

### Desktop notifications

The device can show your desktop notifications too, such as new mail: it dims
the screen and slides a card down for 8 seconds (tap to dismiss). Critical ones
take over the whole screen until you tap Dismiss. It's off by default, since
anyone near your desk can read the display. Turn it on in the config:

```toml
[notifications]
enabled = true
apps = ["Geary"]   # only these apps; leave empty for all
ignore = []        # never these apps
body = true        # false shows just the app and title
```

The host watches the session bus as a monitor, so it works with any desktop:
it reads the freedesktop `Notify` calls that KDE, XFCE and most apps use, and
GNOME Shell's `org.gtk.Notifications`. It leaves out low-urgency notifications,
and holds back the rest while Do Not Disturb is on (Plasma's or GNOME's).
App names work as the desktop
shows them or as their desktop file (`org.gnome.Geary`); the log names each
app as its notifications arrive (`journalctl --user -u nerd-deck | grep notification`).

### Settings window

`nerd-deck --settings` (or **nerd-deck** in the app menu, or a click on the tray
icon) opens a window for everything in the config:

- **Device:** the connected deck, its port, serial number, firmware and
  protocol, and the settings the deck keeps: brightness, flipping the screen,
  the tab it opens on, dimming when idle, turning off when the PC is away, and
  the temperatures at which the CPU's turns orange and red. Each deck has its
  own, so two decks can be set up differently; they can only be changed while
  that deck is connected.
- **Buttons:** the Controls tab as a grid. Pick a button to change its label,
  its text colour (presets or any hex colour), its icon (the deck's own
  symbols, searchable), an image from a file, and what it does: a ready-made
  action (lock, volume, media keys, a terminal, suspend, ...), opening a URL,
  or running any program. **Run now** tries it on the spot. Add, remove and
  reorder them.
- **Stats:** update interval, network interface, battery, and which disks the
  Storage tab shows, in what order.
- **Deck tabs:** the order of the deck's tabs, with a preview of its header.
- **Notifications:** on or off, which apps (each app that has sent one is
  listed), message text, Do Not Disturb, low-urgency ones, and test
  notifications for the deck.
- **Firmware & updates:** the deck's firmware against the newest on GitHub,
  downloading and installing it, automatic updates, the release check, and
  known decks.
- **About:** versions, links, licences and a block to paste into bug reports.

Changes are written to `config.toml` straight away, keeping its comments and
layout, and the service applies them without restarting. A change that would
make the config invalid isn't written. `--page buttons` (or `stats`,
`notifications`, ...) opens on that section. Opening it while it's already open
brings that window to the front. The window is built with
[Slint](https://slint.dev), used under its royalty-free licence; the About page
shows Slint's credit, as that licence asks.

### Tray icon

`nerd-deck --tray` puts the deck's logo in the system tray: in colour while the
deck is connected, grey while the service waits for it, faded when the service
isn't running. Click it for the settings window; its menu shows the connection
and firmware version, sends test notifications (normal or critical) to the deck,
and opens and reloads the config file. It starts at login (`install.sh` and the
packages set that up). GNOME needs the AppIndicator extension for a tray;
Ubuntu ships it.

The tray is a separate process that talks to the service over D-Bus, so the
deck keeps working without it. The service owns `io.github.FunkyNERD.NerdDeck`
on the session bus:

```bash
busctl --user introspect io.github.FunkyNERD.NerdDeck /io/github/FunkyNERD/NerdDeck
busctl --user call io.github.FunkyNERD.NerdDeck /io/github/FunkyNERD/NerdDeck \
    io.github.FunkyNERD.NerdDeck1 TestNotification b false
```

`Status` returns a dict (connected, port, serial, firmware, board, proto,
flashing, flash_error, version, update, config), `Reload` re-reads the config (a
broken one is rejected and the old one kept), `InstallFirmware s` installs the
firmware image at that path if it's newer than the deck's, and `StatusChanged` fires when the status changes.

### Installing

```bash
host/install.sh
```

This builds a release binary, installs it to `~/.local/bin/nerd-deck`,
installs the udev rule from `host/packaging/` (asks for sudo), enables a systemd user service that runs with your graphical session,
and starts the tray icon at login.
Re-running it upgrades in place.

| Option | Effect |
|---|---|
| `--no-udev` | skip the udev rule; no sudo needed |
| `--no-service` | don't install or enable the service |
| `--no-tray` | don't start the tray icon at login |
| `--uninstall` | remove the binary, service and udev rule (the config is kept) |

Stop the service before flashing firmware, because it holds the serial port:

```bash
systemctl --user stop nerd-deck
```

Logs: `journalctl --user -u nerd-deck -f`

### Firmware updates

The device firmware has releases of its own, apart from the app's (see
[Releases](#releases)), with one file per board,
`nerd-deck-firmware-<version>-<board>-proto<N>.bin`; the app's packages don't
carry it. The settings window's Firmware page checks GitHub for the newest
firmware for the connected deck's board that speaks this host's protocol, and
**Install** downloads it, checks it against the release's SHA-256 digest and the
board and version inside the image, and has the service install it. Firmware
that needs a newer protocol is only mentioned, with a note to update the app
first. Downloads are kept in `~/.cache/nerd-deck/firmware/`.

With automatic updates on, the service downloads new firmware the same way at
startup and then daily. When the device says hello with older firmware, it shows
"Updating firmware to …" on the display, resets the device into the ESP32's
ROM bootloader over the same USB cable, writes the new firmware and resets it.
It takes about 15 seconds; the stats come back on their own.

- **Upgrade only.** A device with the same or newer firmware (say, a dev build
  you flashed with PlatformIO) is left alone, and the settings window only
  offers firmware newer than the device's.
- **Same board only.** The device reports its board in `hello` (protocol v3),
  and the host only installs firmware built for that board.
- **Recoverable.** The ROM bootloader can't be overwritten. If a flash is
  interrupted, the host reflashes the device the next time it connects and gets
  no hello. It only does that for a device it has seen identify as a nerd-deck
  before, by USB serial number (kept in `~/.local/state/nerd-deck/devices`).
- At most two attempts per device each time the host starts, so it can't loop.

Turn automatic updates off with `firmware = false` under `[updates]` in the
config. To install an image by hand (service stopped):

```bash
nerd-deck --flash-firmware firmware/.pio/build/wt32-sc01-plus/firmware-merged.bin
```

### Packages

Each release has a `.deb` and an `.rpm` (x86_64, glibc 2.35 or newer: Debian 12+,
Ubuntu 22.04+, Fedora 36+), and the device firmware beside them. They install the binary to `/usr/bin`, the udev rule,
the systemd user unit, the tray autostart entry, the settings app's menu entry and icon, and the example config in
`/usr/share/doc/nerd-deck/`. The
service isn't enabled for anyone, so each user turns it on, and their config is
created on first start:

```bash
systemctl --user enable --now nerd-deck
```

CI builds both packages on every push that touches the app
(`.github/workflows/host-packages.yml`) and keeps them as a run artifact.
To build the packages locally:

```bash
cargo install --locked cargo-deb cargo-generate-rpm
host/packaging/build-packages.sh   # writes host/dist/
```

## Next steps

- Drag to reorder buttons and disks in the settings window.
- Add multiple button pages.
- Report IP addresses on the Network tab.
- Publish the packages to an APT/RPM repository (Forgejo has package
  registries), so host updates arrive with system updates instead of a notice.
- Restart the user service after a package upgrade, so the new host takes
  effect without `systemctl --user restart nerd-deck`.
- Build aarch64 packages as well, for ARM hosts such as a Raspberry Pi.

## Releases

The app and the firmware are released apart, each with its own version and tag
prefix, so either can ship without the other:

| | Version in | Tag | Release has | Workflow |
|---|---|---|---|---|
| App | `host/Cargo.toml` | `host-v1.2.3` | `.deb`, `.rpm`, SHA256SUMS | `host-packages.yml` |
| Firmware | `FW_VERSION` in `firmware/src/config.hpp` | `firmware-v1.2.3` | the image, SHA256SUMS | `firmware.yml` |

Set the version, commit, then push a matching tag; CI checks they agree and
publishes the release:

```bash
git tag host-v0.7.0 && git push origin main host-v0.7.0
git tag firmware-v0.7.0 && git push origin main firmware-v0.7.0
```

A tag with a suffix (`host-v0.7.0-beta.1`) is published as a prerelease, which
the app's update checks ignore. The app's releases are always GitHub's "Latest";
the firmware's never are. Each release's notes list the commits since that
side's previous release that touch its folder or the docs, grouped by type
(`.github/scripts/release-notes.sh`). Releases up to 0.6.0 were tagged `v*` and
carried both.

The protocol version (`kProtoVersion` in the firmware, `PROTO_VERSION` in the
app) is what ties them together. The app installs the newest firmware that
speaks its protocol, so a release on either side that keeps the protocol works
with the other's current release. When the protocol changes, release the app
that speaks it first; until then the app only mentions the new firmware, with
a note to update the app.

To build the firmware release file locally (needs PlatformIO):

```bash
firmware/package.sh   # writes firmware/dist/
```

## License

MIT. See [LICENSE](LICENSE).
