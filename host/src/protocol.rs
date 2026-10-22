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
    /// Settings the deck keeps: tab order, the tab it opens on, brightness and
    /// power saving. Added in v6.
    Display {
        tabs: Vec<&'static str>,
        /// A tab's name, or "first".
        start: String,
        /// Percent.
        brightness: u8,
        /// Seconds untouched before dimming; 0 never.
        dim_after: u32,
        /// Seconds on the waiting screen before the screen turns off; 0 never.
        off_after: u32,
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
    },
    Press { id: String },
    /// An image finished arriving (`ok`) or was dropped after a lost chunk.
    /// Added in v5; the host waits for it before sending the next image.
    Image { id: String, ok: bool },
    #[serde(other)]
    Unknown,
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
    }
}
