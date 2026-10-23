//! Starts and stops the service's systemd user unit, for the tray and the
//! settings window. Goes through the user's systemd manager on the session
//! bus, as `systemctl --user` does.

use zbus::blocking::Connection;
use zbus::zvariant::OwnedObjectPath;

pub const UNIT: &str = "nerd-deck.service";

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait Manager {
    fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
}

/// Asks systemd to start the service; it's running once its name is on the bus.
pub fn start() -> Result<(), String> {
    manager()?.start_unit(UNIT, "replace").map(drop).map_err(describe)
}

/// Asks systemd to stop the service.
pub fn stop() -> Result<(), String> {
    manager()?.stop_unit(UNIT, "replace").map(drop).map_err(describe)
}

fn manager() -> Result<ManagerProxyBlocking<'static>, String> {
    let bus = Connection::session().map_err(|e| format!("connecting to the session bus: {e}"))?;
    ManagerProxyBlocking::new(&bus).map_err(describe)
}

fn describe(e: zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, _, _) if name.ends_with("NoSuchUnit") => {
            format!("{UNIT} isn't installed; run host/install.sh or install the package")
        }
        zbus::Error::MethodError(_, Some(msg), _) => msg,
        e => e.to_string(),
    }
}
