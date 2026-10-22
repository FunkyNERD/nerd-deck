//! Desktop notifications, copied off the session bus for the device to show.
//!
//! Apps send notifications to the desktop's notification server, so this
//! watches as a bus monitor rather than replacing the server. Two interfaces
//! cover the desktops: the freedesktop one (KDE, XFCE, Cinnamon, dunst, ...,
//! and most apps everywhere) and GNOME Shell's `org.gtk.Notifications`, which
//! GLib apps use under GNOME. Flatpak apps go through the portal, which
//! forwards to one of the two.

use std::collections::HashMap;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use zbus::blocking::{fdo::MonitoringProxy, Connection, MessageIterator};
use zbus::message::Type;
use zbus::zvariant::{OwnedValue, Value};
use zbus::MatchRule;

use crate::config::NotificationsConfig;

const RETRY_DELAY: Duration = Duration::from_secs(30);

// Limits on what's sent, well inside the device's line length.
const MAX_APP: usize = 32;
const MAX_SUMMARY: usize = 100;
const MAX_BODY: usize = 240;

const FDO: &str = "org.freedesktop.Notifications";
const GTK: &str = "org.gtk.Notifications";

#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub app: String,
    pub summary: String,
    pub body: String,
    pub critical: bool,
}

/// A notification as an app sent it.
#[derive(Debug, Default)]
struct Raw {
    app: String,
    /// The app's desktop file name or app ID, e.g. `org.gnome.Geary`.
    app_id: Option<String>,
    summary: String,
    body: String,
    urgency: Urgency,
}

#[derive(Debug, Default, PartialEq, PartialOrd)]
enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

/// Watch the session bus on a background thread. Notifications that pass the
/// config's filters arrive on the returned channel.
pub fn watch(cfg: NotificationsConfig) -> Receiver<Notification> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || loop {
        match monitor(&cfg, &tx) {
            Ok(()) => return, // the receiver is gone
            Err(e) => log::warn!(
                "desktop notifications: {e:#}; retrying in {}s",
                RETRY_DELAY.as_secs()
            ),
        }
        thread::sleep(RETRY_DELAY);
    });
    rx
}

/// Runs until the bus connection fails (an error) or `tx`'s receiver is
/// dropped (Ok).
fn monitor(cfg: &NotificationsConfig, tx: &Sender<Notification>) -> Result<()> {
    let bus = Connection::session().context("connecting to the session bus")?;
    // A monitor can't send, so Do Not Disturb is looked up on a second connection.
    let dnd = Connection::session().context("connecting to the session bus")?;

    let rules = [
        method_rule(FDO, "Notify")?,
        method_rule(GTK, "AddNotification")?,
    ];
    let messages = MessageIterator::from(&bus);
    MonitoringProxy::new(&bus)?
        .become_monitor(&rules, 0)
        .context("becoming a bus monitor")?;
    log::info!("watching desktop notifications");

    for msg in messages {
        let msg = msg?;
        let header = msg.header();
        if header.message_type() != Type::MethodCall {
            continue;
        }
        let parsed = match header.interface().map(|i| i.as_str()) {
            Some(FDO) => msg.body().deserialize().map(from_fdo),
            Some(GTK) => msg.body().deserialize().map(from_gtk),
            _ => continue,
        };
        let raw = match parsed {
            Ok(raw) => raw,
            Err(e) => {
                log::debug!("unreadable notification: {e}");
                continue;
            }
        };

        if !accepts(cfg, &raw) {
            log::info!("notification from {:?} (filtered out)", raw.app);
            continue;
        }
        if raw.urgency != Urgency::Critical && do_not_disturb(&dnd) {
            log::info!("notification from {:?} (Do Not Disturb)", raw.app);
            continue;
        }
        log::info!("notification from {:?}", raw.app);
        if tx.send(prepare(cfg, raw)).is_err() {
            return Ok(());
        }
    }
    anyhow::bail!("the session bus closed the connection")
}

