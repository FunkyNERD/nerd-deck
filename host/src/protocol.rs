//! Wire format shared with the firmware: one JSON object per line, tagged by
//! `type`. Keep in sync with docs/PROTOCOL.md and firmware/src/protocol.cpp.

use serde::{Deserialize, Serialize};

/// Bump together with `kProtoVersion` in firmware/src/config.hpp.
pub const PROTO_VERSION: u32 = 6;

/// Messages the host sends to the device.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMsg {
    Hello {
        proto: u32,
        host: String,
    },
    Stats(Stats),
    Buttons {
        buttons: Vec<ButtonDef>,
    },
    Ack {
        id: String,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        msg: Option<String>,
    },
    /// A notice for the device to show briefly, such as an available update.
    Notify {
        text: String,
    },
    /// A desktop notification, such as new mail. Added in v4.
    Notification {
        app: String,
        summary: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
        /// Stays up until tapped.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        critical: bool,
    },
    /// Changes to the settings the deck keeps, and how often stats come.
    /// The deck replies with a device `Display`. Added in v6.
    Display {
        #[serde(flatten)]
        change: DisplayChange,
        /// Milliseconds between stats, so the deck knows how long to wait
        /// for the next before it takes the host for gone. Not kept. Added in
        /// firmware 0.9.0; older firmware waits 3 s.
        #[serde(skip_serializing_if = "Option::is_none")]
        stats_ms: Option<u64>,
    },
    /// One chunk of a button image. Sent after `Buttons`, in order, for each
    /// button with `image: true`.
    Image {
        id: String,
        w: u32,
        h: u32,
        fmt: &'static str,
        /// Byte offset of this chunk within the decoded image.
        offset: usize,
        /// Decoded size of the whole image.
        total: usize,
        /// Base64 of this chunk's bytes.
        data: String,
    },
}

/// Messages the device sends to the host.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceMsg {
    Hello {
        fw: String,
        proto: u32,
        /// Added in v3; older firmware leaves it out.
        #[serde(default)]
        board: Option<String>,
        /// Size of a Controls button's face in pixels, which images are
        /// cropped to fill. Added in v5; older firmware shows images as icons.
        #[serde(default)]
        tile: Option<[u32; 2]>,
        /// The settings this deck keeps. Added in firmware 0.10.0; older
        /// firmware leaves it out, and keeps what it last had.
        #[serde(default)]
        display: Option<DisplaySettings>,
    },
    Press { id: String },
    /// The deck's settings after a host `Display` changed them. Added in
    /// firmware 0.10.0.
    Display(DisplaySettings),
    /// An image finished arriving (`ok`) or was dropped after a lost chunk.
    /// Added in v5; the host waits for it before sending the next image.
    Image { id: String, ok: bool },
    #[serde(other)]
    Unknown,
}

/// The deck's tabs, by the names `tabs` and `start` take, in their default
/// order.
pub const TABS: [&str; 5] = ["system", "network", "storage", "battery", "controls"];

/// The settings a deck keeps in its flash, so they apply from power-on. Each
/// deck has its own: it reports them, and the host only changes them when
/// asked to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplaySettings {
    /// Tab order; tabs it leaves out follow, in their default order.
    pub tabs: Vec<String>,
    /// The tab the deck opens on: one of TABS, or "first" for the first in `tabs`.
    pub start: String,
    /// Backlight, 5-100 percent.
    pub brightness: u8,
    /// Seconds untouched before the screen dims; 0 never.
    pub dim_after: u32,
    /// Seconds on the waiting screen before the screen turns off; 0 never.
    pub off_after: u32,
    /// °C from which the CPU temperature shows orange, and red.
    pub temp_warm: u8,
    pub temp_hot: u8,
    /// Upside down, for the USB cable on the other side.
    pub flip: bool,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            tabs: TABS.iter().map(|t| (*t).to_owned()).collect(),
            start: "first".into(),
            brightness: 80,
            dim_after: 0,
            off_after: 0,
            temp_warm: 70,
            temp_hot: 85,
            flip: false,
        }
    }
}

