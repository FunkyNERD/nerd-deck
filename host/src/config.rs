use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub device: DeviceConfig,
    pub stats: StatsConfig,
    #[serde(rename = "button")]
    pub buttons: Vec<ButtonConfig>,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StatsConfig {
    pub interval_ms: u64,
    /// Network interface to report. When unset, the busiest non-loopback one is used.
    pub interface: Option<String>,
    /// Mount points to report (the device shows up to 4).
    pub mounts: Vec<String>,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            interval_ms: 1000,
            interface: None,
            mounts: vec!["/".into()],
        }
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
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("nerd-deck").join("config.toml"))
    }

    /// Load from `path`, or from the default location if `path` is None.
    /// A missing default file is not an error; a missing explicit one is.
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
                log::warn!("no config at {}, using defaults", path.display());
                return Ok(Self::default());
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };

        let cfg: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for b in &self.buttons {
            anyhow::ensure!(seen.insert(&b.id), "duplicate button id {:?}", b.id);
        }
        Ok(())
    }

    pub fn button(&self, id: &str) -> Option<&ButtonConfig> {
        self.buttons.iter().find(|b| b.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let cfg: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.device.vid, 0x303a);
        assert!(!cfg.buttons.is_empty());
    }
}
