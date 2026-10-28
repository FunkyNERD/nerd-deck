//! Device firmware images, downloaded from GitHub releases into the cache
//! (see updates.rs), and installing them through the ESP32-S3's ROM
//! bootloader over the same USB serial port. The ROM can't be overwritten, so
//! an interrupted flash is always recoverable.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::Flasher;
use espflash::image_format::Segment;
use espflash::target::{Chip, ProgressCallbacks};
use serialport::{SerialPort, UsbPortInfo};

use crate::config::Config;
use crate::device::Port;
use crate::protocol::PROTO_VERSION;
use crate::updates;

/// The boards this host installs firmware for, and their chips: one per build
/// environment in firmware/platformio.ini, whose FW_BOARD the deck reports.
pub const BOARDS: &[(&str, Chip)] = &[
    ("wt32-sc01-plus", Chip::Esp32s3),
    ("waveshare-c5-touch-35", Chip::Esp32c5),
];
/// Firmware before protocol v3 doesn't report a board, and only ever ran on this one.
pub const LEGACY_BOARD: &str = "wt32-sc01-plus";

/// The chip on a board this host knows, or None.
pub fn chip(board: &str) -> Option<Chip> {
    BOARDS.iter().find(|(b, _)| *b == board).map(|&(_, chip)| chip)
}
const MAX_FLASH_ATTEMPTS: u32 = 2;
const MARKER: &[u8] = b"nerd-deck-firmware:";

/// A merged image (bootloader, partitions, app), written at flash offset 0.
pub struct Image {
    pub board: String,
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
        ensure!(chip(&board).is_some(), "firmware for {board}, which this app doesn't know");
        Ok(Self { board, version, data })
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
pub fn file_name(version: &str, board: &str, proto: u32) -> String {
    format!("nerd-deck-firmware-{version}-{board}-proto{proto}.bin")
}

/// A firmware file's name, read back.
#[derive(Debug, Clone, PartialEq)]
pub struct Name {
    pub version: String,
    pub board: &'static str,
    pub proto: u32,
}

/// Reads a name from `file_name`, if it's for a board this host knows.
pub fn parse_name(name: &str) -> Option<Name> {
    let rest = name.strip_prefix("nerd-deck-firmware-")?.strip_suffix(".bin")?;
    let (rest, proto) = rest.rsplit_once("-proto")?;
    // Versions and boards both have dashes, so match the board by name.
    BOARDS.iter().find_map(|&(board, _)| {
        let version = rest.strip_suffix(board)?.strip_suffix('-')?;
        Some(Name {
            version: version.to_owned(),
            board,
            proto: proto.parse().ok()?,
        })
    })
}

/// A downloaded image this host can talk to.
#[derive(Debug, Clone, PartialEq)]
pub struct Cached {
    pub version: String,
    pub path: PathBuf,
}

/// The newest downloaded firmware for a board that speaks this host's protocol.
pub fn newest_cached(board: &str) -> Option<Cached> {
    let dir = cache_dir()?;
    fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = parse_name(&e.file_name().into_string().ok()?)?;
            (name.board == board && name.proto == PROTO_VERSION).then(|| Cached {
                version: name.version,
                path: e.path(),
            })
        })
        .reduce(|a, b| if updates::is_newer(&b.version, &a.version) { b } else { a })
}

/// Decides when to install downloaded firmware by itself: only on the board
/// it's built for, only when it's newer, and at most MAX_FLASH_ATTEMPTS times
/// per device per run, so a build that reports the wrong version can't flash
/// in a loop.
pub struct Flashing {
    enabled: bool,
    /// Looks for the newest downloaded firmware for a board; the cache changes
    /// as the update check and the settings window download more.
    newest: fn(&str) -> Option<Cached>,
    known: KnownDevices,
    attempts: HashMap<String, u32>,
}

impl Flashing {
    pub fn new(cfg: &Config) -> Self {
        Self::with(newest_cached, cfg.updates.firmware, KnownDevices::load())
    }

