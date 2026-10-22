//! The GPU for the System tab: NVIDIA through `nvidia-smi`, AMD through sysfs.
//!
//! Laptops with switchable graphics power the discrete GPU down (runtime D3)
//! when nothing uses it, and asking it anything wakes it up: running
//! `nvidia-smi` opens /dev/nvidia*, and reading amdgpu's counters resumes the
//! device. So the GPU is found and watched through its PCI device's power
//! state in sysfs, which never wakes it, and it's only asked for numbers while
//! it's awake anyway:
//!
//! - Suspended: nothing but sysfs is touched, and it's reported asleep.
//! - Just woken: it has to stay awake for DEBOUNCE polls first, so something
//!   that wakes it for a moment doesn't have us keep it up.
//! - Awake and busy: asked every POLL.
//! - Awake and idle: asked only every IDLE_POLL, which is longer than the
//!   driver waits before suspending an idle GPU, so our own queries never keep
//!   it awake.
//!
//! Each query is a fresh `nvidia-smi` run, so no handle on the GPU outlives
//! it. Polling happens on a thread of its own, so the stats loop never waits.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::protocol::GpuStats;

/// How often the power state is read.
const POLL: Duration = Duration::from_secs(3);
/// Polls in a row the GPU has to be awake before it's asked anything.
const DEBOUNCE: u32 = 2;
/// Between queries while it's idle. NVIDIA's driver suspends a GPU that has
/// been idle for about 20 s; asking any sooner would restart that wait.
const IDLE_POLL: Duration = Duration::from_secs(30);
/// Failed queries in a row before giving up on the GPU.
const MAX_FAILURES: u32 = 3;

const PCI_DEVICES: &str = "/sys/bus/pci/devices";
const NVIDIA: &str = "0x10de";
const AMD: &str = "0x1002";

#[derive(Debug, Clone, Copy, PartialEq)]
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
        let (kind, device) = find_device(Path::new(PCI_DEVICES), on_path("nvidia-smi"))?;
        log::info!("watching the GPU at {}", device.display());
        let latest = Arc::new(Mutex::new(Some(GpuStats { asleep: true, ..Default::default() })));
        let shared = latest.clone();
        thread::Builder::new()
            .name("gpu".into())
            .spawn(move || watch(kind, &device, &shared))
            .ok()?;
        Some(Self { latest })
    }

    pub fn latest(&self) -> Option<GpuStats> {
        self.latest.lock().unwrap().clone()
    }
}

fn watch(kind: Kind, device: &Path, latest: &Mutex<Option<GpuStats>>) {
    let mut poller = Poller::default();
    loop {
        let now = Instant::now();
        match poller.poll(awake(device), now) {
            Step::Asleep => *latest.lock().unwrap() = Some(GpuStats { asleep: true, ..Default::default() }),
            Step::Wait => {}
            Step::Query => {
                let reading = match kind {
                    Kind::Nvidia => nvidia(),
                    Kind::Amd => amd(device),
                };
                log::debug!("asked the GPU: {reading:?}");
                if !poller.answered(reading.as_ref().and_then(|r| r.usage), Instant::now()) {
                    log::info!("the GPU stopped answering; showing the busiest program instead");
                    *latest.lock().unwrap() = None;
                    return;
                }
                if let Some(reading) = reading {
                    *latest.lock().unwrap() = Some(reading);
                }
            }
        }
        thread::sleep(POLL);
    }
}

/// What to do on a poll.
#[derive(Debug, PartialEq)]
enum Step {
    /// Suspended, or not awake for long: report it asleep.
    Asleep,
    /// Awake, but not due a query: keep the last reading.
    Wait,
    Query,
}

/// When to ask the GPU, from its power state; kept apart from the I/O so the
/// rules can be tested.
#[derive(Default)]
struct Poller {
    awake_polls: u32,
    /// Since the last reading; None until there's one for this wake.
    next_query: Option<Instant>,
    failures: u32,
}

impl Poller {
    fn poll(&mut self, awake: bool, now: Instant) -> Step {
        if !awake {
            self.awake_polls = 0;
            self.next_query = None;
            return Step::Asleep;
        }
        self.awake_polls = self.awake_polls.saturating_add(1);
        if self.awake_polls < DEBOUNCE {
            return Step::Asleep;
        }
        match self.next_query {
            Some(at) if now < at => Step::Wait,
            _ => Step::Query,
        }
    }

    /// After a query: its usage, or None if it failed. Returns false once it
    /// has failed MAX_FAILURES times in a row.
    fn answered(&mut self, usage: Option<u8>, now: Instant) -> bool {
        let busy = match usage {
            Some(usage) => {
                self.failures = 0;
                usage > 0
            }
            None => {
                self.failures += 1;
                false
            }
        };
        self.next_query = Some(now + if busy { POLL } else { IDLE_POLL });
        self.failures < MAX_FAILURES
    }
}