impl DisplaySettings {
    /// Every tab, in this order.
    pub fn tab_order(&self) -> Vec<&'static str> {
        let mut order: Vec<&'static str> =
            self.tabs.iter().filter_map(|t| TABS.iter().find(|n| **n == t.as_str()).copied()).collect();
        for t in TABS {
            if !order.contains(&t) {
                order.push(t);
            }
        }
        order
    }

    pub fn apply(&mut self, c: &DisplayChange) {
        if let Some(v) = &c.tabs {
            self.tabs = v.clone();
        }
        if let Some(v) = &c.start {
            self.start = v.clone();
        }
        macro_rules! copy {
            ($($f:ident),*) => { $(if let Some(v) = c.$f { self.$f = v; })* };
        }
        copy!(brightness, dim_after, off_after, temp_warm, temp_hot, flip);
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        use anyhow::ensure;
        ensure!((5..=100).contains(&self.brightness), "brightness must be 5-100, not {}", self.brightness);
        ensure!(
            self.start == "first" || TABS.contains(&self.start.as_str()),
            "start {:?} isn't \"first\" or a tab: {}",
            self.start,
            TABS.join(", ")
        );
        ensure!(
            (30..=120).contains(&self.temp_warm) && (30..=120).contains(&self.temp_hot),
            "temp_warm and temp_hot must be 30-120 °C"
        );
        ensure!(
            self.temp_warm < self.temp_hot,
            "temp_warm ({}) must be below temp_hot ({})",
            self.temp_warm,
            self.temp_hot
        );
        let mut seen = std::collections::HashSet::new();
        for t in &self.tabs {
            ensure!(TABS.contains(&t.as_str()), "unknown tab {t:?}; the tabs are {}", TABS.join(", "));
            ensure!(seen.insert(t), "tabs lists {t:?} twice");
        }
        Ok(())
    }
}

/// Some of a deck's settings, to change; the rest stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tabs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dim_after: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off_after: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_warm: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_hot: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flip: Option<bool>,
}