    fn with(newest: fn(&str) -> Option<Cached>, enabled: bool, known: KnownDevices) -> Self {
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

    /// Whether the device on `port` has said hello as a deck before.
    pub fn is_known(&self, port: &Port) -> bool {
        port.serial().is_some_and(|s| self.known.board(s).is_some())
    }

    fn firmware(&self, board: &str) -> Option<Cached> {
        self.enabled.then(|| (self.newest)(board)).flatten()
    }

    /// Called on the device's hello. Returns the firmware to install, if any.
    pub fn should_update(&mut self, port: &Port, fw: &str, board: Option<&str>) -> Option<Cached> {
        let board = board.unwrap_or(LEGACY_BOARD);
        if chip(board).is_none() {
            log::info!("device board {board:?} isn't one this app knows; leaving its firmware alone");
            return None;
        }
        if let Some(serial) = port.serial() {
            self.known.insert(serial, board);
        }
        let firmware = self.firmware(board)?;
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
        let board = self.known.board(port.serial()?)?.to_owned();
        let firmware = self.firmware(&board)?;
        if !self.take_attempt(port) {
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
    let chip = chip(&fw.board).context("unknown board")?;
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
    let mut flasher = Flasher::connect(conn, true, true, false, Some(chip), None)
        .context("entering the ROM bootloader")?;
    let segments: Vec<Segment> = keeping_nvs(&fw.data)
        .into_iter()
        .map(|(addr, data)| Segment { addr, data: Cow::Borrowed(data) })
        .collect();
    flasher
        .write_bins_to_flash(&segments, &mut Progress::default())
        .context("writing flash")?;
    flasher
        .connection()
        .reset_after(true, chip)
        .context("resetting the device")?;
    if chip == Chip::Esp32c5 {
        // espflash's reset leaves an ESP32-C5 on USB-Serial-JTAG halted. A
        // reset pulse on the port opened afresh, as esptool does, boots it.
        drop(flasher);
        reset_again(port).context("resetting the device")?;
    }
    Ok(())
}

/// Where the image's partition table is, and how its entries start.
const PARTITION_TABLE: usize = 0x8000;
const PARTITION_MAGIC: [u8; 2] = [0xAA, 0x50];

/// The parts of a merged image (written at 0) to write, by flash address:
/// all of it but its NVS partitions. That's where the deck keeps its
/// settings, and the image only has blank flash for them.
fn keeping_nvs(image: &[u8]) -> Vec<(u32, &[u8])> {
    let mut skip: Vec<(usize, usize)> = image
        .get(PARTITION_TABLE..)
        .unwrap_or_default()
        .chunks_exact(32)
        .take_while(|e| e[..2] == PARTITION_MAGIC)
        // Type data, subtype nvs.
        .filter(|e| e[2] == 1 && e[3] == 2)
        .map(|e| {
            let offset = u32::from_le_bytes(e[4..8].try_into().unwrap()) as usize;
            let size = u32::from_le_bytes(e[8..12].try_into().unwrap()) as usize;
            (offset, offset + size)
        })
        .collect();
    skip.sort();
    let mut parts = Vec::new();
    let mut at = 0;
    for (start, end) in skip {
        let (start, end) = (start.min(image.len()), end.min(image.len()));
        if start > at {
            parts.push((at as u32, &image[at..start]));
        }
        at = at.max(end);
    }
    if at < image.len() {
        parts.push((at as u32, &image[at..]));
    }
    parts
}

fn reset_again(port: &str) -> Result<()> {
    // The port goes away while the chip resets.
    let mut tries = 0;
    let mut serial = loop {
        thread::sleep(Duration::from_millis(250));
        match serialport::new(port, 115_200).open_native() {
            Ok(serial) => break serial,
            Err(e) if tries >= 20 => return Err(e).with_context(|| format!("opening {port}")),
            Err(_) => tries += 1,
        }
    };
    serial.write_data_terminal_ready(false)?;
    serial.write_request_to_send(true)?;
    thread::sleep(Duration::from_millis(200));
    serial.write_request_to_send(false)?;
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
/// nerd-deck board, with the board. A known device that stops answering, say
/// after an interrupted flash, may be reflashed with its board's firmware; an
/// unknown one never is.
pub struct KnownDevices {
    path: Option<PathBuf>,
    boards: BTreeMap<String, String>,
}

impl KnownDevices {
    /// One device per line: its serial number, then its board. Lines from
    /// before the board was kept have only the serial, from the one board
    /// there was.
    pub fn load() -> Self {
        let path = dirs::state_dir().map(|d| d.join("nerd-deck").join("devices"));
        let boards = path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|t| {
                t.lines()
                    .filter_map(|l| {
                        let mut fields = l.split_whitespace();
                        let serial = fields.next()?.to_owned();
                        let board = fields.next().unwrap_or(LEGACY_BOARD).to_owned();
                        Some((serial, board))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { path, boards }
    }

    pub fn serials(&self) -> impl Iterator<Item = &str> {
        self.boards.keys().map(String::as_str)
    }

    /// Forget a device, so it's never reflashed unasked until it says hello again.
    pub fn remove(&mut self, serial: &str) {
        if self.boards.remove(serial).is_some() {
            self.save();
        }
    }

    /// The board a known device identified as.
    pub fn board(&self, serial: &str) -> Option<&str> {
        self.boards.get(serial).map(String::as_str)
    }

    /// Every board a known device has identified as.
    pub fn all_boards(&self) -> BTreeSet<String> {
        self.boards.values().cloned().collect()
    }

    pub fn insert(&mut self, serial: &str, board: &str) {
        if self.board(serial) != Some(board) {
            self.boards.insert(serial.to_owned(), board.to_owned());
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let text: String = self.boards.iter().map(|(s, b)| format!("{s} {b}\n")).collect();
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

    #[test]
    fn flashing_keeps_the_settings() {
        let mut image = vec![0xFF; 0x20000];
        let entry = |t: u8, sub: u8, offset: u32, size: u32| {
            let mut e = vec![0xAA, 0x50, t, sub];
            e.extend(offset.to_le_bytes());
            e.extend(size.to_le_bytes());
            e.resize(32, 0);
            e
        };
        let table: Vec<u8> = [entry(1, 2, 0x9000, 0x5000), entry(1, 0, 0xE000, 0x2000), entry(0, 0x10, 0x10000, 0x10000)]
            .concat();
        image[PARTITION_TABLE..PARTITION_TABLE + table.len()].copy_from_slice(&table);
        let parts: Vec<(u32, usize)> = keeping_nvs(&image).iter().map(|(a, d)| (*a, d.len())).collect();
        assert_eq!(parts, [(0, 0x9000), (0xE000, 0x12000)]);

        // No table: all of it.
        assert_eq!(keeping_nvs(&[0; 16]).len(), 1);
    }

    const MAC: &str = "48:27:E2:91:0B:2C";

    const BOARD: &str = LEGACY_BOARD;

    fn v040(_board: &str) -> Option<Cached> {
        Some(Cached {
            version: "0.4.0".into(),
            path: PathBuf::new(),
        })
    }

    fn flashing(known: &[&str]) -> Flashing {
        let known = KnownDevices {
            path: None,
            boards: known.iter().map(|s| (s.to_string(), BOARD.to_string())).collect(),
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
        assert!(f.known.board(MAC).is_none());
        // Firmware before protocol v3 sends no board; it only ran on ours.
        assert!(f.should_update(&p, "0.3.0", None).is_some());
        assert_eq!(f.known.board(MAC), Some(BOARD));
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
            boards: [(MAC.to_string(), BOARD.to_string())].into(),
        };
        let p = port(Some(MAC));
        let mut off = Flashing::with(v040, false, known());
        assert_eq!(off.should_update(&p, "0.3.0", Some(BOARD)), None);
        assert_eq!(off.should_recover(&p), None);
        let mut none = Flashing::with(|_| None, true, known());
        assert_eq!(none.should_update(&p, "0.3.0", Some(BOARD)), None);
        assert_eq!(none.should_recover(&p), None);
    }

    #[test]
    fn names_round_trip() {
        let name = file_name("0.6.0", BOARD, 6);
        assert_eq!(name, "nerd-deck-firmware-0.6.0-wt32-sc01-plus-proto6.bin");
        let parsed = parse_name(&name).unwrap();
        assert_eq!((parsed.version.as_str(), parsed.board, parsed.proto), ("0.6.0", BOARD, 6));
        let parsed = parse_name(&file_name("0.7.0-beta.1", "waveshare-c5-touch-35", 6)).unwrap();
        assert_eq!((parsed.version.as_str(), parsed.board), ("0.7.0-beta.1", "waveshare-c5-touch-35"));
        assert_eq!(parse_name("nerd-deck-firmware-0.6.0-other-board-proto6.bin"), None);
        assert_eq!(parse_name("nerd-deck_0.6.0-1_amd64.deb"), None);
    }

    #[test]
    fn recovers_with_the_devices_own_board() {
        fn newest(board: &str) -> Option<Cached> {
            Some(Cached {
                version: format!("0.4.0-{board}"),
                path: PathBuf::new(),
            })
        }
        let c5 = "waveshare-c5-touch-35";
        let known = KnownDevices {
            path: None,
            boards: [(MAC.to_string(), c5.to_string())].into(),
        };
        let mut f = Flashing::with(newest, true, known);
        assert_eq!(version(f.should_recover(&port(Some(MAC)))), Some(format!("0.4.0-{c5}")));
    }

    #[test]
    fn checks_the_marker() {
        let image = |id: &str| format!("\0\0nerd-deck-firmware:{id};\0").into_bytes();
        assert_eq!(Image::parse(image("wt32-sc01-plus:0.6.0")).unwrap().version, "0.6.0");
        assert_eq!(Image::parse(image("waveshare-c5-touch-35:0.6.0")).unwrap().board, "waveshare-c5-touch-35");
        assert!(Image::parse(image("other-board:0.6.0")).is_err());
        assert!(Image::parse(b"not firmware".to_vec()).is_err());
    }
}
