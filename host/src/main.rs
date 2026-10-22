mod actions;
mod battery;
mod collector;
mod config;
mod device;
mod icons;
mod images;
mod mounts;
mod protocol;

use std::path::PathBuf;
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use base64::Engine;
use clap::Parser;

use collector::Collector;
use config::Config;
use device::Connection;
use protocol::{ButtonDef, DeviceMsg, HostMsg, PROTO_VERSION};

const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// Decoded bytes per `image` message; base64 keeps each line well under the
/// device's 2 KB limit.
const IMAGE_CHUNK: usize = 1024;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Config file (default: ~/.config/nerd-deck/config.toml)
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// List serial ports and exit
    #[arg(long)]
    list_ports: bool,

    /// Print one stats sample as JSON and exit
    #[arg(long)]
    dump: bool,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();

    if cli.list_ports {
        return device::list_ports();
    }

    let cfg = Config::load(cli.config.as_deref())?;
    let mut collector = Collector::new(
        cfg.stats.interface.clone(),
        cfg.stats.mounts.clone(),
        cfg.stats.removable,
    );

    if cli.dump {
        thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        println!("{}", serde_json::to_string_pretty(&HostMsg::Stats(collector.sample()))?);
        return Ok(());
    }

    loop {
        if let Err(e) = run_session(&cfg, &mut collector) {
            log::warn!("{e:#}; retrying in {}s", RECONNECT_DELAY.as_secs());
        }
        thread::sleep(RECONNECT_DELAY);
    }
}

/// Connect, then stream stats and handle button presses until the link drops.
fn run_session(cfg: &Config, collector: &mut Collector) -> Result<()> {
    let path = device::find_port(&cfg.device)?;
    let mut conn = Connection::open(&path, cfg.device.baud)?;
    log::info!("connected to {path}");

    conn.send(&HostMsg::Hello {
        proto: PROTO_VERSION,
        host: sysinfo::System::host_name().unwrap_or_default(),
    })?;
    send_buttons(cfg, &mut conn)?;

    let interval = Duration::from_millis(cfg.stats.interval_ms);
    let mut next_stats = Instant::now();

    loop {
        if Instant::now() >= next_stats {
            conn.send(&HostMsg::Stats(collector.sample()))?;
            next_stats += interval;
        }

        match conn.rx.recv_timeout(next_stats.saturating_duration_since(Instant::now())) {
            Ok(msg) => handle(cfg, &mut conn, msg)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("device disconnected"),
        }
    }
}

fn handle(cfg: &Config, conn: &mut Connection, msg: DeviceMsg) -> Result<()> {
    match msg {
        DeviceMsg::Hello { fw, proto } => {
            log::info!("device firmware {fw}, protocol v{proto}");
            if proto != PROTO_VERSION {
                log::warn!("protocol mismatch: host v{PROTO_VERSION}, device v{proto}");
            }
            // Device may have rebooted; resend its layout.
            send_buttons(cfg, conn)?;
        }
        DeviceMsg::Press { id } => {
            let (ok, msg) = match cfg.button(&id) {
                Some(button) => match actions::run(button) {
                    Ok(()) => (true, None),
                    Err(e) => {
                        log::error!("{e:#}");
                        (false, Some(e.to_string()))
                    }
                },
                None => {
                    log::warn!("press for unknown button {id:?}");
                    (false, Some("unknown button".into()))
                }
            };
            conn.send(&HostMsg::Ack { id, ok, msg })?;
        }
        DeviceMsg::Unknown => log::debug!("ignoring unknown device message"),
    }
    Ok(())
}

fn send_buttons(cfg: &Config, conn: &mut Connection) -> Result<()> {
    let buttons = cfg
        .buttons
        .iter()
        .map(|b| ButtonDef {
            id: b.id.clone(),
            label: b.label.clone(),
            icon: b.icon.as_deref().and_then(icons::lookup).map(String::from),
            image: b.image_data.is_some(),
        })
        .collect();
    conn.send(&HostMsg::Buttons { buttons })?;

    for b in &cfg.buttons {
        let Some(img) = &b.image_data else { continue };
        for (i, chunk) in img.data.chunks(IMAGE_CHUNK).enumerate() {
            conn.send(&HostMsg::Image {
                id: b.id.clone(),
                w: img.width,
                h: img.height,
                fmt: images::FORMAT,
                offset: i * IMAGE_CHUNK,
                total: img.data.len(),
                data: base64::engine::general_purpose::STANDARD.encode(chunk),
            })?;
        }
    }
    Ok(())
}
