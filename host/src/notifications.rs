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
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
pub struct Raw {
    app: String,
    /// The app's desktop file name or app ID, e.g. `org.gnome.Geary`.
    app_id: Option<String>,
    summary: String,
    body: String,
    urgency: Urgency,
    /// The desktop's Do Not Disturb was on when it arrived.
    dnd: bool,
}

#[derive(Debug, Default, PartialEq, PartialOrd)]
enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

/// Watch the session bus on a background thread. Notifications arrive on the
/// returned channel unfiltered; `filter` applies the config, so a reloaded
/// config takes effect at once.
pub fn watch() -> Receiver<Raw> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || loop {
        match monitor(&tx) {
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
fn monitor(tx: &Sender<Raw>) -> Result<()> {
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

        let mut raw = raw;
        raw.dnd = raw.urgency != Urgency::Critical && do_not_disturb(&dnd);
        if tx.send(raw).is_err() {
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
    Raw { app, app_id: string(hints.get("desktop-entry")), summary, body, urgency, dnd: false }
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
        dnd: false,
    }
}

fn string(v: Option<&OwnedValue>) -> Option<String> {
    match v.map(|v| &**v) {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// Applies the config: what to show on the deck, if anything.
pub fn filter(cfg: &NotificationsConfig, n: Raw) -> Option<Notification> {
    if !accepts(cfg, &n) {
        log::info!("notification from {:?} (filtered out)", n.app);
        return None;
    }
    if n.dnd && cfg.dnd {
        log::info!("notification from {:?} (Do Not Disturb)", n.app);
        return None;
    }
    log::info!("notification from {:?}", n.app);
    Some(prepare(cfg, n))
}

/// The config's app filters, and `skip_low` for low-urgency notifications
/// (connection changes, background jobs and the like). The tray's own
/// messages are always left out.
fn accepts(cfg: &NotificationsConfig, n: &Raw) -> bool {
    if (cfg.skip_low && n.urgency == Urgency::Low) || n.app == crate::tray::APP_NAME {
        return false;
    }
    let names = app_names(n);
    let listed = |list: &[String]| list.iter().any(|a| names.contains(&a.to_lowercase()));
    (cfg.apps.is_empty() || listed(&cfg.apps)) && !listed(&cfg.ignore)
}

/// Apps that have sent notifications, so the settings window can list them
/// for filtering. Kept in the state directory as `name<TAB>app id<TAB>unix time`
/// lines, newest first.
pub struct SeenApps {
    path: Option<PathBuf>,
    apps: Vec<SeenApp>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeenApp {
    pub name: String,
    pub app_id: String,
    /// Unix seconds.
    pub last: u64,
}

impl SeenApps {
    pub fn load() -> Self {
        let path = dirs::state_dir().map(|d| d.join("nerd-deck").join("notification-apps"));
        let text = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        Self { path, apps: parse_seen(&text) }
    }

    pub fn list(&self) -> &[SeenApp] {
        &self.apps
    }

    /// Note an app, saving only when it's new or last seen over a minute ago.
    pub fn record(&mut self, n: &Raw) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let app_id = n.app_id.clone().unwrap_or_default();
        let fresh = match self.apps.iter().position(|a| a.name == n.app) {
            Some(i) => {
                let a = self.apps.remove(i);
                let fresh = now.saturating_sub(a.last) >= 60 || a.app_id != app_id;
                self.apps.insert(0, SeenApp { last: if fresh { now } else { a.last }, app_id, ..a });
                fresh
            }
            None => {
                self.apps.insert(0, SeenApp { name: n.app.clone(), app_id, last: now });
                true
            }
        };
        if fresh {
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let text: String = self
            .apps
            .iter()
            .map(|a| format!("{}\t{}\t{}\n", a.name.replace(['\t', '\n'], " "), a.app_id, a.last))
            .collect();
        let saved = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(path, text));
        if let Err(e) = saved {
            log::warn!("can't save {}: {e}", path.display());
        }
    }
}

fn parse_seen(text: &str) -> Vec<SeenApp> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let name = parts.next().filter(|n| !n.is_empty())?.to_owned();
            let app_id = parts.next().unwrap_or_default().to_owned();
            let last = parts.next().and_then(|t| t.parse().ok()).unwrap_or(0);
            Some(SeenApp { name, app_id, last })
        })
        .collect()
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
            ..Default::default()
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
    fn parses_seen_apps() {
        let apps = parse_seen("Geary\torg.gnome.Geary\t1700000000\n\nfoo\t\tx\n");
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].app_id, "org.gnome.Geary");
        assert_eq!(apps[1], SeenApp { name: "foo".into(), app_id: String::new(), last: 0 });
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
