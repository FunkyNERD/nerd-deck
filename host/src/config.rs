use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use serde::Deserialize;

use crate::icons;
use crate::images;

/// The shortest `stats.interval_ms`. The deck keeps up with faster, but it
/// spends so much of its time drawing that taps get slow, and CPU usage over
/// such a short time is noise.
pub const MIN_INTERVAL_MS: u64 = 100;

// Limits imposed by the firmware (firmware/src/config.hpp, stats.hpp).
pub const MAX_BUTTONS: usize = 12;
pub const MAX_ID_LEN: usize = 23;
pub const MAX_LABEL_LEN: usize = 31;

/// Written to the default location on first start, so there's a commented
/// config to edit.
pub const EXAMPLE: &str = include_str!("../config.example.toml");

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub device: DeviceConfig,
    pub stats: StatsConfig,
    /// Before 0.10 the deck's settings lived here. Each deck now keeps its
    /// own, so this is read only so that older configs still load.
    #[serde(rename = "display")]
    pub old_display: Option<toml::Table>,
    pub updates: UpdatesConfig,
    pub notifications: NotificationsConfig,
    #[serde(rename = "button")]
    pub buttons: Vec<ButtonConfig>,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeviceConfig {
    /// Explicit serial port. When unset, the first port matching vid/pid is used.
    pub port: Option<String>,
    pub vid: u16,
    pub pid: Option<u16>,
    pub baud: u32,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            port: None,
            // Espressif USB-Serial-JTAG, which is what the S3's native USB shows up as.
            vid: 0x303a,
            pid: Some(0x1001),
            baud: 115_200,
        }
    }
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StatsConfig {
    pub interval_ms: u64,
    /// Network interface to report. When unset, the busiest non-loopback one is used.
    pub interface: Option<String>,
    /// Mount points to report (the device shows up to 4).
    pub mounts: Vec<String>,
    /// Also report removable drives (USB disks, SD cards) while they're mounted,
    /// after `mounts`.
    pub removable: bool,
    /// Show the battery in the device's header, on PCs that have one.
    pub battery: bool,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            interval_ms: 1000,
            interface: None,
            mounts: vec!["/".into()],
            removable: true,
            battery: true,
        }
    }
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdatesConfig {
    /// Check GitHub daily for a newer release and show a notice on the device.
    pub check: bool,
    /// Download new device firmware from GitHub releases and install it when
    /// the device's is older (only on the board it's built for).
    pub firmware: bool,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self { check: true, firmware: true }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationsConfig {
    /// Show desktop notifications on the device too.
    pub enabled: bool,
    /// Only these apps, by name or desktop file / app ID. Empty means all.
    pub apps: Vec<String>,
    /// Never these apps.
    pub ignore: Vec<String>,
    /// Include the notification's body text, not just its title.
    pub body: bool,
    /// Hold notifications back while the desktop's Do Not Disturb is on
    /// (critical ones still show).
    pub dnd: bool,
    /// Leave out low-urgency notifications.
    pub skip_low: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self { enabled: false, apps: vec![], ignore: vec![], body: true, dnd: true, skip_low: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ButtonConfig {
    pub id: String,
    pub label: String,
    /// Run with `sh -c`. Only commands from this file are ever executed; the
    /// device can only refer to them by id.
    pub command: String,
    /// Name of a built-in LVGL symbol, e.g. "mute" (see icons.rs).
    #[serde(default)]
    pub icon: Option<String>,
    /// PNG/JPEG/BMP file; `~/` and paths relative to the config file work.
    #[serde(default)]
    pub image: Option<PathBuf>,
    /// `image`, loaded; it's sized for the device when the buttons are sent.
    #[serde(skip)]
    pub image_data: Option<image::DynamicImage>,
    /// The label's colour, "#RRGGBB". The deck's own when unset.
    #[serde(default)]
    pub color: Option<String>,
}

/// "#RRGGBB" (or "RRGGBB") as 0xRRGGBB.
pub fn parse_color(s: &str) -> Option<u32> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok()).flatten()
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("nerd-deck").join("config.toml"))
    }

    /// Load from `path`, or from the default location if `path` is None.
    /// A missing default file is created from the example; a missing explicit
    /// one is an error.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let (path, explicit) = match path {
            Some(p) => (p.to_path_buf(), true),
            None => match Self::default_path() {
                Some(p) => (p, false),
                None => return Ok(Self::default()),
            },
        };

        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if !explicit && e.kind() == std::io::ErrorKind::NotFound => {
                match write_example(&path) {
                    Ok(()) => log::info!("created {} from the example", path.display()),
                    Err(e) => log::warn!("can't create {} ({e}); using the example", path.display()),
                }
                EXAMPLE.to_owned()
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };

        let mut cfg: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate()?;
        cfg.load_images(path.parent().unwrap_or(Path::new(".")))?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.stats.interval_ms >= MIN_INTERVAL_MS,
            "stats.interval_ms is {}; the shortest is {MIN_INTERVAL_MS}",
            self.stats.interval_ms
        );
        ensure!(
            self.buttons.len() <= MAX_BUTTONS,
            "{} buttons configured; the device shows at most {MAX_BUTTONS}",
            self.buttons.len()
        );
        let mut seen = std::collections::HashSet::new();
        for b in &self.buttons {
            ensure!(seen.insert(&b.id), "duplicate button id {:?}", b.id);
            ensure!(
                !b.id.is_empty() && b.id.len() <= MAX_ID_LEN,
                "button id {:?} must be 1-{MAX_ID_LEN} bytes",
                b.id
            );
            ensure!(
                b.label.len() <= MAX_LABEL_LEN,
                "button {:?}: label must be at most {MAX_LABEL_LEN} bytes",
                b.id
            );
            ensure!(
                b.icon.is_none() || b.image.is_none(),
                "button {:?}: set either icon or image, not both",
                b.id
            );
            if let Some(color) = &b.color {
                ensure!(
                    parse_color(color).is_some(),
                    "button {:?}: color {color:?} isn't a colour like \"#FFAA00\"",
                    b.id
                );
            }
            if let Some(icon) = &b.icon {
                ensure!(
                    icons::lookup(icon).is_some(),
                    "button {:?}: unknown icon {icon:?}; available: {}",
                    b.id,
                    icons::names().collect::<Vec<_>>().join(", ")
                );
            }
        }
        Ok(())
    }

    fn load_images(&mut self, config_dir: &Path) -> Result<()> {
        for b in &mut self.buttons {
            if let Some(path) = &b.image {
                let path = resolve_path(config_dir, path);
                let img = images::load(&path).with_context(|| format!("button {:?}", b.id))?;
                log::debug!("button {:?}: {} ({}x{})", b.id, path.display(), img.width(), img.height());
                b.image_data = Some(img);
            }
        }
        Ok(())
    }

    pub fn button(&self, id: &str) -> Option<&ButtonConfig> {
        self.buttons.iter().find(|b| b.id == id)
    }
}

