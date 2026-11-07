use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serialport::{SerialPort, SerialPortType, UsbPortInfo};

use crate::config::DeviceConfig;
use crate::protocol::{DeviceMsg, HostMsg};

const READ_TIMEOUT: Duration = Duration::from_millis(200);
const MAX_LINE: usize = 4096;

/// A serial port to connect to, with its USB identity when it has one.
#[derive(Debug, Clone)]
pub struct Port {
    pub name: String,
    pub usb: Option<UsbPortInfo>,
}

impl Port {
    /// The USB serial number; on the ESP32-S3 it's the chip's MAC, the same in
    /// the app and the ROM bootloader.
    pub fn serial(&self) -> Option<&str> {
        self.usb.as_ref()?.serial_number.as_deref()
    }

    /// What tells this device apart: its serial number, else its port.
    pub fn key(&self) -> &str {
        self.serial().unwrap_or(&self.name)
    }
}

/// The serial devices the kernel knows of, by name. Far cheaper than
/// `find_port`'s scan of each one's USB details, so a wait for the deck
/// scans again only once this changes: something was plugged in or out.
pub fn serial_devices() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir("/sys/class/tty")
        .map(|dir| dir.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// Watch udev for serial devices coming and going, on a background thread,
/// and call `changed` for each. Returns false if udev can't be watched; the
/// caller then has to look for itself now and then.
///
/// udev's events come after its rules have run, so a new port already has
/// the permissions it will get.
pub fn watch_ports(changed: impl Fn() + Send + 'static) -> bool {
    let (ready_tx, ready) = mpsc::sync_channel(1);
    let spawned = thread::Builder::new().name("port-watch".into()).spawn(move || {
        // Made here: udev's handles can't move between threads.
        let monitor = libudev::Context::new().and_then(|udev| {
            let mut monitor = libudev::Monitor::new(&udev)?;
            monitor.match_subsystem("tty")?;
            monitor.listen()
        });
        let mut monitor = match monitor {
            Ok(monitor) => {
                let _ = ready_tx.send(Ok(()));
                monitor
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e.to_string()));
                return;
            }
        };
        let mut pfd = libc::pollfd { fd: monitor.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        loop {
            // SAFETY: one valid pollfd, and `monitor` keeps the fd open.
            if unsafe { libc::poll(&mut pfd, 1, -1) } < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == ErrorKind::Interrupted {
                    continue;
                }
                log::warn!("watching for serial ports: {err}; looking every few seconds instead");
                return;
            }
            let mut any = false;
            while let Some(event) = monitor.receive_event() {
                use libudev::EventType::{Add, Remove};
                if matches!(event.event_type(), Add | Remove) {
                    log::debug!("{} {:?}", event.event_type(), event.sysname());
                    any = true;
                }
            }
            if any {
                changed();
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("can't start the serial port watcher: {e}");
        return false;
    }
    match ready.recv() {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            log::warn!("can't watch udev for serial ports ({e}); looking every few seconds instead");
            false
        }
        Err(_) => false,
    }
}

/// Resolve which serial port to use: the configured one, or the first USB
/// port matching the configured VID/PID.
pub fn find_port(cfg: &DeviceConfig) -> Result<Port> {
    find_ports(cfg)?.into_iter().next().ok_or_else(|| anyhow!("no device with vid {:04x} found", cfg.vid))
}

/// The ports the deck may be on: the configured one, or every USB port
/// matching the configured VID/PID. Other boards share Espressif's VID/PID,
/// so only a hello tells a deck apart.
pub fn find_ports(cfg: &DeviceConfig) -> Result<Vec<Port>> {
    let ports = serialport::available_ports()?;
    let usb = |p: &serialport::SerialPortInfo| match &p.port_type {
        SerialPortType::UsbPort(u) => Some(u.clone()),
        _ => None,
    };
    if let Some(name) = &cfg.port {
        // Not plugged in: nothing to open, and nothing to retry until it is.
        if !Path::new(name).exists() {
            return Ok(Vec::new());
        }
        let usb = ports.iter().find(|p| &p.port_name == name).and_then(usb);
        return Ok(vec![Port { name: name.clone(), usb }]);
    }
    Ok(ports
        .iter()
        .filter_map(|p| {
            let u = usb(p)?;
            (u.vid == cfg.vid && cfg.pid.is_none_or(|pid| pid == u.pid))
                .then(|| Port { name: p.port_name.clone(), usb: Some(u) })
        })
        .collect())
}

pub fn list_ports() -> Result<()> {
    for p in serialport::available_ports()? {
        match p.port_type {
            SerialPortType::UsbPort(u) => println!(
                "{}  usb {:04x}:{:04x}  {} {}",
                p.port_name,
                u.vid,
                u.pid,
                u.manufacturer.unwrap_or_default(),
                u.product.unwrap_or_default()
            ),
            other => println!("{}  {:?}", p.port_name, other),
        }
    }
    Ok(())
}

/// An open link to the device. Incoming messages are parsed on a background
/// thread and delivered on `rx`; it disconnects when the port errors out.
pub struct Connection {
    writer: Box<dyn SerialPort>,
    pub rx: Receiver<DeviceMsg>,
    /// Image results (id, ok), kept apart so waiting for one doesn't swallow
    /// presses or a hello.
    images: Receiver<(String, bool)>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}

impl Connection {
    pub fn open(path: &str, baud: u32) -> Result<Self> {
        let port = serialport::new(path, baud)
            .timeout(READ_TIMEOUT)
            .open()
            .with_context(|| format!("opening {path}"))?;
        let reader = port.try_clone().context("cloning serial port")?;

        let (tx, rx) = mpsc::channel();
        let (images_tx, images) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_reader = stop.clone();
        let reader = thread::Builder::new()
            .name("serial-reader".into())
            .spawn(move || read_loop(reader, tx, images_tx, stop_reader))?;

        Ok(Self { writer: port, rx, images, stop, reader: Some(reader) })
    }

    /// Forget image results from earlier sends.
    pub fn clear_image_results(&self) {
        self.images.try_iter().for_each(drop);
    }

    /// Wait for the device's result for image `id`: Some(ok), or None if none
    /// arrives within `timeout`.
    pub fn image_result(&self, id: &str, timeout: Duration) -> Option<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            match self.images.recv_timeout(left) {
                Ok((got, ok)) if got == id => return Some(ok),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    }

    pub fn send(&mut self, msg: &HostMsg) -> Result<()> {
        let mut line = serde_json::to_vec(msg)?;
        line.push(b'\n');
        self.writer.write_all(&line).context("writing to device")?;
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wait (at most READ_TIMEOUT) for the reader's handle to close, so the
        // port is free to reopen, e.g. by the firmware flasher.
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn read_loop(
    mut port: Box<dyn SerialPort>,
    tx: Sender<DeviceMsg>,
    images: Sender<(String, bool)>,
    stop: Arc<AtomicBool>,
) {
    let mut buf = Vec::with_capacity(MAX_LINE);
    let mut chunk = [0u8; 256];

    while !stop.load(Ordering::Relaxed) {
        let n = match port.read(&mut chunk) {
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::TimedOut => continue,
            Err(e) => {
                log::warn!("serial read failed: {e}");
                return;
            }
        };
        buf.extend_from_slice(&chunk[..n]);

        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // The ESP32 ROM/bootloader may print plain text on reset; skip it.
            match serde_json::from_str::<DeviceMsg>(line) {
                Ok(DeviceMsg::Image { id, ok }) => {
                    let _ = images.send((id, ok));
                }
                Ok(msg) => {
                    if tx.send(msg).is_err() {
                        return;
                    }
                }
                Err(_) => log::debug!("ignoring non-protocol line: {line}"),
            }
        }

        if buf.len() > MAX_LINE {
            log::warn!("discarding {} bytes without newline", buf.len());
            buf.clear();
        }
    }
}