fn method_rule(interface: &'static str, member: &'static str) -> Result<MatchRule<'static>> {
    Ok(MatchRule::builder()
        .msg_type(Type::MethodCall)
        .interface(interface)?
        .member(member)?
        .build())
}

type FdoNotify = (
    String,                      // app_name
    u32,                         // replaces_id
    String,                      // app_icon
    String,                      // summary
    String,                      // body
    Vec<String>,                 // actions
    HashMap<String, OwnedValue>, // hints
    i32,                         // expire_timeout
);

fn from_fdo((app, _, _, summary, body, _, hints, _): FdoNotify) -> Raw {
    let urgency = match hints.get("urgency").map(|v| &**v) {
        Some(Value::U8(0)) => Urgency::Low,
        Some(Value::U8(2)) => Urgency::Critical,
        _ => Urgency::Normal,
    };
    Raw { app, app_id: string(hints.get("desktop-entry")), summary, body, urgency }
}

type GtkAddNotification = (
    String,                      // app_id
    String,                      // notification id
    HashMap<String, OwnedValue>, // title, body, priority, ...
);

fn from_gtk((app_id, _, n): GtkAddNotification) -> Raw {
    let urgency = match string(n.get("priority")).as_deref() {
        Some("low") => Urgency::Low,
        Some("urgent") => Urgency::Critical,
        _ if matches!(n.get("urgent").map(|v| &**v), Some(Value::Bool(true))) => Urgency::Critical,
        _ => Urgency::Normal,
    };
    Raw {
        // GNOME Shell shows the desktop file's name; the app ID's last part is close.
        app: app_id.rsplit('.').next().unwrap_or(&app_id).to_owned(),
        app_id: Some(app_id),
        summary: string(n.get("title")).unwrap_or_default(),
        body: string(n.get("body")).unwrap_or_default(),
        urgency,
    }
}

