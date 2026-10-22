//! Device firmware images, downloaded from GitHub releases into the cache
//! (see updates.rs), and installing them through the ESP32-S3's ROM
//! bootloader over the same USB serial port. The ROM can't be overwritten, so
//! an interrupted flash is always recoverable.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::Flasher;
use espflash::image_format::Segment;
use espflash::target::{Chip, ProgressCallbacks};
use serialport::UsbPortInfo;

use crate::config::Config;
use crate::device::Port;
use crate::protocol::PROTO_VERSION;
use crate::updates;

/// The board this host installs firmware for (FW_BOARD in firmware/src/config.hpp).
pub const BOARD: &str = "wt32-sc01-plus";
/// Firmware before protocol v3 doesn't report a board, and only ever ran on this one.
pub const LEGACY_BOARD: &str = BOARD;
const MAX_FLASH_ATTEMPTS: u32 = 2;
const MARKER: &[u8] = b"nerd-deck-firmware:";

/// A merged image (bootloader, partitions, app), written at flash offset 0.
pub struct Image {
    pub version: String,
    data: Vec<u8>,
}

impl Image {
    pub fn load(path: &Path) -> Result<Self> {
        let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(data).with_context(|| path.display().to_string())
    }

    /// Checks the `nerd-deck-firmware:<board>:<version>;` marker the firmware
    /// carries (firmware/src/protocol.cpp), so the board and version always
    /// describe the image itself.
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        let (board, version) = identify(&data).context("not nerd-deck firmware")?;
        ensure!(board == BOARD, "firmware for {board}, not {BOARD}");
        Ok(Self { version, data })
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// Finds `nerd-deck-firmware:<board>:<version>;` and returns (board, version).
fn identify(image: &[u8]) -> Option<(String, String)> {
    let start = image.windows(MARKER.len()).position(|w| w == MARKER)? + MARKER.len();
    let len = image[start..].iter().take(64).position(|&b| b == b';')?;
    let id = std::str::from_utf8(&image[start..start + len]).ok()?;
    let (board, version) = id.split_once(':')?;
    Some((board.to_owned(), version.to_owned()))
}

/// Where downloaded firmware is kept.
pub fn cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("nerd-deck").join("firmware"))
}

/// A release asset's name, which the cached copy keeps:
/// nerd-deck-firmware-<version>-<board>-proto<N>.bin (the release workflow
/// names it, firmware/package.sh).
#[cfg(test)]
pub fn file_name(version: &str, proto: u32) -> String {
    format!("nerd-deck-firmware-{version}-{BOARD}-proto{proto}.bin")
}

/// The version and protocol in a name from `file_name`, if it's for our board.
pub fn parse_name(name: &str) -> Option<(String, u32)> {
    let rest = name.strip_prefix("nerd-deck-firmware-")?.strip_suffix(".bin")?;
    let (rest, proto) = rest.rsplit_once("-proto")?;
    let version = rest.strip_suffix(BOARD)?.strip_suffix('-')?;
    Some((version.to_owned(), proto.parse().ok()?))
}

/// A downloaded image this host can talk to.
#[derive(Debug, Clone, PartialEq)]
pub struct Cached {
    pub version: String,
    pub path: PathBuf,
}

/// The newest downloaded firmware that speaks this host's protocol.
pub fn newest_cached() -> Option<Cached> {
    let dir = cache_dir()?;
    fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let (version, proto) = parse_name(&name)?;
            (proto == PROTO_VERSION).then(|| Cached { version, path: e.path() })
        })
        .reduce(|a, b| if updates::is_newer(&b.version, &a.version) { b } else { a })
}

/// Decides when to install downloaded firmware by itself: only on the board
/// it's built for, only when it's newer, and at most MAX_FLASH_ATTEMPTS times
/// per device per run, so a build that reports the wrong version can't flash
/// in a loop.
pub struct Flashing {
    enabled: bool,
    /// Looks for the newest downloaded firmware; the cache changes as the
    /// update check and the settings window download more.
    newest: fn() -> Option<Cached>,
    known: KnownDevices,
    attempts: HashMap<String, u32>,
}

impl Flashing {
    pub fn new(cfg: &Config) -> Self {
        Self::with(newest_cached, cfg.updates.firmware, KnownDevices::load())
    }

    fn with(newest: fn() -> Option<Cached>, enabled: bool, known: KnownDevices) -> Self {
        if !enabled {
            log::info!("device firmware updates are off");
        }
        Self {
            enabled,
            newest,
            known,
            attempts: HashMap::new(),
        }
    }