/// Writes the example config to `path`, never replacing an existing file.
fn write_example(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(EXAMPLE.as_bytes())
}

pub fn resolve_path(config_dir: &Path, path: &Path) -> PathBuf {
    if let (Ok(rest), Some(home)) = (path.strip_prefix("~"), dirs::home_dir()) {
        home.join(rest)
    } else {
        config_dir.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let cfg: Config = toml::from_str(EXAMPLE).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.device.vid, 0x303a);
        assert!(!cfg.buttons.is_empty());
    }

    fn parse(toml: &str) -> Result<Config> {
        let cfg: Config = toml::from_str(toml)?;
        cfg.validate()?;
        Ok(cfg)
    }

    #[test]
    fn rejects_too_short_an_interval() {
        assert!(parse("[stats]\ninterval_ms = 100").is_ok());
        let err = parse("[stats]\ninterval_ms = 20").unwrap_err();
        assert!(err.to_string().contains("shortest is 100"));
    }

    #[test]
    fn rejects_unknown_icon() {
        let err = parse("[[button]]\nid='a'\nlabel='A'\ncommand='true'\nicon='nope'").unwrap_err();
        assert!(err.to_string().contains("unknown icon"));
    }

    #[test]
    fn parses_colors() {
        assert_eq!(parse_color("#26C6DA"), Some(0x26C6DA));
        assert_eq!(parse_color("ffffff"), Some(0xFFFFFF));
        assert_eq!(parse_color("#fff"), None);
        assert!(parse("[[button]]\nid='a'\nlabel='A'\ncommand='true'\ncolor='red'").is_err());
    }

    #[test]
    fn old_display_settings_are_ignored() {
        let cfg = parse("[display]\nbrightness = 2\nflip = true").unwrap();
        assert!(cfg.old_display.is_some());
    }

    #[test]
    fn rejects_icon_and_image() {
        let toml = "[[button]]\nid='a'\nlabel='A'\ncommand='true'\nicon='mute'\nimage='x.png'";
        assert!(parse(toml).is_err());
    }

    #[test]
    fn writes_example_once() {
        let dir = std::env::temp_dir().join(format!("nerd-deck-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nerd-deck").join("config.toml");

        write_example(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), EXAMPLE);

        // An existing config is never replaced.
        std::fs::write(&path, "# mine").unwrap();
        assert!(write_example(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# mine");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolves_image_paths() {
        let dir = Path::new("/etc/nerd-deck");
        assert_eq!(resolve_path(dir, Path::new("a.png")), Path::new("/etc/nerd-deck/a.png"));
        assert_eq!(resolve_path(dir, Path::new("/x/a.png")), Path::new("/x/a.png"));
        let home = dirs::home_dir().unwrap();
        assert_eq!(resolve_path(dir, Path::new("~/a.png")), home.join("a.png"));
    }
}
