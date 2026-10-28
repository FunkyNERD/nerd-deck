use std::io::{ErrorKind, Read, Write};
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

/// Resolve which serial port to use: the configured one, or the first USB
/// port matching the configured VID/PID.
pub fn find_port(cfg: &DeviceConfig) -> Result<Port> {
    let ports = serialport::available_ports()?;
    let usb = |p: &serialport::SerialPortInfo| match &p.port_type {
        SerialPortType::UsbPort(u) => Some(u.clone()),
        _ => None,
    };
    if let Some(name) = &cfg.port {
        let usb = ports.iter().find(|p| &p.port_name == name).and_then(usb);
        return Ok(Port { name: name.clone(), usb });
    }
    ports
        .iter()
        .find_map(|p| {
            let u = usb(p)?;
            (u.vid == cfg.vid && cfg.pid.is_none_or(|pid| pid == u.pid))
                .then(|| Port { name: p.port_name.clone(), usb: Some(u) })
        })
        .ok_or_else(|| anyhow!("no device with vid {:04x} found", cfg.vid))
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