    /// Re-read the known devices, which the settings window can change.
    pub fn reload_known(&mut self) {
        self.known = KnownDevices::load();
    }

    fn firmware(&self) -> Option<Cached> {
        self.enabled.then(self.newest).flatten()
    }

    /// Called on the device's hello. Returns the firmware to install, if any.
    pub fn should_update(&mut self, port: &Port, fw: &str, board: Option<&str>) -> Option<Cached> {
        let board = board.unwrap_or(LEGACY_BOARD);
        if board != BOARD {
            log::info!(
                "device board {board:?} isn't {:?}; leaving its firmware alone",
                BOARD
            );
            return None;
        }
        if let Some(serial) = port.serial() {
            self.known.insert(serial);
        }
        let firmware = self.firmware()?;
        if !updates::is_newer(&firmware.version, fw) {
            return None;
        }
        if !self.take_attempt(port) {
            log::warn!(
                "device still has firmware {fw}; not trying to install {} again",
                firmware.version
            );
            return None;
        }
        log::info!("updating device firmware {fw} -> {}", firmware.version);
        Some(firmware)
    }

    /// Called when the device hasn't said hello. Only a device that has
    /// identified as our board before (by USB serial number) is reflashed.
    pub fn should_recover(&mut self, port: &Port) -> Option<Cached> {
        let firmware = self.firmware()?;
        if !port.serial().is_some_and(|s| self.known.contains(s)) || !self.take_attempt(port) {
            return None;
        }
        log::warn!(
            "known device isn't answering; reinstalling firmware {}",
            firmware.version
        );
        Some(firmware)
    }

    fn take_attempt(&mut self, port: &Port) -> bool {
        let key = port.serial().unwrap_or(&port.name).to_owned();
        let attempts = self.attempts.entry(key).or_default();
        *attempts += 1;
        *attempts <= MAX_FLASH_ATTEMPTS
    }
}

/// Resets the device into its ROM bootloader, writes the image and resets it
/// back into the new firmware. Blocks for roughly 10-20 s.
pub fn flash(fw: &Image, port: &str, info: UsbPortInfo) -> Result<()> {
    let serial = serialport::new(port, 115_200)
        .open_native()
        .with_context(|| format!("opening {port}"))?;
    let conn = Connection::new(
        serial,
        info,
        ResetAfterOperation::HardReset,
        ResetBeforeOperation::DefaultReset,
        115_200,
    );
    let mut flasher = Flasher::connect(conn, true, true, false, Some(Chip::Esp32s3), None)
        .context("entering the ROM bootloader")?;
    flasher
        .write_bins_to_flash(
            &[Segment {
                addr: 0,
                data: Cow::Borrowed(&fw.data),
            }],
            &mut Progress::default(),
        )
        .context("writing flash")?;
    flasher
        .connection()
        .reset_after(true, Chip::Esp32s3)
        .context("resetting the device")?;
    Ok(())
}

/// Logs progress every 25%.
#[derive(Default)]
struct Progress {
    total: usize,
    logged: usize,
}

impl ProgressCallbacks for Progress {
    fn init(&mut self, _addr: u32, total: usize) {
        self.total = total.max(1);
        self.logged = 0;
    }

    fn update(&mut self, current: usize) {
        let pct = current * 100 / self.total;
        if pct >= self.logged + 25 {
            self.logged = pct - pct % 25;
            log::info!("flashing: {}%", self.logged);
        }
    }

    fn verifying(&mut self) {
        log::info!("verifying");
    }

    fn finish(&mut self, _skipped: bool) {}
}

/// USB serial numbers (the chip's MAC) of devices that have identified as a
/// nerd-deck board. A known device that stops answering, say after an
/// interrupted flash, may be reflashed; an unknown one never is.
pub struct KnownDevices {
    path: Option<PathBuf>,
    serials: BTreeSet<String>,
}

