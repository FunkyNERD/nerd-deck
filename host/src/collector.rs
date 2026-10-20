use std::path::Path;
use std::time::Instant;

use sysinfo::{Components, Disks, Networks, System};

use crate::protocol::{CpuStats, DiskStats, MemStats, NetStats, Stats};

/// Sensor labels to prefer for CPU temperature, in order.
const CPU_TEMP_LABELS: &[&str] = &["Package id 0", "Tctl", "Tdie", "k10temp", "coretemp", "cpu"];

pub struct Collector {
    sys: System,
    networks: Networks,
    disks: Disks,
    components: Components,
    interface: Option<String>,
    mounts: Vec<String>,
    last_sample: Instant,
}

impl Collector {
    pub fn new(interface: Option<String>, mounts: Vec<String>) -> Self {
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
            last_sample: Instant::now(),
        }
    }

    pub fn sample(&mut self) -> Stats {
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();
        self.networks.refresh();
        self.disks.refresh();
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
        self.mounts
            .iter()
            .filter_map(|m| {
                let d = self.disks.iter().find(|d| d.mount_point() == Path::new(m))?;
                Some(DiskStats {
                    mount: m.clone(),
                    used: d.total_space() - d.available_space(),
                    total: d.total_space(),
                })
            })
            .collect()
    }
}
