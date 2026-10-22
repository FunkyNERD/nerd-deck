//! System battery from /sys/class/power_supply. sysinfo doesn't cover batteries.

use std::fs;
use std::path::Path;

use crate::protocol::BatteryStats;

const POWER_SUPPLY: &str = "/sys/class/power_supply";

pub fn read() -> Option<BatteryStats> {
    read_from(Path::new(POWER_SUPPLY))
}

/// Combines every system battery (some laptops have two). `None` when there
/// are none, so desktops send no `battery` field.
fn read_from(root: &Path) -> Option<BatteryStats> {
    let mut now = 0u64;
    let mut full = 0u64;
    let mut capacities = Vec::new();
    let mut charging = false;

    for entry in fs::read_dir(root).ok()?.flatten() {
        let dir = entry.path();
        let attr = |name: &str| {
            fs::read_to_string(dir.join(name))
                .ok()
                .map(|s| s.trim().to_owned())
        };
        let num = |name: &str| attr(name).and_then(|s| s.parse::<u64>().ok());

        // Wireless mice and keyboards report scope "Device"; only system batteries count.
        if attr("type").as_deref() != Some("Battery") || attr("scope").as_deref() == Some("Device")
        {
            continue;
        }
        if attr("present").as_deref() == Some("0") {
            continue;
        }
        let Some(capacity) = num("capacity") else {
            continue;
        };

        capacities.push(capacity.min(100));
        charging |= attr("status").as_deref() == Some("Charging");
        // Drivers report either energy (µWh) or charge (µAh); either works as a ratio.
        if let Some((n, f)) = num("energy_now")
            .zip(num("energy_full"))
            .or_else(|| num("charge_now").zip(num("charge_full")))
        {
            now += n;
            full += f;
        }
    }

    if capacities.is_empty() {
        return None;
    }
    let percent = if capacities.len() > 1 && full > 0 {
        (now * 100 / full).min(100)
    } else {
        capacities.iter().sum::<u64>() / capacities.len() as u64
    };
    Some(BatteryStats {
        percent: percent as u8,
        charging,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct FakeSysfs(PathBuf);

    impl FakeSysfs {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("nerd-deck-battery-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn supply(&self, name: &str, attrs: &[(&str, &str)]) {
            let dir = self.0.join(name);
            fs::create_dir_all(&dir).unwrap();
            for (k, v) in attrs {
                fs::write(dir.join(k), format!("{v}\n")).unwrap();
            }
        }
    }

    impl Drop for FakeSysfs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn no_battery_is_none() {
        let fs = FakeSysfs::new("none");
        fs.supply("AC", &[("type", "Mains"), ("online", "1")]);
        fs.supply(
            "hidpp_battery_0",
            &[("type", "Battery"), ("scope", "Device"), ("capacity", "40")],
        );
        assert!(read_from(&fs.0).is_none());
    }

    #[test]
    fn single_battery() {
        let fs = FakeSysfs::new("single");
        fs.supply(
            "BAT0",
            &[
                ("type", "Battery"),
                ("capacity", "85"),
                ("status", "Charging"),
            ],
        );
        let b = read_from(&fs.0).unwrap();
        assert_eq!((b.percent, b.charging), (85, true));
    }

    #[test]
    fn two_batteries_weighted_by_energy() {
        let fs = FakeSysfs::new("dual");
        fs.supply(
            "BAT0",
            &[
                ("type", "Battery"),
                ("capacity", "100"),
                ("status", "Full"),
                ("energy_now", "20000000"),
                ("energy_full", "20000000"),
            ],
        );
        fs.supply(
            "BAT1",
            &[
                ("type", "Battery"),
                ("capacity", "20"),
                ("status", "Discharging"),
                ("energy_now", "12000000"),
                ("energy_full", "60000000"),
            ],
        );
        let b = read_from(&fs.0).unwrap();
        assert_eq!((b.percent, b.charging), (40, false));
    }
}