impl KnownDevices {
    pub fn load() -> Self {
        let path = dirs::state_dir().map(|d| d.join("nerd-deck").join("devices"));
        let serials = path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|t| {
                t.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        Self { path, serials }
    }

    pub fn serials(&self) -> impl Iterator<Item = &str> {
        self.serials.iter().map(String::as_str)
    }

    /// Forget a device, so it's never reflashed unasked until it says hello again.
    pub fn remove(&mut self, serial: &str) {
        if self.serials.remove(serial) {
            self.save();
        }
    }

    pub fn contains(&self, serial: &str) -> bool {
        self.serials.contains(serial)
    }

    pub fn insert(&mut self, serial: &str) {
        if self.serials.insert(serial.to_owned()) {
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let text: String = self.serials.iter().map(|s| format!("{s}\n")).collect();
        let saved = path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(path, text));
        if let Err(e) = saved {
            log::warn!("can't save {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: &str = "48:27:E2:91:0B:2C";

    fn v040() -> Option<Cached> {
        Some(Cached {
            version: "0.4.0".into(),
            path: PathBuf::new(),
        })
    }

    fn flashing(known: &[&str]) -> Flashing {
        let known = KnownDevices {
            path: None,
            serials: known.iter().map(|s| s.to_string()).collect(),
        };
        Flashing::with(v040, true, known)
    }

    fn port(serial: Option<&str>) -> Port {
        let usb = serial.map(|s| UsbPortInfo {
            vid: 0x303a,
            pid: 0x1001,
            serial_number: Some(s.into()),
            manufacturer: None,
            product: None,
        });
        Port {
            name: "/dev/ttyACM0".into(),
            usb,
        }
    }

    fn version(c: Option<Cached>) -> Option<String> {
        c.map(|c| c.version)
    }

    #[test]
    fn upgrades_only() {
        let mut f = flashing(&[]);
        let p = port(Some(MAC));
        assert_eq!(version(f.should_update(&p, "0.3.0", Some(BOARD))).as_deref(), Some("0.4.0"));
        assert_eq!(f.should_update(&p, "0.4.0", Some(BOARD)), None);
        assert_eq!(f.should_update(&p, "0.5.0", Some(BOARD)), None);
        assert_eq!(f.should_update(&p, "0.4.0-dev", Some(BOARD)), None);
    }

    #[test]
    fn same_board_only() {
        let mut f = flashing(&[]);
        let p = port(Some(MAC));
        assert_eq!(f.should_update(&p, "0.1.0", Some("some-other-board")), None);
        assert!(!f.known.contains(MAC));
        // Firmware before protocol v3 sends no board; it only ran on ours.
        assert!(f.should_update(&p, "0.3.0", None).is_some());
        assert!(f.known.contains(MAC));
    }

    #[test]
    fn gives_up_after_max_attempts() {
        let mut f = flashing(&[]);
        let p = port(Some(MAC));
        for _ in 0..MAX_FLASH_ATTEMPTS {
            assert!(f.should_update(&p, "0.3.0", Some(BOARD)).is_some());
        }
        assert_eq!(f.should_update(&p, "0.3.0", Some(BOARD)), None);
        assert_eq!(f.should_recover(&p), None);
    }

    #[test]
    fn recovers_known_devices_only() {
        assert!(flashing(&[MAC]).should_recover(&port(Some(MAC))).is_some());
        assert!(flashing(&[MAC]).should_recover(&port(Some("AA:BB:CC:DD:EE:FF"))).is_none());
        assert!(flashing(&[MAC]).should_recover(&port(None)).is_none());
    }

    #[test]
    fn does_nothing_when_off_or_not_downloaded() {
        let known = || KnownDevices {
            path: None,
            serials: [MAC.to_string()].into(),
        };
        let p = port(Some(MAC));
        let mut off = Flashing::with(v040, false, known());
        assert_eq!(off.should_update(&p, "0.3.0", Some(BOARD)), None);
        assert_eq!(off.should_recover(&p), None);
        let mut none = Flashing::with(|| None, true, known());
        assert_eq!(none.should_update(&p, "0.3.0", Some(BOARD)), None);
        assert_eq!(none.should_recover(&p), None);
    }

    #[test]
    fn names_round_trip() {
        let name = file_name("0.6.0", 6);
        assert_eq!(name, "nerd-deck-firmware-0.6.0-wt32-sc01-plus-proto6.bin");
        assert_eq!(parse_name(&name), Some(("0.6.0".into(), 6)));
        assert_eq!(parse_name("nerd-deck-firmware-0.6.0-other-board-proto6.bin"), None);
        assert_eq!(parse_name("nerd-deck_0.6.0-1_amd64.deb"), None);
    }

    #[test]
    fn checks_the_marker() {
        let image = |id: &str| format!("\0\0nerd-deck-firmware:{id};\0").into_bytes();
        assert_eq!(Image::parse(image("wt32-sc01-plus:0.6.0")).unwrap().version, "0.6.0");
        assert!(Image::parse(image("other-board:0.6.0")).is_err());
        assert!(Image::parse(b"not firmware".to_vec()).is_err());
    }
}
