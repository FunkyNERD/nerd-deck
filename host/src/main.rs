mod actions;
mod battery;
mod collector;
mod config;
mod device;
mod firmware;
mod icons;
mod images;
mod mounts;
mod notifications;
mod protocol;
mod updates;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::Engine;
use clap::Parser;

use collector::Collector;
use config::Config;
use device::Connection;
use protocol::{ButtonDef, DeviceMsg, HostMsg, PROTO_VERSION};

const RECONNECT_DELAY: Duration = Duration::from_secs(2);
/// The device says hello on boot and in reply to ours, well within this.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

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

    /// Install the bundled device firmware now, whatever the device runs, and
    /// exit. Stop the service first; it holds the port.
    #[arg(long)]
    flash_firmware: bool,
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

    if cli.flash_firmware {
        let fw = firmware::bundled().context("this build has no bundled firmware")?;
        let port = device::find_port(&cfg.device)?;
        log::info!("installing firmware {} on {}", fw.version, port.name);
        return firmware::flash(&fw, &port.name, usb_info(&cfg, &port));
    }

    let updates = cfg.updates.check.then(updates::watch);
    let notifications =
        cfg.notifications.enabled.then(|| notifications::watch(cfg.notifications.clone()));
    let mut flashing = firmware::Flashing::new(&cfg);
    loop {
        let session = run_session(
            &cfg,
            &mut collector,
            updates.as_ref(),
            notifications.as_ref(),
            &mut flashing,
        );
        match session {
            Ok(SessionEnd::Flash(port)) => flashing.flash(&port, usb_info(&cfg, &port)),
            Err(e) => log::warn!("{e:#}; retrying in {}s", RECONNECT_DELAY.as_secs()),
        }
        thread::sleep(RECONNECT_DELAY);
    }
}

/// Why a session ended other than by an error.
enum SessionEnd {
    /// The port is closed; install the bundled firmware on this device.
    Flash(device::Port),
}

/// Connect, then stream stats and handle button presses until the link drops.
fn run_session(
    cfg: &Config,
    collector: &mut Collector,
    updates: Option<&updates::Shared>,
    notifications: Option<&Receiver<notifications::Notification>>,
    flashing: &mut firmware::Flashing,
) -> Result<SessionEnd> {
    // Drop any that came in while the device was away; they're old news.
    if let Some(rx) = notifications {
        rx.try_iter().for_each(drop);
    }

    let port = device::find_port(&cfg.device)?;
    let mut conn = Connection::open(&port.name, cfg.device.baud)?;
    log::info!("connected to {}", port.name);
    let opened = Instant::now();

    conn.send(&HostMsg::Hello {
        proto: PROTO_VERSION,
        host: sysinfo::System::host_name().unwrap_or_default(),
    })?;
    send_buttons(cfg, &mut conn)?;

    let interval = Duration::from_millis(cfg.stats.interval_ms);
    let mut next_stats = Instant::now();
    // The update check's `seq` last shown on the device; 0 = none this session.
    let mut notified = 0;
    let mut said_hello = false;

    loop {
        let available = updates.and_then(|u| u.lock().unwrap().clone());
        if let Some(a) = available.filter(|a| a.seq != notified) {
            let text = format!("Update available: nerd-deck {}", a.version);
            conn.send(&HostMsg::Notify { text })?;
            notified = a.seq;
        }

        for n in notifications.into_iter().flat_map(|rx| rx.try_iter()) {
            conn.send(&HostMsg::Notification {
                app: n.app,
                summary: n.summary,
                body: n.body,
                critical: n.critical,
            })?;
        }

        // A device that never says hello may be stuck after an interrupted flash.
        if !said_hello && opened.elapsed() >= HELLO_TIMEOUT {
            said_hello = true; // only decide once per session
            if flashing.should_recover(&port) {
                return Ok(SessionEnd::Flash(port));
            }
            log::warn!("no hello from the device after {}s", HELLO_TIMEOUT.as_secs());
        }

        if Instant::now() >= next_stats {
            conn.send(&HostMsg::Stats(collector.sample()))?;
            next_stats += interval;
        }

        match conn.rx.recv_timeout(next_stats.saturating_duration_since(Instant::now())) {
            Ok(msg) => {
                if let DeviceMsg::Hello { fw, board, .. } = &msg {
                    said_hello = true;
                    // The device rebooted, so it lost any notice.
                    notified = 0;
                    if let Some(version) = flashing.should_update(&port, fw, board.as_deref()) {
                        let text = format!("Updating firmware to {version}...");
                        conn.send(&HostMsg::Notify { text })?;
                        // Leave the notice up briefly; the screen freezes while flashing.
                        thread::sleep(Duration::from_secs(2));
                        return Ok(SessionEnd::Flash(port));
                    }
                }
                handle(cfg, &mut conn, msg)?
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("device disconnected"),
        }
    }
}

/// espflash picks its reset sequence from the USB PID, so a port with no USB
/// info (an explicit `port` that isn't USB) gets the configured IDs.
fn usb_info(cfg: &Config, port: &device::Port) -> serialport::UsbPortInfo {
    port.usb.clone().unwrap_or(serialport::UsbPortInfo {
        vid: cfg.device.vid,
        pid: cfg.device.pid.unwrap_or(0),
        serial_number: None,
        manufacturer: None,
        product: None,
    })
}

fn handle(cfg: &Config, conn: &mut Connection, msg: DeviceMsg) -> Result<()> {
    match msg {
        DeviceMsg::Hello { fw, proto, .. } => {
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
