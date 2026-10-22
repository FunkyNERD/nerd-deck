mod actions;
mod api;
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
mod settings;
mod tray;
mod updates;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::Engine;
use clap::Parser;

use api::Command;
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
/// Pause after each image chunk. The device reads serial between frames, so
/// a full-button image (~26 KB) sent flat out can overrun it while it draws.
const IMAGE_CHUNK_PAUSE: Duration = Duration::from_millis(5);
/// How long to wait for the device's result for an image (protocol v5).
const IMAGE_RESULT_TIMEOUT: Duration = Duration::from_secs(3);
/// Tries per image; the device drops one that loses a chunk.
const IMAGE_TRIES: usize = 3;

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

    /// Show the tray icon, which talks to the running service
    #[arg(long)]
    tray: bool,

    /// Open the settings window
    #[arg(long)]
    settings: bool,

    /// With --settings: the section to open on (device, buttons, stats,
    /// notifications, firmware, about)
    #[arg(long, requires = "settings")]
    page: Option<String>,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();

    if cli.list_ports {
        return device::list_ports();
    }
    if cli.tray {
        return tray::run();
    }
    if cli.settings {
        return settings::run(cli.config, cli.page.as_deref());
    }

    let cfg = Config::load(cli.config.as_deref())?;

    if cli.dump {
        let mut collector = new_collector(&cfg);
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

    Host::new(cli.config, cfg).run()
}

fn new_collector(cfg: &Config) -> Collector {
    Collector::new(
        cfg.stats.interface.clone(),
        cfg.stats.mounts.clone(),
        cfg.stats.removable,
        cfg.stats.battery,
    )
}

/// Why a session ended other than by an error.
enum SessionEnd {
    /// The port is closed; install the bundled firmware on this device.
    Flash(device::Port),
    /// The device settings changed; connect again with the new ones.
    Reconnect,
    /// Asked for over D-Bus: install the bundled firmware whatever the device runs.
    Reinstall(device::Port),
}

/// The service: streams stats to the deck, runs its buttons, and answers the
/// tray over D-Bus.
struct Host {
    /// From `--config`; None means the default location.
    config_path: Option<PathBuf>,
    cfg: Config,
    collector: Collector,
    /// Started once the config turns them on; a reload never stops them, but
    /// their results are only used while the config says so.
    updates: Option<updates::Shared>,
    notifications: Option<Receiver<notifications::Raw>>,
    seen_apps: notifications::SeenApps,
    flashing: firmware::Flashing,
    api: api::Service,
    commands: Receiver<Command>,
}

impl Host {
    fn new(config_path: Option<PathBuf>, cfg: Config) -> Self {
        let (tx, commands) = mpsc::channel();
        let status = api::Status {
            version: env!("CARGO_PKG_VERSION").into(),
            bundled: firmware::bundled().map(|fw| fw.version.to_owned()).unwrap_or_default(),
            config: config_path
                .clone()
                .or_else(Config::default_path)
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            ..Default::default()
        };
        let mut host = Self {
            config_path,
            collector: new_collector(&cfg),
            flashing: firmware::Flashing::new(&cfg),
            cfg,
            updates: None,
            notifications: None,
            seen_apps: notifications::SeenApps::load(),
            api: api::Service::start(status, tx),
            commands,
        };
        host.start_watchers();
        host
    }

    fn start_watchers(&mut self) {
        if self.cfg.updates.check && self.updates.is_none() {
            self.updates = Some(updates::watch());
        }
        if self.cfg.notifications.enabled && self.notifications.is_none() {
            self.notifications = Some(notifications::watch());
        }
    }

    fn run(mut self) -> Result<()> {
        loop {
            match self.run_session() {
                Ok(SessionEnd::Flash(port)) => {
                    self.flashing.flash(&port, usb_info(&self.cfg, &port));
                }
                Ok(SessionEnd::Reconnect) => {
                    self.api.update(disconnected);
                    continue;
                }
                Ok(SessionEnd::Reinstall(port)) => {
                    self.api.update(|s| {
                        disconnected(s);
                        s.flashing = true;
                    });
                    if let Some(fw) = firmware::bundled() {
                        log::info!("reinstalling firmware {} on {}", fw.version, port.name);
                        if let Err(e) = firmware::flash(&fw, &port.name, usb_info(&self.cfg, &port)) {
                            log::error!("firmware reinstall failed: {e:#}");
                        }
                    }
                    self.api.update(|s| s.flashing = false);
                }
                Err(e) => log::warn!("{e:#}; retrying in {}s", RECONNECT_DELAY.as_secs()),
            }
            self.api.update(disconnected);
            self.idle(RECONNECT_DELAY);
        }
    }