impl DisplayChange {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Adds `later`'s changes, which win.
    pub fn merge(&mut self, later: DisplayChange) {
        macro_rules! take {
            ($($f:ident),*) => { $(if later.$f.is_some() { self.$f = later.$f; })* };
        }
        take!(tabs, start, brightness, dim_after, off_after, temp_warm, temp_hot, flip);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ButtonDef {
    pub id: String,
    pub label: String,
    /// A built-in LVGL symbol glyph, sent as text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// An `Image` message for this button follows.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub image: bool,
    /// The label's colour, 0xRRGGBB. Added in v5.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub host: String,
    /// Seconds.
    pub uptime: u64,
    pub load: [f64; 3],
    pub cpu: CpuStats,
    pub mem: MemStats,
    pub net: NetStats,
    pub disks: Vec<DiskStats>,
    /// The discrete GPU, when there's one to read. Added in v6.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu: Option<GpuStats>,
    /// The busiest program, on hosts without `gpu`. Added in v6.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top: Option<TopProcess>,
    /// Left out when the host has no battery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery: Option<BatteryStats>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CpuStats {
    /// Percent, 0-100.
    pub usage: f32,
    pub cores: Vec<f32>,
    pub freq_mhz: u64,
    /// Degrees Celsius, if a sensor was found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp: Option<f32>,
    /// The power profile, such as "balanced" or "performance", when the PC
    /// reports one. Added in firmware 0.8.0; older firmware ignores it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// All values in bytes.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MemStats {
    pub used: u64,
    pub total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NetStats {
    pub iface: String,
    pub rx_bps: u64,
    pub tx_bps: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DiskStats {
    pub mount: String,
    pub used: u64,
    pub total: u64,
    /// A USB drive or SD card.
    pub removable: bool,
    /// Bytes/s. Added in v6.
    pub read_bps: u64,
    pub write_bps: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GpuStats {
    /// Powered down: the other fields are left out, since reading them would
    /// wake it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub asleep: bool,
    /// Percent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<u8>,
    /// Degrees Celsius.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp: Option<f32>,
    /// Bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_used: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_total: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TopProcess {
    /// The program; its processes are counted together.
    pub name: String,
    /// Percent of the whole CPU, like `cpu.usage`.
    pub cpu: f32,
    /// Bytes.
    pub mem: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BatteryStats {
    /// 0-100.
    pub percent: u8,
    pub charging: bool,
    /// "charging", "discharging", "full", or "idle" (plugged in, not charging,
    /// say held at a charge limit). Added in v5, like the fields below; each
    /// is left out when the battery doesn't report it.
    pub state: &'static str,
    /// Into the battery while charging, out of it otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watts: Option<f32>,
    /// To empty while discharging, to full (or the charge limit) while charging.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minutes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wh: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_wh: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub design_wh: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volts: Option<f32>,
    /// Degrees Celsius.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycles: Option<u32>,
    /// Charging stops at this percentage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u8>,
    /// On mains or USB-C power.
    pub ac: bool,
    /// The charger's negotiated power, when it reports one (USB-C PD).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charger_w: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_msgs_are_tagged() {
        let json = serde_json::to_string(&HostMsg::Ack {
            id: "lock".into(),
            ok: true,
            msg: None,
        })
        .unwrap();
        assert_eq!(json, r#"{"type":"ack","id":"lock","ok":true}"#);

        let json = serde_json::to_string(&HostMsg::Stats(Stats::default())).unwrap();
        assert!(json.starts_with(r#"{"type":"stats","host":"#));
    }

    #[test]
    fn device_msgs_parse() {
        let msg: DeviceMsg = serde_json::from_str(r#"{"type":"press","id":"lock"}"#).unwrap();
        assert!(matches!(msg, DeviceMsg::Press { id } if id == "lock"));

        let msg: DeviceMsg = serde_json::from_str(r#"{"type":"something_new"}"#).unwrap();
        assert!(matches!(msg, DeviceMsg::Unknown));

        let msg: DeviceMsg =
            serde_json::from_str(r#"{"type":"display","tabs":["controls"],"brightness":40,"flip":true}"#).unwrap();
        let DeviceMsg::Display(d) = msg else { panic!("not a display") };
        assert_eq!((d.brightness, d.flip, d.temp_hot), (40, true, 85));
        assert_eq!(d.tab_order(), ["controls", "system", "network", "storage", "battery"]);

        // Firmware before 0.10.0 doesn't report its settings.
        let msg: DeviceMsg = serde_json::from_str(r#"{"type":"hello","fw":"0.9.0","proto":6}"#).unwrap();
        assert!(matches!(msg, DeviceMsg::Hello { display: None, .. }));
    }

    #[test]
    fn display_sends_only_changes() {
        let msg = HostMsg::Display { change: DisplayChange::default(), stats_ms: Some(1000) };
        assert_eq!(serde_json::to_string(&msg).unwrap(), r#"{"type":"display","stats_ms":1000}"#);
        let change = DisplayChange { flip: Some(true), ..Default::default() };
        let msg = HostMsg::Display { change, stats_ms: None };
        assert_eq!(serde_json::to_string(&msg).unwrap(), r#"{"type":"display","flip":true}"#);
    }

    #[test]
    fn display_changes_validate() {
        let mut d = DisplaySettings::default();
        let mut change = DisplayChange { temp_warm: Some(60), ..Default::default() };
        change.merge(DisplayChange { temp_hot: Some(80), brightness: Some(50), ..Default::default() });
        d.apply(&change);
        assert!(d.validate().is_ok());
        assert_eq!((d.temp_warm, d.temp_hot, d.brightness), (60, 80, 50));

        let bad = |c: DisplayChange| {
            let mut d = DisplaySettings::default();
            d.apply(&c);
            d.validate().is_err()
        };
        assert!(bad(DisplayChange { brightness: Some(2), ..Default::default() }));
        assert!(bad(DisplayChange { start: Some("nope".into()), ..Default::default() }));
        assert!(bad(DisplayChange { temp_warm: Some(90), ..Default::default() }));
        assert!(bad(DisplayChange { tabs: Some(vec!["system".into(), "system".into()]), ..Default::default() }));
        assert!(serde_json::from_str::<DisplayChange>(r#"{"stats_ms":5}"#).is_err());
    }
}
