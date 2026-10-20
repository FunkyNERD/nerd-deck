//! Wire format shared with the firmware: one JSON object per line, tagged by
//! `type`. Keep in sync with docs/PROTOCOL.md and firmware/src/protocol.cpp.

use serde::{Deserialize, Serialize};

/// Bump together with `kProtoVersion` in firmware/src/config.hpp.
pub const PROTO_VERSION: u32 = 1;

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
}

/// Messages the device sends to the host.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceMsg {
    Hello { fw: String, proto: u32 },
    Press { id: String },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct ButtonDef {
    pub id: String,
    pub label: String,
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
