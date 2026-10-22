//! The tray icon (`nerd-deck --tray`): shows whether the deck is connected and
//! offers a few actions. It holds no state of its own; it asks the running
//! service over D-Bus (api.rs), so it can start and stop independently.

use std::process::Command;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ksni::blocking::TrayMethods;
use ksni::menu::{StandardItem, SubMenu};
use ksni::{Icon, MenuItem, ToolTip};
use zbus::blocking::Connection;
use zbus::fdo::{RequestNameFlags, RequestNameReply};

use crate::api::{NerdDeckProxyBlocking, Status};

/// The app name on the tray's own desktop notifications. The service never
/// copies these to the deck.
pub const APP_NAME: &str = "nerd-deck";

/// Held while the tray runs, so a second one exits.
const TRAY_NAME: &str = "io.github.FunkyNERD.NerdDeck.Tray";

/// Status is also polled, in case a signal is missed while the service restarts.
const POLL: Duration = Duration::from_secs(5);

const RELEASES: &str = "https://github.com/FunkyNERD/nerd-deck/releases/latest";

pub fn run() -> Result<()> {
    let bus = Connection::session().context("connecting to the session bus")?;
    match bus.request_name_with_flags(TRAY_NAME, RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner) => {}
        Ok(_) | Err(zbus::Error::NameTaken) => bail!("the nerd-deck tray is already running"),
        Err(e) => return Err(e.into()),
    }

    let deck = NerdDeckProxyBlocking::new(&bus)?;
    let tray = DeckTray { deck: deck.clone(), status: deck.status().ok() };
    let handle = tray
        .spawn()
        .context("showing the tray icon (does this desktop have a system tray?)")?;

    let refresh = {
        let (deck, handle) = (deck.clone(), handle.clone());
        move || {
            let status = deck.status().ok();
            handle.update(|t| {
                if t.status != status {
                    t.status = status;
                }
            });
        }
    };
    {
        let (deck, refresh) = (deck.clone(), refresh.clone());
        thread::spawn(move || match deck.receive_status_changed() {
            Ok(signals) => signals.for_each(|_| refresh()),
            Err(e) => log::warn!("not watching for status changes: {e}"),
        });
    }
    loop {
        thread::sleep(POLL);
        refresh();
    }
}

struct DeckTray {
    deck: NerdDeckProxyBlocking<'static>,
    /// None while the service isn't running.
    status: Option<Status>,
}

impl DeckTray {
    fn summary(&self) -> String {
        match &self.status {
            None => "The nerd-deck service isn't running".into(),
            Some(s) if !s.connected => "Waiting for the deck".into(),
            Some(s) if s.firmware.is_empty() => format!("Connected on {}", s.port),
            Some(s) => format!("Connected · firmware {}", s.firmware),
        }
    }

    fn connected(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.connected)
    }

    /// Run a service call off the tray's thread; report a failure as a
    /// desktop notification, since a menu has nowhere to show it.
    fn call(&self, what: &'static str, f: impl FnOnce(&NerdDeckProxyBlocking) -> zbus::Result<()> + Send + 'static) {
        let deck = self.deck.clone();
        thread::spawn(move || {
            if let Err(e) = f(&deck) {
                let msg = match e {
                    zbus::Error::MethodError(_, Some(msg), _) => msg,
                    e => e.to_string(),
                };
                log::warn!("{what}: {msg}");
                desktop_notification(what, &msg);
            }
        });
    }
}

impl ksni::Tray for DeckTray {
    // A click opens the settings; the menu is on the right button.
    fn activate(&mut self, _x: i32, _y: i32) {
        open_settings();
    }

    fn id(&self) -> String {
        "nerd-deck".into()
    }