fn string(v: Option<&OwnedValue>) -> Option<String> {
    match v.map(|v| &**v) {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// The config's app filters. Low-urgency notifications (connection changes,
/// background jobs and the like) are always left out.
fn accepts(cfg: &NotificationsConfig, n: &Raw) -> bool {
    if n.urgency == Urgency::Low {
        return false;
    }
    let names = app_names(n);
    let listed = |list: &[String]| list.iter().any(|a| names.contains(&a.to_lowercase()));
    (cfg.apps.is_empty() || listed(&cfg.apps)) && !listed(&cfg.ignore)
}

/// What a config entry can call an app by: its name, its desktop file or app
/// ID, and that ID's last part (`geary` for `org.gnome.Geary`).
fn app_names(n: &Raw) -> Vec<String> {
    let mut names = vec![n.app.to_lowercase()];
    if let Some(id) = &n.app_id {
        let id = id.to_lowercase();
        let id = id.strip_suffix(".desktop").unwrap_or(&id).to_owned();
        if let Some(last) = id.rsplit('.').next() {
            names.push(last.to_owned());
        }
        names.push(id);
    }
    names
}

fn prepare(cfg: &NotificationsConfig, n: Raw) -> Notification {
    let mut app = clean(&n.app, MAX_APP);
    if app.is_empty() {
        app = "Notification".into();
    }
    Notification {
        app,
        summary: clean(&n.summary, MAX_SUMMARY),
        body: if cfg.body { clean(&n.body, MAX_BODY) } else { String::new() },
        critical: n.urgency == Urgency::Critical,
    }
}

/// Fit text to the device's fonts, which only have ASCII: drop markup (the
/// freedesktop spec allows a little HTML in bodies), decode its entities, swap
/// typographic punctuation for ASCII, drop anything else, fold whitespace and
/// line breaks to single spaces, and cut to `max` bytes.
fn clean(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        rest = &rest[c.len_utf8()..];
        let ascii = match c {
            '<' if rest.contains('>') => {
                rest = &rest[rest.find('>').unwrap() + 1..];
                continue;
            }
            '&' => match ["amp;", "lt;", "gt;", "quot;", "apos;", "#39;"]
                .iter()
                .zip(["&", "<", ">", "\"", "'", "'"])
                .find(|(entity, _)| rest.starts_with(**entity))
            {
                Some((entity, s)) => {
                    rest = &rest[entity.len()..];
                    s
                }
                None => "&",
            },
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{2032}' => "'",
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{2033}' | '\u{00AB}' | '\u{00BB}' => "\"",
            '\u{2010}'..='\u{2015}' | '\u{2212}' => "-",
            '\u{2026}' => "...",
            '\u{2022}' | '\u{00B7}' => "*",
            c if c.is_whitespace() => " ",
            c if c.is_ascii_graphic() => {
                out.push(c);
                continue;
            }
            _ => continue,
        };
        if ascii == " " && (out.is_empty() || out.ends_with(' ')) {
            continue;
        }
        out.push_str(ascii);
    }
    let mut out = out.trim_end().to_owned();
    if out.len() > max {
        out.truncate(max - 3);
        out.push_str("...");
    }
    out
}

/// Whether the desktop is holding notifications back: Plasma (and others)
/// expose the freedesktop server's `Inhibited` property; GNOME has a setting.
fn do_not_disturb(bus: &Connection) -> bool {
    let inhibited = zbus::blocking::Proxy::new(bus, FDO, "/org/freedesktop/Notifications", FDO)
        .and_then(|p| p.get_property::<bool>("Inhibited"));
    if let Ok(inhibited) = inhibited {
        return inhibited;
    }
    Command::new("gsettings")
        .args(["get", "org.gnome.desktop.notifications", "show-banners"])
        .output()
        .is_ok_and(|o| o.status.success() && o.stdout.starts_with(b"false"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(apps: &[&str], ignore: &[&str]) -> NotificationsConfig {
        NotificationsConfig {
            enabled: true,
            apps: apps.iter().map(|s| s.to_string()).collect(),
            ignore: ignore.iter().map(|s| s.to_string()).collect(),
            body: true,
        }
    }

    fn raw(app: &str, app_id: Option<&str>) -> Raw {
        Raw { app: app.into(), app_id: app_id.map(String::from), ..Default::default() }
    }

    #[test]
    fn filters_by_app() {
        let geary = raw("Geary", Some("org.gnome.Geary"));
        assert!(accepts(&cfg(&[], &[]), &geary));
        assert!(accepts(&cfg(&["geary"], &[]), &geary));
        assert!(accepts(&cfg(&["org.gnome.Geary"], &[]), &geary));
        assert!(!accepts(&cfg(&["Thunderbird"], &[]), &geary));
        assert!(!accepts(&cfg(&[], &["Geary"]), &geary));
        assert!(accepts(&cfg(&["thunderbird"], &[]), &raw("Thunderbird", Some("thunderbird.desktop"))));

        let low = Raw { urgency: Urgency::Low, ..raw("Geary", None) };
        assert!(!accepts(&cfg(&[], &[]), &low));
    }

    #[test]
    fn cleans_text() {
        assert_eq!(clean("<b>Re:</b> lunch &amp; <i>stuff</i>", 100), "Re: lunch & stuff");
        assert_eq!(clean("It\u{2019}s \u{201C}done\u{201D} \u{2014} ok\u{2026}", 100), "It's \"done\" - ok...");
        assert_eq!(clean("line one\n\n  line\ttwo  ", 100), "line one line two");
        assert_eq!(clean("caf\u{e9} \u{1F600} a < b", 100), "caf a < b");
        assert_eq!(clean("abcdefghij", 8), "abcde...");
    }

    #[test]
    fn reads_gtk_priority() {
        let mut n = HashMap::new();
        n.insert("title".to_string(), OwnedValue::try_from(Value::from("Hi")).unwrap());
        n.insert("priority".to_string(), OwnedValue::try_from(Value::from("urgent")).unwrap());
        let r = from_gtk(("org.gnome.Geary".into(), "1".into(), n));
        assert_eq!(r.app, "Geary");
        assert_eq!(r.summary, "Hi");
        assert_eq!(r.urgency, Urgency::Critical);
    }
}
