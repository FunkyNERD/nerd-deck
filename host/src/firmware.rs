//! The device firmware bundled into this build (see build.rs), and installing
//! it through the ESP32-S3's ROM bootloader over the same USB serial port. The
//! ROM can't be overwritten, so an interrupted flash is always recoverable.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::Flasher;
use espflash::image_format::Segment;
use espflash::target::{Chip, ProgressCallbacks};
use serialport::UsbPortInfo;

use crate::config::Config;
use crate::device::Port;
use crate::updates;

/// The board the bundled firmware is built for (FW_BOARD in firmware/src/config.hpp;
/// build.rs refuses an image for any other).
pub const BOARD: &str = "wt32-sc01-plus";
/// Firmware before protocol v3 doesn't report a board, and only ever ran on this one.
pub const LEGACY_BOARD: &str = BOARD;
const MAX_FLASH_ATTEMPTS: u32 = 2;

/// Merged image (bootloader, partitions, app) written at flash offset 0.
static IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/firmware.bin"));
const VERSION: &str = env!("NERD_DECK_FIRMWARE_VERSION");

pub struct Bundled {
    pub version: &'static str,
    image: &'static [u8],
}

pub fn bundled() -> Option<Bundled> {
    (!IMAGE.is_empty()).then_some(Bundled {
        version: VERSION,
        image: IMAGE,
    })
}

/// Decides when to install the bundled firmware: only on the board it's built
/// for, only when it's newer, and at most MAX_FLASH_ATTEMPTS times per device
/// per run, so a build that reports the wrong version can't flash in a loop.
pub struct Flashing {
    /// None when there's no bundled firmware or updates are off.
    firmware: Option<Bundled>,
    known: KnownDevices,
    attempts: HashMap<String, u32>,
}

impl Flashing {
    pub fn new(cfg: &Config) -> Self {
        Self::with(bundled(), cfg.updates.firmware, KnownDevices::load())
    }

    fn with(firmware: Option<Bundled>, enabled: bool, known: KnownDevices) -> Self {
        match &firmware {
            Some(fw) if enabled => log::info!("bundled device firmware {}", fw.version),
            Some(_) => log::info!("device firmware updates are off"),
            None => log::debug!("no bundled device firmware"),
        }
        let firmware = firmware.filter(|_| enabled);
        Self {
            firmware,
            known,
            attempts: HashMap::new(),
        }
    }

    /// Re-read the known devices, which the settings window can change.
    pub fn reload_known(&mut self) {
        self.known = KnownDevices::load();
    }

    /// Called on the device's hello. Returns the version to install, if any.
    pub fn should_update(
        &mut self,
        port: &Port,
        fw: &str,
        board: Option<&str>,
    ) -> Option<&'static str> {
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
        let version = self.firmware.as_ref()?.version;
        if !updates::is_newer(version, fw) {
            return None;
        }
        if !self.take_attempt(port) {
            log::warn!("device still has firmware {fw}; not trying to install {version} again");
            return None;
        }
        log::info!("updating device firmware {fw} -> {version}");
        Some(version)
    }

    /// Called when the device hasn't said hello. Only a device that has
    /// identified as our board before (by USB serial number) is reflashed.
    pub fn should_recover(&mut self, port: &Port) -> bool {
        let Some(version) = self.firmware.as_ref().map(|f| f.version) else {
            return false;
        };
        if !port.serial().is_some_and(|s| self.known.contains(s)) || !self.take_attempt(port) {
            return false;
        }
        log::warn!("known device isn't answering; reinstalling firmware {version}");
        true
    }

    fn take_attempt(&mut self, port: &Port) -> bool {
        let key = port.serial().unwrap_or(&port.name).to_owned();
        let attempts = self.attempts.entry(key).or_default();
        *attempts += 1;
        *attempts <= MAX_FLASH_ATTEMPTS
    }

    pub fn flash(&self, port: &Port, info: UsbPortInfo) {
        let Some(fw) = &self.firmware else { return };
        match flash(fw, &port.name, info) {
            Ok(()) => log::info!(
                "installed firmware {}; waiting for the device to restart",
                fw.version
            ),
            Err(e) => log::error!("firmware update failed: {e:#}"),
        }
    }
}

/// Resets the device into its ROM bootloader, writes the image and resets it
/// back into the new firmware. Blocks for roughly 10-20 s.
pub fn flash(fw: &Bundled, port: &str, info: UsbPortInfo) -> Result<()> {
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
                data: Cow::Borrowed(fw.image),
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

    fn flashing(known: &[&str]) -> Flashing {
        let known = KnownDevices {
            path: None,
            serials: known.iter().map(|s| s.to_string()).collect(),
        };
        Flashing::with(
            Some(Bundled {
                version: "0.4.0",
                image: &[],
            }),
            true,
            known,
        )
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

    #[test]
    fn upgrades_only() {
        let mut f = flashing(&[]);
        let p = port(Some(MAC));
        assert_eq!(f.should_update(&p, "0.3.0", Some(BOARD)), Some("0.4.0"));
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
        assert_eq!(f.should_update(&p, "0.3.0", None), Some("0.4.0"));
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
        assert!(!f.should_recover(&p));
    }

    #[test]
    fn recovers_known_devices_only() {
        assert!(flashing(&[MAC]).should_recover(&port(Some(MAC))));
        assert!(!flashing(&[MAC]).should_recover(&port(Some("AA:BB:CC:DD:EE:FF"))));
        assert!(!flashing(&[MAC]).should_recover(&port(None)));
    }

    #[test]
    fn does_nothing_when_off_or_unbundled() {
        let known = || KnownDevices {
            path: None,
            serials: [MAC.to_string()].into(),
        };
        let p = port(Some(MAC));
        let mut off = Flashing::with(
            Some(Bundled {
                version: "0.4.0",
                image: &[],
            }),
            false,
            known(),
        );
        assert_eq!(off.should_update(&p, "0.3.0", Some(BOARD)), None);
        assert!(!off.should_recover(&p));
        let mut none = Flashing::with(None, true, known());
        assert_eq!(none.should_update(&p, "0.3.0", Some(BOARD)), None);
        assert!(!none.should_recover(&p));
    }
}
