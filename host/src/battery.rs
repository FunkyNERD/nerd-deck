//! System battery from /sys/class/power_supply. sysinfo doesn't cover batteries.

use std::fs;
use std::path::Path;

use crate::protocol::BatteryStats;

const POWER_SUPPLY: &str = "/sys/class/power_supply";

pub fn read() -> Option<BatteryStats> {
    read_from(Path::new(POWER_SUPPLY))
}

/// Minutes until the battery is empty (discharging) or reaches its charge
/// limit (charging), at `watts`. The collector passes a smoothed rate, since
/// the instant one jumps around.
pub fn minutes_left(b: &BatteryStats, watts: f32) -> Option<u32> {
    let (wh, full) = (b.wh?, b.full_wh?);
    if watts < 0.5 {
        return None;
    }
    let hours = match b.state {
        "discharging" => wh / watts,
        "charging" => {
            let target = full * f32::from(b.limit.unwrap_or(100).min(100)) / 100.0;
            if target <= wh {
                return None;
            }
            (target - wh) / watts
        }
        _ => return None,
    };
    // Past a couple of days it's noise from a near-zero rate.
    (hours < 48.0).then(|| (hours * 60.0).round() as u32)
}

/// Combines every system battery (some laptops have two). `None` when there
/// are none, so desktops send no `battery` field.
fn read_from(root: &Path) -> Option<BatteryStats> {
    let mut now = 0u64;
    let mut full = 0u64;
    let mut capacities = Vec::new();
    let mut states = Vec::new();
    let (mut watts, mut wh, mut full_wh, mut design_wh) = (None::<f32>, None::<f32>, None::<f32>, None::<f32>);
    let (mut volts, mut temp, mut cycles, mut limit) = (None, None::<f32>, None, None);
    let mut ac = false;
    let mut charger_w = None::<f32>;

    let add = |total: &mut Option<f32>, v: Option<f32>| {
        if let Some(v) = v {
            *total = Some(total.unwrap_or(0.0) + v);
        }
    };

    for entry in fs::read_dir(root).ok()?.flatten() {
        let dir = entry.path();
        let attr = |name: &str| {
            fs::read_to_string(dir.join(name))
                .ok()
                .map(|s| s.trim().to_owned())
        };
        let num = |name: &str| attr(name).and_then(|s| s.parse::<u64>().ok());
        // Microunits (µW, µWh, µA, µV, µAh) as plain units.
        let micro = |name: &str| num(name).map(|v| v as f32 / 1e6);

        let kind = attr("type");
        // Wireless mice and keyboards report scope "Device"; only system supplies count.
        if attr("scope").as_deref() == Some("Device") {
            continue;
        }
        if matches!(kind.as_deref(), Some("Mains" | "USB")) {
            if attr("online").as_deref() == Some("1") {
                ac = true;
                // USB-C PD sources report the negotiated voltage and current.
                if let Some(w) = micro("voltage_now").zip(micro("current_now")).map(|(v, a)| v * a) {
                    if w > 0.5 {
                        charger_w = Some(charger_w.map_or(w, |c| c.max(w)));
                    }
                }
            }
            continue;
        }
        if kind.as_deref() != Some("Battery") || attr("present").as_deref() == Some("0") {
            continue;
        }
        let Some(capacity) = num("capacity") else {
            continue;
        };

        capacities.push(capacity.min(100));
        states.push(attr("status").unwrap_or_default());
        // Drivers report either energy (µWh) or charge (µAh); either works as a ratio.
        if let Some((n, f)) = num("energy_now")
            .zip(num("energy_full"))
            .or_else(|| num("charge_now").zip(num("charge_full")))
        {
            now += n;
            full += f;
        }

        let v_now = micro("voltage_now");
        // Charge becomes energy at the battery's nominal voltage.
        let nominal = micro("voltage_min_design").or(v_now);
        let energy = |e: &str, c: &str| micro(e).or_else(|| micro(c).zip(nominal).map(|(ah, v)| ah * v));
        add(&mut wh, energy("energy_now", "charge_now"));
        add(&mut full_wh, energy("energy_full", "charge_full"));
        add(&mut design_wh, energy("energy_full_design", "charge_full_design"));
        add(&mut watts, micro("power_now").or_else(|| micro("current_now").zip(v_now).map(|(a, v)| a * v)));
        volts = volts.or(v_now);
        // Tenths of a degree.
        if let Some(t) = attr("temp").and_then(|t| t.parse::<f32>().ok()).map(|t| t / 10.0) {
            temp = Some(temp.map_or(t, |m| m.max(t)));
        }
        cycles = cycles.or(num("cycle_count").filter(|&c| c > 0).map(|c| c as u32));
        limit = limit.or(num("charge_control_end_threshold").filter(|&l| l < 100).map(|l| l as u8));
    }

    if capacities.is_empty() {
        return None;
    }
    let percent = if capacities.len() > 1 && full > 0 {
        (now * 100 / full).min(100)
    } else {
        capacities.iter().sum::<u64>() / capacities.len() as u64
    };
    let has = |s: &str| states.iter().any(|st| st == s);
    let state = if has("Charging") {
        "charging"
    } else if has("Discharging") || (!ac && !has("Full")) {
        "discharging"
    } else if states.iter().all(|st| st == "Full") {
        "full"
    } else {
        // Plugged in but held, say at a charge limit.
        "idle"
    };
    let round = |v: Option<f32>| v.map(|v| (v * 10.0).round() / 10.0);
    Some(BatteryStats {
        percent: percent as u8,
        charging: state == "charging",
        state,
        watts: round(watts),
        minutes: None,
        wh: round(wh),
        full_wh: round(full_wh),
        design_wh: round(design_wh),
        volts: round(volts),
        temp: round(temp),
        cycles,
        limit,
        ac,
        charger_w: charger_w.map(f32::round),
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
        assert_eq!(b.state, "discharging");
        assert_eq!((b.wh, b.full_wh), (Some(32.0), Some(80.0)));
    }

    #[test]
    fn charge_based_battery_on_usb_c() {
        // As a Dell reports it: charge in µAh, current and voltage, a charge limit.
        let fs = FakeSysfs::new("dell");
        fs.supply(
            "BAT0",
            &[
                ("type", "Battery"),
                ("capacity", "60"),
                ("status", "Charging"),
                ("charge_now", "4000000"),
                ("charge_full", "8000000"),
                ("charge_full_design", "7500000"),
                ("current_now", "1000000"),
                ("voltage_now", "12500000"),
                ("voltage_min_design", "11400000"),
                ("charge_control_end_threshold", "85"),
                ("cycle_count", "0"),
                ("temp", "246"),
            ],
        );
        fs.supply(
            "ucsi-source-psy-USBC000:001",
            &[("type", "USB"), ("online", "1"), ("voltage_now", "15000000"), ("current_now", "4250000")],
        );
        let b = read_from(&fs.0).unwrap();
        assert_eq!(b.state, "charging");
        assert_eq!(b.watts, Some(12.5));
        assert_eq!((b.wh, b.full_wh, b.design_wh), (Some(45.6), Some(91.2), Some(85.5)));
        assert_eq!((b.volts, b.temp, b.cycles, b.limit), (Some(12.5), Some(24.6), None, Some(85)));
        assert!(b.ac);
        assert_eq!(b.charger_w, Some(64.0));
        // To 85% of 91.2 Wh = 77.5 Wh from 45.6 Wh at 12.5 W: about 2.55 h.
        assert_eq!(minutes_left(&b, 12.5), Some(153));
    }

    #[test]
    fn held_at_the_limit_is_idle() {
        let fs = FakeSysfs::new("idle");
        fs.supply("AC", &[("type", "Mains"), ("online", "1")]);
        fs.supply("BAT0", &[("type", "Battery"), ("capacity", "85"), ("status", "Not charging")]);
        let b = read_from(&fs.0).unwrap();
        assert_eq!((b.state, b.charging, b.ac), ("idle", false, true));
    }
}
