//! The GPU for the System tab: NVIDIA through `nvidia-smi`, AMD through sysfs.
//!
//! Laptops with switchable graphics power the discrete GPU down when nothing
//! uses it, and asking it anything wakes it up. So a powered-down GPU is left
//! alone and reported as asleep, and an awake one is only asked every few
//! seconds, from a thread of its own, so the stats loop never waits on it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::protocol::GpuStats;

const POLL: Duration = Duration::from_secs(3);

enum Kind {
    Nvidia,
    Amd,
}

pub struct Gpu {
    /// The latest reading; None once the GPU turns out to be unreadable.
    latest: Arc<Mutex<Option<GpuStats>>>,
}

impl Gpu {
    /// The first discrete GPU found, or None (Intel's built-in GPUs have no
    /// usage figure without root).
    pub fn find() -> Option<Self> {
        let (kind, device) = find_device()?;
        let latest = Arc::new(Mutex::new(Some(GpuStats { asleep: true, ..Default::default() })));
        let shared = latest.clone();
        thread::Builder::new()
            .name("gpu".into())
            .spawn(move || loop {
                let reading = if asleep(&device) {
                    Some(GpuStats { asleep: true, ..Default::default() })
                } else {
                    match kind {
                        Kind::Nvidia => nvidia(),
                        Kind::Amd => amd(&device),
                    }
                };
                let stop = reading.is_none();
                *shared.lock().unwrap() = reading;
                if stop {
                    log::info!("the GPU stopped answering; showing the busiest process instead");
                    return;
                }
                thread::sleep(POLL);
            })
            .ok()?;
        Some(Self { latest })
    }

    pub fn latest(&self) -> Option<GpuStats> {
        self.latest.lock().unwrap().clone()
    }
}

fn find_device() -> Option<(Kind, PathBuf)> {
    let mut cards: Vec<PathBuf> = fs::read_dir("/sys/class/drm")
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name.starts_with("card") && name[4..].chars().all(|c| c.is_ascii_digit())
        })
        .collect();
    cards.sort();
    let devices: Vec<PathBuf> = cards.iter().filter_map(|c| c.join("device").canonicalize().ok()).collect();
    let vendor = |d: &Path| fs::read_to_string(d.join("vendor")).unwrap_or_default().trim().to_owned();
    if let Some(d) = devices.iter().find(|d| vendor(d) == "0x10de") {
        let has_smi = Command::new("nvidia-smi").arg("--version").output().is_ok_and(|o| o.status.success());
        if has_smi {
            return Some((Kind::Nvidia, d.clone()));
        }
    }
    devices
        .into_iter()
        .find(|d| vendor(d) == "0x1002" && d.join("gpu_busy_percent").exists())
        .map(|d| (Kind::Amd, d))
}

fn asleep(device: &Path) -> bool {
    fs::read_to_string(device.join("power/runtime_status")).is_ok_and(|s| s.trim() == "suspended")
}

fn nvidia() -> Option<GpuStats> {
    let out = Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu,temperature.gpu,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    parse_nvidia(&String::from_utf8_lossy(&out.stdout))
}

/// The first line of `nvidia-smi`'s CSV: usage %, °C, then VRAM in MiB.
fn parse_nvidia(csv: &str) -> Option<GpuStats> {
    let fields: Vec<&str> = csv.lines().next()?.split(',').map(str::trim).collect();
    let num = |i: usize| fields.get(i).and_then(|f| f.parse::<f64>().ok());
    Some(GpuStats {
        asleep: false,
        usage: Some(num(0)?.clamp(0.0, 100.0) as u8),
        temp: num(1).map(|t| t as f32),
        vram_used: num(2).map(|m| (m * 1024.0 * 1024.0) as u64),
        vram_total: num(3).map(|m| (m * 1024.0 * 1024.0) as u64),
    })
}

fn amd(device: &Path) -> Option<GpuStats> {
    let read = |name: &str| fs::read_to_string(device.join(name)).ok()?.trim().parse::<u64>().ok();
    let temp = fs::read_dir(device.join("hwmon"))
        .ok()
        .and_then(|mut d| d.next())
        .and_then(|e| e.ok())
        .and_then(|e| fs::read_to_string(e.path().join("temp1_input")).ok())
        .and_then(|t| t.trim().parse::<f32>().ok())
        .map(|millis| millis / 1000.0);
    Some(GpuStats {
        asleep: false,
        usage: Some(read("gpu_busy_percent")?.min(100) as u8),
        temp,
        vram_used: read("mem_info_vram_used"),
        vram_total: read("mem_info_vram_total"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nvidia_smi() {
        let g = parse_nvidia("41, 62, 3174, 4096\n").unwrap();
        assert_eq!(g.usage, Some(41));
        assert_eq!(g.temp, Some(62.0));
        assert_eq!(g.vram_total, Some(4096 * 1024 * 1024));
        // Some GPUs report [N/A] for a field.
        let g = parse_nvidia("12, [N/A], 100, 2000").unwrap();
        assert_eq!(g.temp, None);
        assert!(parse_nvidia("").is_none());
    }
}