    fn title(&self) -> String {
        "nerd-deck".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        let look = match &self.status {
            Some(s) if s.connected => Look::Connected,
            Some(_) => Look::Waiting,
            None => Look::Stopped,
        };
        [16, 22, 24, 32, 48, 64].into_iter().map(|size| logo(size, look)).collect()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "nerd-deck".into(),
            description: self.summary(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let running = self.status.is_some();
        let mut menu: Vec<MenuItem<Self>> = vec![
            StandardItem { label: self.summary(), enabled: false, ..Default::default() }.into(),
            StandardItem {
                label: "Settings…".into(),
                icon_name: "configure".into(),
                activate: Box::new(|_| open_settings()),
                ..Default::default()
            }
            .into(),
        ];
        if let Some(update) = self.status.as_ref().map(|s| &s.update).filter(|u| !u.is_empty()) {
            menu.push(
                StandardItem {
                    label: format!("nerd-deck {update} is available…"),
                    activate: Box::new(|_| open(RELEASES)),
                    ..Default::default()
                }
                .into(),
            );
        }
        menu.push(MenuItem::Separator);
        menu.push(
            SubMenu {
                label: "Test notification".into(),
                enabled: self.connected(),
                submenu: vec![
                    StandardItem {
                        label: "Normal".into(),
                        activate: Box::new(|t: &mut Self| {
                            t.call("Test notification", |d| d.test_notification(false))
                        }),
                        ..Default::default()
                    }
                    .into(),
                    StandardItem {
                        label: "Critical".into(),
                        activate: Box::new(|t: &mut Self| {
                            t.call("Test notification", |d| d.test_notification(true))
                        }),
                        ..Default::default()
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
        );
        menu.push(
            StandardItem {
                label: "Edit config file…".into(),
                enabled: running,
                icon_name: "document-edit".into(),
                activate: Box::new(|t: &mut Self| {
                    if let Some(s) = &t.status {
                        open(&s.config);
                    }
                }),
                ..Default::default()
            }
            .into(),
        );
        menu.push(
            StandardItem {
                label: "Reload config".into(),
                enabled: running,
                icon_name: "view-refresh".into(),
                activate: Box::new(|t: &mut Self| t.call("Reload config", |d| d.reload())),
                ..Default::default()
            }
            .into(),
        );
        menu.push(MenuItem::Separator);
        menu.push(
            StandardItem {
                label: "Quit tray".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        );
        menu
    }
}

/// Opens the settings window, or brings the open one to the front.
fn open_settings() {
    let exe = std::env::current_exe().unwrap_or_else(|_| "nerd-deck".into());
    if let Err(e) = Command::new(exe).arg("--settings").spawn() {
        log::warn!("opening the settings window: {e}");
    }
}

fn open(target: &str) {
    if let Err(e) = Command::new("xdg-open").arg(target).spawn() {
        log::warn!("opening {target}: {e}");
    }
}

fn desktop_notification(summary: &str, body: &str) {
    let sent = Connection::session().and_then(|bus| {
        bus.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(
                APP_NAME,
                0u32,
                "dialog-error",
                summary,
                body,
                Vec::<&str>::new(),
                std::collections::HashMap::<&str, zbus::zvariant::Value>::new(),
                -1i32,
            ),
        )
    });
    if let Err(e) = sent {
        log::warn!("showing a desktop notification: {e}");
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Connected,
    /// The service runs but the deck isn't connected.
    Waiting,
    /// The service isn't running.
    Stopped,
}

/// The deck's logo, three rising bars, as an ARGB32 icon. Grey while waiting,
/// faded grey while the service is stopped.
fn logo(size: i32, look: Look) -> Icon {
    // The waiting screen's mark, in its own units: 14 wide, 6 apart, 56 tall.
    const BARS: [(f32, f32, [u8; 3]); 3] = [
        (0.0, 30.0, [0x26, 0xC6, 0xDA]),
        (20.0, 56.0, [0x66, 0xBB, 0x6A]),
        (40.0, 40.0, [0xFF, 0xA7, 0x26]),
    ];
    const W: f32 = 54.0;
    const H: f32 = 56.0;
    const RADIUS: f32 = 3.0;
    const GREY: [u8; 3] = [0x8D, 0x96, 0xA4];
    const SAMPLES: i32 = 4; // per axis, for anti-aliasing

    let scale = (size as f32 - 2.0) / H;
    let left = (size as f32 - W * scale) / 2.0;
    let top = 1.0;
    let alpha = if look == Look::Stopped { 0.5 } else { 1.0 };

    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let mut pixel = [0u8; 4];
            for &(x0, h, color) in &BARS {
                let mut hits = 0;
                for sy in 0..SAMPLES {
                    for sx in 0..SAMPLES {
                        // Sample point in logo units.
                        let x = (px as f32 + (sx as f32 + 0.5) / SAMPLES as f32 - left) / scale;
                        let y = (py as f32 + (sy as f32 + 0.5) / SAMPLES as f32 - top) / scale;
                        if in_rounded_rect(x, y, x0, H - h, 14.0, h, RADIUS) {
                            hits += 1;
                        }
                    }
                }
                if hits > 0 {
                    let rgb = if look == Look::Connected { color } else { GREY };
                    let a = hits as f32 / (SAMPLES * SAMPLES) as f32 * alpha;
                    pixel = [(a * 255.0).round() as u8, rgb[0], rgb[1], rgb[2]];
                }
            }
            data.extend_from_slice(&pixel);
        }
    }
    Icon { width: size, height: size, data }
}

fn in_rounded_rect(x: f32, y: f32, left: f32, top: f32, w: f32, h: f32, r: f32) -> bool {
    if x < left || x > left + w || y < top || y > top + h {
        return false;
    }
    // Distance into the nearest corner's square, if in one.
    let dx = (left + r - x).max(x - (left + w - r)).max(0.0);
    let dy = (top + r - y).max(y - (top + h - r)).max(0.0);
    dx * dx + dy * dy <= r * r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_is_argb_and_sized() {
        let icon = logo(22, Look::Connected);
        assert_eq!(icon.data.len(), 22 * 22 * 4);
        // Bottom middle is inside the tall green bar.
        let i = ((20 * 22 + 11) * 4) as usize;
        assert_eq!(&icon.data[i..i + 4], &[255, 0x66, 0xBB, 0x6A]);
        // The top-left corner is empty.
        assert_eq!(&icon.data[0..4], &[0, 0, 0, 0]);
    }
}