/// The first NVIDIA GPU (when `nvidia-smi` is there to ask it) or AMD GPU with
/// a usage counter, by vendor and PCI class (VGA 0x0300xx, 3D 0x0302xx).
fn find_device(pci: &Path, has_smi: bool) -> Option<(Kind, PathBuf)> {
    let mut devices: Vec<PathBuf> = fs::read_dir(pci).ok()?.flatten().map(|e| e.path()).collect();
    devices.sort();
    let read = |d: &Path, name: &str| fs::read_to_string(d.join(name)).unwrap_or_default().trim().to_owned();
    let gpu = |d: &&PathBuf, vendor: &str| {
        let class = read(d, "class");
        read(d, "vendor") == vendor && (class.starts_with("0x0300") || class.starts_with("0x0302"))
    };
    if has_smi {
        if let Some(d) = devices.iter().find(|d| gpu(d, NVIDIA)) {
            return Some((Kind::Nvidia, d.clone()));
        }
    }
    devices
        .iter()
        .find(|d| gpu(d, AMD) && d.join("gpu_busy_percent").exists())
        .map(|d| (Kind::Amd, d.clone()))
}

/// Whether a program is on PATH, without running it.
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// The PCI device's runtime power state is "active"; reading it doesn't wake it.
/// A device without runtime power management is always awake.
fn awake(device: &Path) -> bool {
    match fs::read_to_string(device.join("power/runtime_status")) {
        Ok(status) => status.trim() == "active",
        Err(_) => true,
    }
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

    #[test]
    fn leaves_a_sleeping_or_just_woken_gpu_alone() {
        let mut p = Poller::default();
        let t = Instant::now();
        assert_eq!(p.poll(false, t), Step::Asleep);
        // Woken for a moment: not asked.
        assert_eq!(p.poll(true, t), Step::Asleep);
        assert_eq!(p.poll(false, t + POLL), Step::Asleep);
        assert_eq!(p.poll(true, t + POLL * 2), Step::Asleep);
        // Awake on DEBOUNCE polls in a row: asked.
        assert_eq!(p.poll(true, t + POLL * 3), Step::Query);
    }

    #[test]
    fn asks_an_idle_gpu_rarely() {
        let mut p = Poller::default();
        let t = Instant::now();
        p.poll(true, t);
        assert_eq!(p.poll(true, t), Step::Query);
        // Busy: asked again on the next poll.
        assert!(p.answered(Some(40), t));
        assert_eq!(p.poll(true, t + POLL), Step::Query);
        // Idle: left long enough to suspend.
        assert!(p.answered(Some(0), t + POLL));
        assert_eq!(p.poll(true, t + POLL * 2), Step::Wait);
        assert_eq!(p.poll(true, t + POLL + IDLE_POLL), Step::Query);
        // Suspending starts it over.
        assert_eq!(p.poll(false, t + IDLE_POLL * 2), Step::Asleep);
        assert_eq!(p.poll(true, t + IDLE_POLL * 3), Step::Asleep);
    }

    #[test]
    fn gives_up_after_repeated_failures() {
        let mut p = Poller::default();
        let t = Instant::now();
        assert!(p.answered(None, t));
        assert!(p.answered(Some(0), t));
        for _ in 1..MAX_FAILURES {
            assert!(p.answered(None, t));
        }
        assert!(!p.answered(None, t));
    }

    #[test]
    fn finds_gpus_by_vendor_and_class() {
        let dir = std::env::temp_dir().join(format!("nerd-deck-gpu-test-{}", std::process::id()));
        let device = |name: &str, vendor: &str, class: &str| {
            let d = dir.join(name);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("vendor"), format!("{vendor}\n")).unwrap();
            fs::write(d.join("class"), format!("{class}\n")).unwrap();
            d
        };
        device("0000:00:02.0", "0x8086", "0x030000"); // Intel graphics
        device("0000:00:1f.3", NVIDIA, "0x040300"); // NVIDIA audio, not a GPU
        let nv = device("0000:01:00.0", NVIDIA, "0x030200");
        let amd = device("0000:03:00.0", AMD, "0x030000");
        fs::write(amd.join("gpu_busy_percent"), "0\n").unwrap();

        assert_eq!(find_device(&dir, true), Some((Kind::Nvidia, nv)));
        // Without nvidia-smi there's no asking the NVIDIA one.
        assert_eq!(find_device(&dir, false), Some((Kind::Amd, amd)));
        fs::remove_dir_all(&dir).unwrap();
    }
}