    /// Wait between connection attempts, still answering commands.
    fn idle(&mut self, wait: Duration) {
        let until = Instant::now() + wait;
        while let Ok(cmd) = self.commands.recv_timeout(until.saturating_duration_since(Instant::now()))
        {
            match cmd {
                Command::Reload(reply) => {
                    let _ = reply.send(self.reload().map(drop).map_err(|e| format!("{e:#}")));
                }
                Command::TestNotification { reply, .. } | Command::ReinstallFirmware(reply) => {
                    let _ = reply.send(Err("the deck isn't connected".into()));
                }
            }
        }
        // Notifications that came in meanwhile are old news by the time the
        // deck is back.
        if let Some(rx) = &self.notifications {
            rx.try_iter().for_each(drop);
        }
    }

    /// Read the config again and apply it. Returns whether the device settings
    /// changed, which needs a new connection.
    fn reload(&mut self) -> Result<bool> {
        let cfg = Config::load(self.config_path.as_deref())?;
        let device_changed = cfg.device != self.cfg.device;
        if cfg.stats != self.cfg.stats {
            self.collector = new_collector(&cfg);
        }
        if cfg.updates.firmware != self.cfg.updates.firmware {
            self.flashing = firmware::Flashing::new(&cfg);
        } else {
            // The settings window may have forgotten a device.
            self.flashing.reload_known();
        }
        self.cfg = cfg;
        self.start_watchers();
        log::info!("reloaded the config");
        Ok(device_changed)
    }

