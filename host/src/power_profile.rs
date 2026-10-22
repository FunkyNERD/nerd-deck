//! The power profile for the System tab: the one the desktop sets through
//! power-profiles-daemon ("power-saver", "balanced" or "performance"), else
//! the firmware's ACPI platform profile ("quiet", "balanced", ...).

use std::fs;

use zbus::blocking::{Connection, Proxy};

const PLATFORM_PROFILE: &str = "/sys/firmware/acpi/platform_profile";

/// power-profiles-daemon's names: the current one, then the one before 0.20.
const DAEMONS: [(&str, &str, &str); 2] = [
    (
        "org.freedesktop.UPower.PowerProfiles",
        "/org/freedesktop/UPower/PowerProfiles",
        "org.freedesktop.UPower.PowerProfiles",
    ),
    ("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles", "net.hadess.PowerProfiles"),
];

pub struct PowerProfile {
    /// Empty without a system bus. The proxies follow the daemon's
    /// ActiveProfile as it changes, so reading it is cheap.
    daemons: Vec<Proxy<'static>>,
}

impl PowerProfile {
    pub fn new() -> Self {
        let daemons = Connection::system()
            .map(|conn| {
                DAEMONS
                    .iter()
                    .filter_map(|&(name, path, iface)| Proxy::new(&conn, name, path, iface).ok())
                    .collect()
            })
            .unwrap_or_default();
        Self { daemons }
    }

    /// The active profile's name, or None when nothing reports one.
    pub fn current(&self) -> Option<String> {
        self.daemons
            .iter()
            .find_map(|p| p.get_property::<String>("ActiveProfile").ok())
            .or_else(|| fs::read_to_string(PLATFORM_PROFILE).ok())
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty())
    }
}
