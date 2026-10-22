use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use sysinfo::{Components, Disks, Networks, System};

use crate::battery;
use crate::mounts;
use crate::protocol::{CpuStats, DiskStats, MemStats, NetStats, Stats};

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
    /// Set by the mount watcher; the disk list is rescanned only then.
    mounts_changed: Arc<AtomicBool>,
    last_sample: Instant,
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
            mounts_changed: mounts::watch(),
            last_sample: Instant::now(),
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
            disks: self.disks(),
            battery: if self.battery { battery::read() } else { None },
        }
    }

    fn cpu(&self) -> CpuStats {
        let cpus = self.sys.cpus();
        CpuStats {
            usage: self.sys.global_cpu_usage(),
            cores: cpus.iter().map(|c| c.cpu_usage()).collect(),
            freq_mhz: cpus.first().map(|c| c.frequency()).unwrap_or(0),
            temp: self.cpu_temp(),
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

    fn disks(&self) -> Vec<DiskStats> {
        let stats = |mount: String, d: &sysinfo::Disk| DiskStats {
            mount,
            used: d.total_space() - d.available_space(),
            total: d.total_space(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automount_dirs() {
        assert!(is_automount(Path::new("/media/jamie/11FA-2B9C")));
        assert!(is_automount(Path::new("/run/media/jamie/Gaming")));
        assert!(!is_automount(Path::new("/media")));
        assert!(!is_automount(Path::new("/mediafiles")));
        assert!(!is_automount(Path::new("/home")));
    }
}
