//! The service's D-Bus interface on the session bus. The tray and the
//! settings window use it to see the service's state and steer it.
//!
//! Method calls run on zbus's own thread; they hand a `Command` to the main
//! loop, which owns the device, and wait for its reply.

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use zbus::blocking::Connection;
use zbus::fdo;
use zbus::names::BusName;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{DeserializeDict, SerializeDict, Type};

pub const NAME: &str = "io.github.FunkyNERD.NerdDeck";
pub const PATH: &str = "/io/github/FunkyNERD/NerdDeck";
pub const INTERFACE: &str = "io.github.FunkyNERD.NerdDeck1";

/// How long a method call waits for the main loop, which checks for commands
/// at least once per stats interval.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Sent as a string-keyed dict, so fields can be added later without
/// breaking older clients.
#[derive(Debug, Clone, Default, PartialEq, SerializeDict, DeserializeDict, Type)]
#[zvariant(signature = "dict")]
pub struct Status {
    pub connected: bool,
    /// Serial port of the connected deck; empty when not connected.
    pub port: String,
    /// The deck's USB serial number (its chip's MAC); empty when not connected.
    pub serial: String,
    /// The deck's firmware version; empty until it says hello.
    pub firmware: String,
    /// The board the deck's firmware is built for, and its protocol version;
    /// empty and 0 until it says hello.
    pub board: String,
    pub proto: u32,
    /// Firmware is being installed on the deck.
    pub flashing: bool,
    /// Why the last install failed; None when it worked or none was tried.
    /// Optional, as fields added later must be, so a newer client can still
    /// read an older service's status.
    pub flash_error: Option<String>,
    /// This host's version.
    pub version: String,
    /// A newer nerd-deck release, if the update check found one.
    pub update: String,
    /// The config file the service reads.
    pub config: String,
}

pub type Reply = Sender<Result<(), String>>;

/// Requests for the main loop.
pub enum Command {
    /// Read the config file again and apply it.
    Reload(Reply),
    /// Show a sample notification on the deck, whatever the filters say.
    TestNotification { critical: bool, reply: Reply },
    /// Install the firmware image at this path on the deck, if it's newer than
    /// what the deck runs.
    InstallFirmware { path: String, reply: Reply },
}

/// The service's side: holds the status and tells clients when it changes.
pub struct Service {
    status: Arc<Mutex<Status>>,
    /// None when the bus or the name wasn't available; the service runs anyway.
    conn: Option<Connection>,
}

impl Service {
    pub fn start(status: Status, commands: Sender<Command>) -> Self {
        let status = Arc::new(Mutex::new(status));
        let api = Api { status: status.clone(), commands };
        let conn = zbus::blocking::connection::Builder::session()
            .and_then(|b| b.name(NAME))
            .and_then(|b| b.serve_at(PATH, api))
            .and_then(|b| b.build())
            .with_context(|| format!("claiming {NAME} on the session bus"));
        let conn = match conn {
            Ok(conn) => Some(conn),
            Err(e) => {
                log::warn!("{e:#}; the tray can't reach this instance");
                None
            }
        };
        Self { status, conn }
    }

    /// Change the status, and signal clients if that changed anything.
    pub fn update(&self, f: impl FnOnce(&mut Status)) {
        let changed = {
            let mut status = self.status.lock().unwrap();
            let before = status.clone();
            f(&mut status);
            *status != before
        };
        if let (true, Some(conn)) = (changed, &self.conn) {
            let sent = conn.emit_signal(None::<BusName>, PATH, INTERFACE, "StatusChanged", &());
            if let Err(e) = sent {
                log::debug!("emitting StatusChanged: {e}");
            }
        }
    }
}

struct Api {
    status: Arc<Mutex<Status>>,
    commands: Sender<Command>,
}

impl Api {
    fn call(&self, command: impl FnOnce(Reply) -> Command) -> fdo::Result<()> {
        let (tx, rx) = mpsc::channel();
        self.commands
            .send(command(tx))
            .map_err(|_| fdo::Error::Failed("the service is stopping".into()))?;
        match rx.recv_timeout(COMMAND_TIMEOUT) {
            Ok(result) => result.map_err(fdo::Error::Failed),
            Err(_) => Err(fdo::Error::Failed("the service didn't respond".into())),
        }
    }
}

#[zbus::interface(name = "io.github.FunkyNERD.NerdDeck1")]
impl Api {
    fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    fn reload(&self) -> fdo::Result<()> {
        self.call(Command::Reload)
    }

    fn test_notification(&self, critical: bool) -> fdo::Result<()> {
        self.call(|reply| Command::TestNotification { critical, reply })
    }

    /// Checks the image, starts the install and returns; `Status.flashing`
    /// and `Status.flash_error` follow it.
    fn install_firmware(&self, path: String) -> fdo::Result<()> {
        self.call(|reply| Command::InstallFirmware { path, reply })
    }

    /// Emitted when anything in `Status` changes.
    #[zbus(signal)]
    async fn status_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// The client's side, for the tray.
#[zbus::proxy(
    interface = "io.github.FunkyNERD.NerdDeck1",
    default_service = "io.github.FunkyNERD.NerdDeck",
    default_path = "/io/github/FunkyNERD/NerdDeck"
)]
pub trait NerdDeck {
    fn status(&self) -> zbus::Result<Status>;
    fn reload(&self) -> zbus::Result<()>;
    fn test_notification(&self, critical: bool) -> zbus::Result<()>;
    fn install_firmware(&self, path: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    fn status_changed(&self) -> zbus::Result<()>;
}
