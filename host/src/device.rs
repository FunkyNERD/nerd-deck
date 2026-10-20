use std::io::{ErrorKind, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serialport::{SerialPort, SerialPortType};

use crate::config::DeviceConfig;
use crate::protocol::{DeviceMsg, HostMsg};

const READ_TIMEOUT: Duration = Duration::from_millis(200);
const MAX_LINE: usize = 4096;

/// Resolve which serial port to use: the configured one, or the first USB
/// port matching the configured VID/PID.
pub fn find_port(cfg: &DeviceConfig) -> Result<String> {
    if let Some(port) = &cfg.port {
        return Ok(port.clone());
    }
    serialport::available_ports()?
        .into_iter()
        .find(|p| match &p.port_type {
            SerialPortType::UsbPort(u) => u.vid == cfg.vid && cfg.pid.is_none_or(|pid| pid == u.pid),
            _ => false,
        })
        .map(|p| p.port_name)
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
    stop: Arc<AtomicBool>,
}

impl Connection {
    pub fn open(path: &str, baud: u32) -> Result<Self> {
        let port = serialport::new(path, baud)
            .timeout(READ_TIMEOUT)
            .open()
            .with_context(|| format!("opening {path}"))?;
        let reader = port.try_clone().context("cloning serial port")?;

        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_reader = stop.clone();
        thread::Builder::new()
            .name("serial-reader".into())
            .spawn(move || read_loop(reader, tx, stop_reader))?;

        Ok(Self { writer: port, rx, stop })
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
    }
}

fn read_loop(mut port: Box<dyn SerialPort>, tx: Sender<DeviceMsg>, stop: Arc<AtomicBool>) {
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