    /// Connect, then stream stats and handle button presses and commands until
    /// the link drops.
    fn run_session(&mut self) -> Result<SessionEnd> {
        let port = device::find_port(&self.cfg.device)?;
        let mut conn = Connection::open(&port.name, self.cfg.device.baud)?;
        log::info!("connected to {}", port.name);
        let opened = Instant::now();
        self.api.update(|s| {
            s.connected = true;
            s.port = port.name.clone();
            s.serial = port.serial().unwrap_or_default().to_owned();
        });

        // The device answers with its own hello, and the buttons follow that:
        // it says how big their images should be.
        conn.send(&HostMsg::Hello {
            proto: PROTO_VERSION,
            host: sysinfo::System::host_name().unwrap_or_default(),
        })?;
        let mut tile = None;

        let mut next_stats = Instant::now();
        // The update check's `seq` last shown on the device; 0 = none this session.
        let mut notified = 0;
        let mut said_hello = false;

        loop {
            let available = self.updates.as_ref().and_then(|u| u.lock().unwrap().clone());
            let available = available.filter(|_| self.cfg.updates.check);
            let update = available.as_ref().map(|a| a.version.clone()).unwrap_or_default();
            self.api.update(|s| s.update = update);
            if let Some(a) = available.filter(|a| a.seq != notified) {
                let text = format!("Update available: nerd-deck {}", a.version);
                conn.send(&HostMsg::Notify { text })?;
                notified = a.seq;
            }

            let incoming: Vec<_> = self.notifications.iter().flat_map(|rx| rx.try_iter()).collect();
            for raw in incoming {
                self.seen_apps.record(&raw);
                if !self.cfg.notifications.enabled {
                    continue;
                }
                if let Some(n) = notifications::filter(&self.cfg.notifications, raw) {
                    conn.send(&notification(n))?;
                }
            }

            while let Ok(cmd) = self.commands.try_recv() {
                match cmd {
                    Command::Reload(reply) => match self.reload() {
                        Ok(device_changed) => {
                            let _ = reply.send(Ok(()));
                            if device_changed {
                                return Ok(SessionEnd::Reconnect);
                            }
                            send_display(&self.cfg, &mut conn)?;
                            send_buttons(&self.cfg, &mut conn, tile)?;
                        }
                        Err(e) => {
                            let _ = reply.send(Err(format!("{e:#}")));
                        }
                    },
                    Command::TestNotification { critical, reply } => {
                        let sent = conn.send(&test_notification(critical));
                        let _ = reply.send(sent.as_ref().map_err(|e| e.to_string()).copied());
                        sent?;
                    }
                    Command::ReinstallFirmware(reply) => {
                        if firmware::bundled().is_none() {
                            let _ = reply.send(Err("this build has no bundled firmware".into()));
                            continue;
                        }
                        let _ = reply.send(Ok(()));
                        let text = "Reinstalling firmware...".to_owned();
                        conn.send(&HostMsg::Notify { text })?;
                        thread::sleep(Duration::from_secs(2));
                        return Ok(SessionEnd::Reinstall(port));
                    }
                }
            }

            // A device that never says hello may be stuck after an interrupted flash.
            if !said_hello && opened.elapsed() >= HELLO_TIMEOUT {
                said_hello = true; // only decide once per session
                if self.flashing.should_recover(&port) {
                    return Ok(SessionEnd::Flash(port));
                }
                log::warn!("no hello from the device after {}s", HELLO_TIMEOUT.as_secs());
            }

            if Instant::now() >= next_stats {
                conn.send(&HostMsg::Stats(self.collector.sample()))?;
                next_stats += Duration::from_millis(self.cfg.stats.interval_ms);
            }

            match conn.rx.recv_timeout(next_stats.saturating_duration_since(Instant::now())) {
                Ok(msg) => {
                    if let DeviceMsg::Hello { fw, board, proto, tile: t } = &msg {
                        said_hello = true;
                        tile = *t;
                        // The device rebooted, so it lost any notice.
                        notified = 0;
                        self.api.update(|s| {
                            s.firmware = fw.clone();
                            s.proto = *proto;
                            s.board = board.clone().unwrap_or_else(|| firmware::LEGACY_BOARD.into());
                        });
                        if let Some(version) =
                            self.flashing.should_update(&port, fw, board.as_deref())
                        {
                            let text = format!("Updating firmware to {version}...");
                            conn.send(&HostMsg::Notify { text })?;
                            // Leave the notice up briefly; the screen freezes while flashing.
                            thread::sleep(Duration::from_secs(2));
                            return Ok(SessionEnd::Flash(port));
                        }
                    }
                    handle(&self.cfg, &mut conn, msg)?
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => bail!("device disconnected"),
            }
        }
    }
}

fn disconnected(s: &mut api::Status) {
    s.connected = false;
    s.port.clear();
    s.serial.clear();
    s.firmware.clear();
    s.board.clear();
    s.proto = 0;
}

fn notification(n: notifications::Notification) -> HostMsg {
    HostMsg::Notification {
        app: n.app,
        summary: n.summary,
        body: n.body,
        critical: n.critical,
    }
}

fn test_notification(critical: bool) -> HostMsg {
    let (summary, body) = if critical {
        ("Critical test notification", "Critical notifications stay on the deck until you tap Dismiss.")
    } else {
        ("Test notification", "Notifications show here for 8 seconds. Tap to dismiss.")
    };
    HostMsg::Notification {
        app: "nerd-deck".into(),
        summary: summary.into(),
        body: body.into(),
        critical,
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
        DeviceMsg::Hello { fw, proto, tile, .. } => {
            log::info!("device firmware {fw}, protocol v{proto}");
            if let Some([w, h]) = tile {
                log::debug!("button images fill {w}x{h}");
            }
            if proto != PROTO_VERSION {
                log::warn!("protocol mismatch: host v{PROTO_VERSION}, device v{proto}");
            }
            // Device may have rebooted; resend its settings and layout.
            send_display(cfg, conn)?;
            send_buttons(cfg, conn, tile)?;
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
        // The reader routes these to the image sender.
        DeviceMsg::Image { .. } => {}
        DeviceMsg::Unknown => log::debug!("ignoring unknown device message"),
    }
    Ok(())
}

fn send_display(cfg: &Config, conn: &mut Connection) -> Result<()> {
    let d = &cfg.display;
    conn.send(&HostMsg::Display {
        tabs: d.tab_order(),
        start: d.start.clone(),
        brightness: d.brightness,
        dim_after: d.dim_after * 60,
        off_when_away: d.off_when_away,
    })
}

/// Sends the layout, then each button's image: filling a `tile` (width,
/// height) when the device reports one, else icon-sized for older firmware.
/// Devices that report a tile (v5) also say when each image has landed, so
/// the next one waits for it and a dropped one is sent again.
fn send_buttons(cfg: &Config, conn: &mut Connection, tile: Option<[u32; 2]>) -> Result<()> {
    let buttons = cfg
        .buttons
        .iter()
        .map(|b| ButtonDef {
            id: b.id.clone(),
            label: b.label.clone(),
            icon: b.icon.as_deref().and_then(icons::lookup).map(String::from),
            image: b.image_data.is_some(),
            color: b.color.as_deref().and_then(config::parse_color),
        })
        .collect();
    conn.send(&HostMsg::Buttons { buttons })?;

    for b in &cfg.buttons {
        let Some(source) = &b.image_data else { continue };
        let img = match tile {
            Some([w, h]) => images::fill(source, w, h),
            None => images::icon(source),
        };
        for attempt in 1..=IMAGE_TRIES {
            conn.clear_image_results();
            send_image(conn, &b.id, &img)?;
            if tile.is_none() {
                break; // older firmware doesn't say
            }
            match conn.image_result(&b.id, IMAGE_RESULT_TIMEOUT) {
                Some(true) => break,
                Some(false) if attempt < IMAGE_TRIES => {
                    log::debug!("button {:?}: image dropped; sending it again", b.id);
                }
                Some(false) => log::warn!("button {:?}: the device kept dropping its image", b.id),
                None => {
                    log::warn!("button {:?}: no word from the device on its image", b.id);
                    break;
                }
            }
        }
    }
    Ok(())
}

fn send_image(conn: &mut Connection, id: &str, img: &images::ButtonImage) -> Result<()> {
    for (i, chunk) in img.data.chunks(IMAGE_CHUNK).enumerate() {
        conn.send(&HostMsg::Image {
            id: id.to_owned(),
            w: img.width,
            h: img.height,
            fmt: images::FORMAT,
            offset: i * IMAGE_CHUNK,
            total: img.data.len(),
            data: base64::engine::general_purpose::STANDARD.encode(chunk),
        })?;
        thread::sleep(IMAGE_CHUNK_PAUSE);
    }
    Ok(())
}
