use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use sysinfo::{Components, Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::battery;
use crate::gpu::Gpu;
use crate::mounts;
use crate::power_profile::PowerProfile;
use crate::protocol::{CpuStats, DiskStats, MemStats, NetStats, Stats, TopProcess};

/// The device shows at most this many disks.
const MAX_DISKS: usize = 4;

/// Sensor labels to prefer for CPU temperature, in order.
const CPU_TEMP_LABELS: &[&str] = &["Package id 0", "Tctl", "Tdie", "k10temp", "coretemp", "cpu"];

pub struct Collector {
    sys: System,
    networks: Networks,
    disks: Disks,
    components: Components,
    interface: Option<String>,
    mounts: Vec<String>,
    removable: bool,
    battery: bool,
    /// Smoothed battery power and the state it's for, so the time-left
    /// estimate doesn't jump around.
    battery_watts: Option<(&'static str, f32)>,
    /// Set by the mount watcher; the disk list is rescanned only then.
    mounts_changed: Arc<AtomicBool>,
    last_sample: Instant,
    /// Sectors read and written so far, by block device, from the last sample.
    disk_io: HashMap<String, (u64, u64)>,
    /// None on PCs without a readable GPU, which get the busiest process instead.
    gpu: Option<Gpu>,
    power_profile: PowerProfile,
}

impl Collector {
    pub fn new(interface: Option<String>, mounts: Vec<String>, removable: bool, battery: bool) -> Self {
        let mut sys = System::new();
        // Prime CPU counters; the first usage reading is relative to this.
        sys.refresh_cpu_all();
        Self {
            sys,
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
            components: Components::new_with_refreshed_list(),
            interface,
            mounts,
            removable,
            battery,
            battery_watts: None,
            mounts_changed: mounts::watch(),
            last_sample: Instant::now(),
            disk_io: read_diskstats(),
            gpu: Gpu::find(),
            power_profile: PowerProfile::new(),
        }
    }

    pub fn sample(&mut self) -> Stats {
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();
        self.networks.refresh();
        if self.mounts_changed.swap(false, Ordering::Relaxed) {
            self.disks.refresh_list();
        } else {
            self.disks.refresh();
        }
        self.components.refresh();

        let elapsed = self.last_sample.elapsed().as_secs_f64().max(0.001);
        self.last_sample = Instant::now();

        let load = System::load_average();
        let io = read_diskstats();
        let disks = self.disks(&io, elapsed);
        self.disk_io = io;
        let gpu = self.gpu.as_ref().and_then(Gpu::latest);
        let top = if gpu.is_none() { self.top_process() } else { None };

        Stats {
            host: System::host_name().unwrap_or_default(),
            uptime: System::uptime(),
            load: [load.one, load.five, load.fifteen],
            cpu: self.cpu(),
            mem: MemStats {
                used: self.sys.used_memory(),
                total: self.sys.total_memory(),
                swap_used: self.sys.used_swap(),
                swap_total: self.sys.total_swap(),
            },
            net: self.net(elapsed),
            disks,
            gpu,
            top,
            battery: if self.battery { self.battery() } else { None },
        }
    }

    fn battery(&mut self) -> Option<crate::protocol::BatteryStats> {
        let mut b = battery::read()?;
        let watts = b.watts.unwrap_or(0.0);
        // Start over when charging starts or stops.
        let smoothed = match self.battery_watts {
            Some((state, avg)) if state == b.state => avg + (watts - avg) * 0.1,
            _ => watts,
        };
        self.battery_watts = Some((b.state, smoothed));
        b.minutes = battery::minutes_left(&b, smoothed);
        Some(b)
    }

    fn cpu(&self) -> CpuStats {
        let cpus = self.sys.cpus();
        CpuStats {
            usage: self.sys.global_cpu_usage(),
            cores: cpus.iter().map(|c| c.cpu_usage()).collect(),
            freq_mhz: cpus.first().map(|c| c.frequency()).unwrap_or(0),
            temp: self.cpu_temp(),
            profile: self.power_profile.current(),
        }
    }

    fn cpu_temp(&self) -> Option<f32> {
        CPU_TEMP_LABELS.iter().find_map(|want| {
            self.components
                .iter()
                .find(|c| c.label().contains(want))
                .map(|c| c.temperature())
                .filter(|t| t.is_finite())
        })
    }

    fn net(&self, elapsed: f64) -> NetStats {
        let chosen = match &self.interface {
            Some(name) => self.networks.iter().find(|(n, _)| *n == name),
            None => self
                .networks
                .iter()
                .filter(|(n, _)| n.as_str() != "lo")
                .max_by_key(|(_, d)| d.total_received() + d.total_transmitted()),
        };

        match chosen {
            Some((name, data)) => NetStats {
                iface: name.clone(),
                // received()/transmitted() are deltas since the previous refresh.
                rx_bps: (data.received() as f64 / elapsed) as u64,
                tx_bps: (data.transmitted() as f64 / elapsed) as u64,
            },
            None => NetStats::default(),
        }
    }

    /// The program using the most CPU, its processes counted together.
    fn top_process(&mut self) -> Option<TopProcess> {
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new().with_cpu().with_memory().with_exe(UpdateKind::OnlyIfNotSet),
        );
        let cores = self.sys.cpus().len().max(1) as f32;
        // Not nerd-deck itself: the service, the tray and the settings window
        // are all this program.
        let ours = own_program();
        let mut by_name: HashMap<String, (f32, u64)> = HashMap::new();
        for p in self.sys.processes().values() {
            // Threads show up as processes too; count each once.
            if p.thread_kind().is_some() {
                continue;
            }
            // By program: the kernel cuts names to 15 characters, and many
            // programs name their helpers ("Isolated Web Co" is firefox).
            let name = p
                .exe()
                .and_then(Path::file_name)
                .unwrap_or(p.name())
                .to_string_lossy()
                // A program replaced while it runs, say by an upgrade.
                .trim_end_matches(" (deleted)")
                .to_owned();
            if name == ours {
                continue;
            }
            let e = by_name.entry(name).or_default();
            e.0 += p.cpu_usage();
            e.1 += p.memory();
        }
        by_name
            .into_iter()
            .max_by(|a, b| a.1 .0.total_cmp(&b.1 .0))
            .map(|(name, (cpu, mem))| TopProcess { name, cpu: (cpu / cores).min(100.0), mem })
    }

    fn disks(&self, io: &HashMap<String, (u64, u64)>, elapsed: f64) -> Vec<DiskStats> {
        let stats = |mount: String, d: &sysinfo::Disk| {
            // Rates from the block device's counters; a device that's new
            // since the last sample starts at zero.
            let (read_bps, write_bps) = block_device(d)
                .and_then(|dev| Some((io.get(&dev)?, self.disk_io.get(&dev)?)))
                .map(|(now, before)| {
                    let rate = |n: u64, b: u64| (n.saturating_sub(b) as f64 * 512.0 / elapsed) as u64;
                    (rate(now.0, before.0), rate(now.1, before.1))
                })
                .unwrap_or((0, 0));
            DiskStats {
                mount,
                used: d.total_space() - d.available_space(),
                total: d.total_space(),
                removable: is_removable(d),
                read_bps,
                write_bps,
            }
        };

        let mut out: Vec<DiskStats> = self
            .mounts
            .iter()
            .filter_map(|m| {
                let d = self.disks.iter().find(|d| d.mount_point() == Path::new(m))?;
                let label = if m == "/" { "Root".to_owned() } else { m.clone() };
                Some(stats(label, d))
            })
            .collect();

        if self.removable {
            let mut removable: Vec<&sysinfo::Disk> = self
                .disks
                .iter()
                .filter(|d| is_removable(d))
                .filter(|d| !self.mounts.iter().any(|m| d.mount_point() == Path::new(m)))
                .collect();
            removable.sort_by_key(|d| d.mount_point());
            removable.dedup_by_key(|d| d.mount_point());
            // Label by the last path component ("Gaming", not /media/jamie/Gaming)
            // so it fits the device's disk row.
            out.extend(removable.into_iter().map(|d| {
                let path = d.mount_point();
                let label = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                stats(label, d)
            }));
        }

        out.truncate(MAX_DISKS);
        out
    }
}

/// Desktops auto-mount hot-plugged drives under these (udisks2: /media/$USER on
/// Debian/Ubuntu, /run/media/$USER elsewhere).
const AUTOMOUNT_DIRS: &[&str] = &["/media", "/run/media"];

/// USB drives (sysinfo checks /dev/disk/by-id for `usb-` links), plus anything
/// the desktop auto-mounted. The second catches drives sysinfo misses, like SD
/// cards in a built-in reader, whose kernel removable flag is 0.
pub fn is_removable(d: &sysinfo::Disk) -> bool {
    d.is_removable() || is_automount(d.mount_point())
}

fn is_automount(mount: &Path) -> bool {
    AUTOMOUNT_DIRS.iter().any(|dir| mount.starts_with(dir) && mount != Path::new(dir))
}

/// Sectors read and written so far, by block device ("nvme0n1p2", "dm-0").
fn read_diskstats() -> HashMap<String, (u64, u64)> {
    parse_diskstats(&std::fs::read_to_string("/proc/diskstats").unwrap_or_default())
}

fn parse_diskstats(text: &str) -> HashMap<String, (u64, u64)> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            Some((f.get(2)?.to_string(), (f.get(5)?.parse().ok()?, f.get(9)?.parse().ok()?)))
        })
        .collect()
}

/// The block device a disk lives on: /dev/mapper/root resolves to dm-0.
fn block_device(d: &sysinfo::Disk) -> Option<String> {
    let path = std::fs::canonicalize(d.name()).ok()?;
    Some(path.file_name()?.to_str()?.to_owned())
}

/// This program's file name, as the busiest-process count names programs.
fn own_program() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_name().map(|n| n.to_string_lossy().into_owned()))
        // Replaced while running, the link reads "nerd-deck (deleted)".
        .map(|n| n.trim_end_matches(" (deleted)").to_owned())
        .unwrap_or_else(|| env!("CARGO_PKG_NAME").to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_diskstats() {
        let text = " 259       2 nvme0n1p2 4211 120 1000 300 900 40 2000 500 0 700 800\n";
        assert_eq!(parse_diskstats(text)["nvme0n1p2"], (1000, 2000));
    }

    #[test]
    fn automount_dirs() {
        assert!(is_automount(Path::new("/media/jamie/11FA-2B9C")));
        assert!(is_automount(Path::new("/run/media/jamie/Gaming")));
        assert!(!is_automount(Path::new("/media")));
        assert!(!is_automount(Path::new("/mediafiles")));
        assert!(!is_automount(Path::new("/home")));
    }
}
