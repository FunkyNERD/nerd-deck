//! The settings window (`nerd-deck --settings`). It edits the config file in
//! place, keeping its comments and layout, and asks the running service to
//! reload it after each change, so the deck follows along. Status and actions
//! (test notifications, reinstalling firmware) go through the service's D-Bus
//! interface (api.rs). The UI itself is in ui/settings.slint.

slint::include_modules!();

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{fs, thread};

use anyhow::{bail, Context, Result};
use slint::{ComponentHandle, Model, ModelRc, SharedPixelBuffer, SharedString, Timer, TimerMode, VecModel};
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value};
use zbus::blocking::Connection;
use zbus::fdo::{RequestNameFlags, RequestNameReply};

use crate::api::{NerdDeckProxyBlocking, Status};
use crate::config::{self, Config, MAX_BUTTONS, MAX_ID_LEN, MAX_LABEL_LEN};
use crate::firmware::KnownDevices;
use crate::notifications::{SeenApp, SeenApps};
use crate::{battery, collector, icons, images};

/// Held while the window is open. Opening it again asks the open one to come
/// to the front instead (`Present`).
const SETTINGS_NAME: &str = "io.github.FunkyNERD.NerdDeck.Settings";
const SETTINGS_PATH: &str = "/io/github/FunkyNERD/NerdDeck/Settings";

/// Typing is saved once it pauses for this long.
const SAVE_DELAY: Duration = Duration::from_millis(600);

/// The status is also polled, in case a signal is missed while the service restarts.
const STATUS_POLL: Duration = Duration::from_secs(3);

/// The choices offered for `stats.interval_ms`.
const INTERVALS: [(u64, &str); 4] =
    [(500, "Half a second"), (1000, "1 second"), (2000, "2 seconds"), (5000, "5 seconds")];

/// The device shows at most this many disks.
const MAX_DISKS: usize = 4;

/// Sections, in sidebar order, by the names `--page` takes.
const PAGES: [&str; 7] = ["device", "buttons", "stats", "tabs", "notifications", "firmware", "about"];

/// The choices offered for `display.dim_after`, in minutes.
const DIM_AFTER: [(u32, &str); 5] =
    [(0, "Never"), (1, "After 1 minute"), (5, "After 5 minutes"), (15, "After 15 minutes"), (30, "After 30 minutes")];

/// The choices offered for `display.off_after`, in minutes.
const OFF_AFTER: [(u32, &str); 8] = [
    (0, "Never"),
    (1, "After 1 minute"),
    (5, "After 5 minutes"),
    (10, "After 10 minutes"),
    (15, "After 15 minutes"),
    (30, "After 30 minutes"),
    (45, "After 45 minutes"),
    (60, "After 1 hour"),
];

/// A drop list's labels for `choices`, and the index of `value`; a value not
/// among them is added as "After N minutes".
fn minutes_choices(choices: &[(u32, &str)], value: u32) -> (Vec<SharedString>, i32) {
    let mut labels: Vec<SharedString> = choices.iter().map(|(_, l)| (*l).into()).collect();
    let index = match choices.iter().position(|(m, _)| *m == value) {
        Some(i) => i,
        None => {
            labels.push(format!("After {value} minutes").into());
            labels.len() - 1
        }
    };
    (labels, index as i32)
}

/// The deck's tabs for the Deck tabs page: name, what's on it, Font Awesome glyph.
fn tab_info(id: &str) -> (&'static str, &'static str, &'static str) {
    match id {
        "system" => ("System", "CPU, memory, uptime and per-core load", "\u{f015}"),
        "network" => ("Network", "Download and upload rates, with a traffic graph", "\u{f1eb}"),
        "storage" => ("Storage", "How full each disk is, USB drives included", "\u{f0a0}"),
        "battery" => ("Battery", "Charge, power draw, health and the charge limit", "\u{f240}"),
        _ => ("Controls", "Your buttons", "\u{f11c}"),
    }
}

/// The deck's protocol version that added the `display` message.
const DISPLAY_PROTO: i32 = 6;

pub fn run(config_path: Option<PathBuf>, page: Option<&str>) -> Result<()> {
    let bus = Connection::session().context("connecting to the session bus")?;
    match bus.request_name_with_flags(SETTINGS_NAME, RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner) => {}
        Ok(_) | Err(zbus::Error::NameTaken) => {
            let open = SettingsProxyBlocking::new(&bus)?;
            open.present(page.unwrap_or_default()).context("bringing the open settings window forward")?;
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    }
    crate::tray::ensure_running(&bus);
    let deck = NerdDeckProxyBlocking::new(&bus)?;
    let status = deck.status().ok();

    // Edit the file the service reads, when it's running.
    let path = status
        .as_ref()
        .map(|s| PathBuf::from(&s.config))
        .filter(|p| !p.as_os_str().is_empty())
        .or(config_path)
        .or_else(Config::default_path)
        .context("no config directory")?;

    let ui = SettingsWindow::new()?;
    // Lets the desktop match the window to nerd-deck.desktop and its icon.
    // Needs the backend that creating the window starts; applies when it's shown.
    slint::set_xdg_app_id("nerd-deck")?;
    if let Some(page) = page {
        let index = PAGES.iter().position(|p| *p == page);
        ui.set_page(index.with_context(|| format!("no settings page {page:?}; try {}", PAGES.join(", ")))? as i32);
    }
    let state = Rc::new(RefCell::new(State::load(path, deck)?));
    show_status(&ui, status.as_ref());
    state.borrow_mut().status = status;
    state.borrow().refresh_all(&ui);
    connect(&ui, &state);
    watch_status(&ui, &state);
    bus.object_server().at(SETTINGS_PATH, Presenter { ui: ui.as_weak() })?;

    // Run until the window is closed, not until it's hidden: presenting it
    // hides it for a moment.
    ui.window().on_close_requested(|| {
        let _ = slint::quit_event_loop();
        slint::CloseRequestResponse::HideWindow
    });
    ui.show()?;
    slint::run_event_loop_until_quit()?;
    // Save typing that hasn't been saved yet.
    if state.borrow().dirty {
        state.borrow_mut().save(&ui);
    }
    Ok(())
}

/// Lets a second `--settings` bring this window forward.
struct Presenter {
    ui: slint::Weak<SettingsWindow>,
}

#[zbus::interface(name = "io.github.FunkyNERD.NerdDeck.Settings1")]
impl Presenter {
    /// Bring the window to the front, on `page` if it isn't empty.
    fn present(&self, page: String) {
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            if let Some(i) = PAGES.iter().position(|p| *p == page) {
                ui.set_page(i as i32);
            }
            // Wayland lets a window take focus only when it's mapped, so map
            // it again: on Wayland Slint makes a new window, which the
            // compositor raises and focuses; on X11 the window manager raises
            // a remapped window.
            let _ = ui.hide();
            let _ = ui.show();
        });
    }
}

#[zbus::proxy(
    interface = "io.github.FunkyNERD.NerdDeck.Settings1",
    default_service = "io.github.FunkyNERD.NerdDeck.Settings",
    default_path = "/io/github/FunkyNERD/NerdDeck/Settings"
)]
trait Settings {
    fn present(&self, page: &str) -> zbus::Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Icon,
    Image,
    Text,
}

#[derive(Debug, Clone)]
struct Button {
    id: String,
    label: String,
    kind: Kind,
    icon: Option<String>,
    image: Option<String>,
    command: String,
    /// "#RRGGBB"; the deck's default when None.
    color: Option<String>,
    /// Added in this window; its ID follows its label until the window closes.
    fresh: bool,
}

/// The deck's label colour when a button doesn't set one.
const DEFAULT_LABEL: u32 = 0xECEFF4;

/// The picker's presets: the deck's palette first, then common colours.
const SWATCHES: [u32; 14] = [
    0xECEFF4, 0xFFFFFF, 0xC3CAD5, 0x8792A2, 0x26C6DA, 0x66BB6A, 0xFFA726, 0xEF5350, 0xFFEE58, 0x42A5F5,
    0xAB47BC, 0xEC407A, 0x26A69A, 0x0B0E13,
];

fn slint_color(rgb: u32) -> slint::Color {
    slint::Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

struct State {
    path: PathBuf,
    doc: DocumentMut,
    cfg: Config,
    /// One per deck slot; buttons first, then empty slots.
    buttons: Vec<Option<Button>>,
    icon_filter: String,
    /// Previews by resolved path.
    images: HashMap<PathBuf, Option<slint::Image>>,
    status: Option<Status>,
    deck: NerdDeckProxyBlocking<'static>,
    save_timer: Timer,
    /// Edits waiting for the save timer.
    dirty: bool,
}

impl State {
    fn load(path: PathBuf, deck: NerdDeckProxyBlocking<'static>) -> Result<Self> {
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            // Saved on the first change, the way the service would write it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => config::EXAMPLE.to_owned(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let doc: DocumentMut = text.parse().with_context(|| format!("parsing {}", path.display()))?;
        let cfg: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let mut buttons: Vec<Option<Button>> = cfg
            .buttons
            .iter()
            .map(|b| {
                Some(Button {
                    id: b.id.clone(),
                    label: b.label.clone(),
                    kind: if b.image.is_some() {
                        Kind::Image
                    } else if b.icon.is_some() {
                        Kind::Icon
                    } else {
                        Kind::Text
                    },
                    icon: b.icon.clone(),
                    image: b.image.as_ref().map(|p| p.display().to_string()),
                    command: b.command.clone(),
                    color: b.color.as_deref().and_then(config::parse_color).map(|c| format!("#{c:06X}")),
                    fresh: false,
                })
            })
            .collect();
        buttons.resize(MAX_BUTTONS, None);
        Ok(Self {
            path,
            doc,
            cfg,
            buttons,
            icon_filter: String::new(),
            images: HashMap::new(),
            status: None,
            deck,
            save_timer: Timer::default(),
            dirty: false,
        })
    }

    fn config_dir(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }

    // ---- Writing the config ----

    /// Write the edits to the file and ask the service to apply them. A config
    /// that wouldn't load is never written.
    fn save(&mut self, ui: &SettingsWindow) {
        self.dirty = false;
        self.write_buttons();
        let text = self.doc.to_string();
        let checked = toml::from_str::<Config>(&text)
            .map_err(anyhow::Error::from)
            .and_then(|cfg| cfg.validate().map(|()| cfg));
        let cfg = match checked {
            Ok(cfg) => cfg,
            Err(e) => return problem(ui, format!("Not saved: {e:#}")),
        };
        if let Err(e) = write_atomically(&self.path, &text) {
            return problem(ui, format!("Couldn't save {}: {e}", self.path.display()));
        }
        self.cfg = cfg;
        if self.status.is_some() {
            let deck = self.deck.clone();
            call(ui, move || deck.reload());
        }
    }

    fn save_later(&mut self, ui: &SettingsWindow, state: &Rc<RefCell<State>>) {
        self.dirty = true;
        let (ui, state) = (ui.as_weak(), Rc::downgrade(state));
        self.save_timer.start(TimerMode::SingleShot, SAVE_DELAY, move || {
            if let (Some(ui), Some(state)) = (ui.upgrade(), state.upgrade()) {
                state.borrow_mut().save(&ui);
            }
        });
    }

    fn set(&mut self, table: &str, key: &str, value: impl Into<Value>) {
        ensure_table(&mut self.doc, table);
        let mut value = value.into();
        let item = &mut self.doc[table][key];
        // Keep the comment after the value.
        if let Some(old) = item.as_value() {
            *value.decor_mut() = old.decor().clone();
        }
        *item = Item::Value(value);
    }

    fn remove(&mut self, table: &str, key: &str) {
        if let Some(t) = self.doc.get_mut(table).and_then(Item::as_table_like_mut) {
            t.remove(key);
        }
    }

    fn set_list(&mut self, table: &str, key: &str, list: &[String]) {
        self.set(table, key, list.iter().map(String::as_str).collect::<Array>());
    }

    fn write_buttons(&mut self) {
        write_buttons(&mut self.doc, &self.buttons);
    }

    // ---- Buttons ----

    /// Buttons first: the config is a list, so the deck has no gaps.
    fn compact(&mut self) {
        let mut buttons: Vec<_> = self.buttons.drain(..).flatten().map(Some).collect();
        buttons.resize(MAX_BUTTONS, None);
        self.buttons = buttons;
    }

    fn unique_id(&self, label: &str, skip: usize) -> String {
        let mut base: String = label
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>()
            .split('_')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("_");
        if base.is_empty() {
            base = "button".into();
        }
        base.truncate(MAX_ID_LEN - 3);
        let taken = |id: &str| {
            self.buttons
                .iter()
                .enumerate()
                .any(|(i, b)| i != skip && b.as_ref().is_some_and(|b| b.id == id))
        };
        let mut id = base.clone();
        let mut n = 2;
        while taken(&id) {
            id = format!("{base}_{n}");
            n += 1;
        }
        id
    }

    fn image(&mut self, path: &str) -> Option<slint::Image> {
        let path = config::resolve_path(self.config_dir(), Path::new(path));
        self.images.entry(path.clone()).or_insert_with(|| preview(&path)).clone()
    }

    fn deck_button(&mut self, slot: usize) -> DeckButton {
        let Some(b) = self.buttons[slot].clone() else {
            return DeckButton { empty: true, ..Default::default() };
        };
        let image = b.image.as_deref().and_then(|p| self.image(p));
        DeckButton {
            label: b.label.into(),
            id: b.id.into(),
            kind: b.kind as i32,
            glyph: b.icon.as_deref().and_then(icons::lookup).map(String::from).unwrap_or_default().into(),
            icon: b.icon.unwrap_or_default().into(),
            has_image: image.is_some(),
            image: image.unwrap_or_default(),
            image_path: b.image.unwrap_or_default().into(),
            command: b.command.into(),
            empty: false,
            has_color: b.color.is_some(),
            color: slint_color(b.color.as_deref().and_then(config::parse_color).unwrap_or(DEFAULT_LABEL)),
            color_hex: b.color.unwrap_or_default().into(),
        }
    }

    // ---- Stats ----

    fn disk_rows(&self) -> Vec<DiskRow> {
        let disks = sysinfo::Disks::new_with_refreshed_list();
        let mut rows: Vec<DiskRow> = Vec::new();
        let mut add = |mount: &str, on: bool| {
            if rows.iter().any(|r| r.mount == mount) {
                return;
            }
            let disk = disks.iter().find(|d| d.mount_point() == Path::new(mount));
            let (usage, percent) = match disk {
                Some(d) if d.total_space() > 0 => {
                    let used = d.total_space() - d.available_space();
                    let percent = used as f32 / d.total_space() as f32;
                    (
                        format!("{} of {} · {:.0}%", size(used), size(d.total_space()), percent * 100.0),
                        percent,
                    )
                }
                _ => ("Not mounted".into(), 0.0),
            };
            rows.push(DiskRow {
                title: disk_title(mount).into(),
                mount: mount.into(),
                usage: usage.into(),
                percent,
                on,
            });
        };
        for m in &self.cfg.stats.mounts {
            add(m, true);
        }
        let mut others: Vec<_> = disks
            .iter()
            .filter(|d| !collector::is_removable(d))
            .map(|d| d.mount_point().display().to_string())
            .filter(|m| !m.starts_with("/boot") && !m.starts_with("/snap"))
            .collect();
        others.sort();
        for m in others {
            add(&m, false);
        }
        rows
    }

    // ---- Display ----

    fn refresh_display(&self, ui: &SettingsWindow) {
        let b = ui.global::<Backend>();
        let d = &self.cfg.display;
        let order = d.tab_order();
        b.set_brightness(d.brightness.into());

        let first = tab_info(order[0]).0;
        let starts: Vec<SharedString> = std::iter::once(format!("First tab ({first})"))
            .chain(config::TABS.iter().map(|t| tab_info(t).0.to_owned()))
            .map(Into::into)
            .collect();
        b.set_start_options(ModelRc::new(VecModel::from(starts)));
        let start = config::TABS.iter().position(|t| *t == d.start).map_or(0, |i| i + 1);
        b.set_start_index(start as i32);

        let (dims, index) = minutes_choices(&DIM_AFTER, d.dim_after);
        b.set_dim_options(ModelRc::new(VecModel::from(dims)));
        b.set_dim_index(index);
        let (offs, index) = minutes_choices(&OFF_AFTER, d.off_after);
        b.set_off_options(ModelRc::new(VecModel::from(offs)));
        b.set_off_index(index);

        let opens = if d.start == "first" { order[0] } else { d.start.as_str() };
        let rows: Vec<TabRow> = order
            .iter()
            .map(|id| {
                let (name, desc, glyph) = tab_info(id);
                TabRow {
                    name: name.into(),
                    desc: desc.into(),
                    glyph: glyph.into(),
                    start: *id == opens,
                    laptop: *id == "battery",
                }
            })
            .collect();
        b.set_tabs(ModelRc::new(VecModel::from(rows)));
    }

    // ---- Notifications ----

    /// Apps that have sent notifications, then any the config names that haven't.
    fn app_rows(&self) -> Vec<(SeenApp, bool)> {
        let n = &self.cfg.notifications;
        let mut apps: Vec<SeenApp> = SeenApps::load().list().to_vec();
        for name in n.apps.iter().chain(&n.ignore) {
            if !apps.iter().any(|a| app_matches(a, name)) {
                apps.push(SeenApp { name: name.clone(), app_id: String::new(), last: 0 });
            }
        }
        apps.into_iter()
            .map(|a| {
                let shown = if n.apps.is_empty() {
                    !n.ignore.iter().any(|i| app_matches(&a, i))
                } else {
                    n.apps.iter().any(|i| app_matches(&a, i))
                };
                (a, shown)
            })
            .collect()
    }

    // ---- Filling the window ----

    fn refresh_all(&self, ui: &SettingsWindow) {
        let b = ui.global::<Backend>();
        let cfg = &self.cfg;

        // Buttons and their previews need &mut self for the image cache, so
        // they're filled by refresh_buttons.
        b.set_icons(self.icon_model());

        let interval = cfg.stats.interval_ms;
        let mut intervals: Vec<SharedString> = INTERVALS.iter().map(|(_, l)| (*l).into()).collect();
        let index = match INTERVALS.iter().position(|(ms, _)| *ms == interval) {
            Some(i) => i,
            None => {
                intervals.push(format!("{interval} ms").into());
                intervals.len() - 1
            }
        };
        b.set_intervals(ModelRc::new(VecModel::from(intervals)));
        b.set_interval_index(index as i32);

        let networks = sysinfo::Networks::new_with_refreshed_list();
        let mut names: Vec<String> = networks.iter().map(|(n, _)| n.clone()).filter(|n| n != "lo").collect();
        names.sort();
        if let Some(i) = &cfg.stats.interface {
            if !names.contains(i) {
                names.push(i.clone());
            }
        }
        let index = cfg.stats.interface.as_ref().and_then(|i| names.iter().position(|n| n == i)).map_or(0, |i| i + 1);
        let interfaces: Vec<SharedString> = std::iter::once("Busiest".to_owned()).chain(names).map(Into::into).collect();
        b.set_interfaces(ModelRc::new(VecModel::from(interfaces)));
        b.set_interface_index(index as i32);

        b.set_disks(ModelRc::new(VecModel::from(self.disk_rows())));
        b.set_battery_note(
            match battery::read() {
                Some(bat) if bat.charging => format!("{}%, charging. Hidden on PCs without one.", bat.percent),
                Some(bat) => format!("{}%. Hidden on PCs without one.", bat.percent),
                None => "No battery found on this PC.".into(),
            }
            .into(),
        );
        b.set_removable(cfg.stats.removable);
        b.set_battery(cfg.stats.battery);

        let n = &cfg.notifications;
        b.set_notify_enabled(n.enabled);
        b.set_notify_all(n.apps.is_empty());
        b.set_notify_body(n.body);
        b.set_notify_dnd(n.dnd);
        b.set_notify_skip_low(n.skip_low);
        let now = unix_now();
        let apps: Vec<AppRow> = self
            .app_rows()
            .into_iter()
            .map(|(a, shown)| AppRow {
                name: a.name.into(),
                app_id: a.app_id.into(),
                seen: if a.last == 0 { "not seen yet".into() } else { ago(now.saturating_sub(a.last)).into() },
                shown,
            })
            .collect();
        b.set_apps(ModelRc::new(VecModel::from(apps)));

        self.refresh_display(ui);

        b.set_auto_firmware(cfg.updates.firmware);
        b.set_check_updates(cfg.updates.check);
        self.refresh_known(ui);
        b.set_sysinfo(self.sysinfo().into());
    }

    fn refresh_buttons(&mut self, ui: &SettingsWindow) {
        let rows: Vec<DeckButton> = (0..MAX_BUTTONS).map(|i| self.deck_button(i)).collect();
        let b = ui.global::<Backend>();
        // Update rows in place, so the grid doesn't rebuild under the pointer.
        let model = b.get_buttons();
        match model.as_any().downcast_ref::<VecModel<DeckButton>>() {
            Some(m) if m.row_count() == rows.len() => {
                for (i, row) in rows.into_iter().enumerate() {
                    if m.row_data(i).as_ref() != Some(&row) {
                        m.set_row_data(i, row);
                    }
                }
            }
            _ => b.set_buttons(ModelRc::new(VecModel::from(rows))),
        }
    }

    fn icon_model(&self) -> ModelRc<IconChoice> {
        let filter = self.icon_filter.to_lowercase().replace(' ', "_");
        let icons: Vec<IconChoice> = icons::ICONS
            .iter()
            .filter(|(name, _)| filter.is_empty() || name.contains(&filter))
            .map(|(name, glyph)| IconChoice { name: (*name).into(), glyph: glyph.to_string().into() })
            .collect();
        ModelRc::new(VecModel::from(icons))
    }

    fn refresh_known(&self, ui: &SettingsWindow) {
        let connected = self.status.as_ref().filter(|s| s.connected).map(|s| s.serial.clone());
        let known: Vec<KnownDeck> = KnownDevices::load()
            .serials()
            .map(|serial| KnownDeck {
                serial: serial.into(),
                note: if connected.as_deref() == Some(serial) { "Connected now" } else { "Not connected" }.into(),
            })
            .collect();
        ui.global::<Backend>().set_known(ModelRc::new(VecModel::from(known)));
    }

    fn sysinfo(&self) -> String {
        let mut lines = vec![format!("nerd-deck {} ({})", env!("CARGO_PKG_VERSION"), std::env::consts::ARCH)];
        match &self.status {
            Some(s) if s.connected => lines.push(format!(
                "deck: firmware {} · protocol v{} · {}",
                or_dash(&s.firmware),
                s.proto,
                or_dash(&s.board)
            )),
            Some(_) => lines.push("deck: not connected".into()),
            None => lines.push("service: not running".into()),
        }
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown".into());
        let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into());
        let os = sysinfo::System::long_os_version().unwrap_or_else(|| "Linux".into());
        lines.push(format!("system: {os} · {desktop} ({session})"));
        lines.join("\n")
    }
}

/// Replace the [[button]] tables, keeping each surviving button's comments.
fn write_buttons(doc: &mut DocumentMut, buttons: &[Option<Button>]) {
    let old: Vec<Table> = doc
        .get("button")
        .and_then(Item::as_array_of_tables)
        .map(|a| a.iter().cloned().collect())
        .unwrap_or_default();
    let mut tables = ArrayOfTables::new();
    for (i, b) in buttons.iter().flatten().enumerate() {
        let found = old.iter().position(|t| t.get("id").and_then(Item::as_str) == Some(b.id.as_str()));
        let prev = found.map(|j| &old[j]);
        let mut t = Table::new();
        // The comments above the first [[button]] introduce the section, so
        // they stay at the top whichever button comes first.
        match (i, found) {
            (0, _) => {
                if let Some(first) = old.first() {
                    *t.decor_mut() = first.decor().clone();
                }
            }
            (_, Some(j)) if j > 0 => *t.decor_mut() = old[j].decor().clone(),
            _ => {}
        }
        let mut fields = vec![("id", b.id.as_str()), ("label", b.label.as_str())];
        match (b.kind, &b.icon, &b.image) {
            (Kind::Icon, Some(icon), _) => fields.push(("icon", icon)),
            (Kind::Image, _, Some(image)) => fields.push(("image", image)),
            _ => {}
        }
        if let Some(color) = &b.color {
            fields.push(("color", color));
        }
        fields.push(("command", &b.command));
        // Keys in the order the file had them; new ones after.
        let rank = |key: &str| prev.and_then(|p| p.iter().position(|(k, _)| k == key)).unwrap_or(usize::MAX);
        fields.sort_by_key(|(key, _)| rank(key));
        for (key, value) in fields {
            put(&mut t, prev, key, value);
        }
        tables.push(t);
    }
    doc.remove("button");
    if !tables.is_empty() {
        doc.insert("button", Item::ArrayOfTables(tables));
    }
}

/// Set a key in `table`, keeping the old key's comments and the value's.
fn put(table: &mut Table, prev: Option<&Table>, key: &str, value: &str) {
    let mut value = Value::from(value);
    match prev.and_then(|p| p.get_key_value(key)) {
        Some((old_key, old)) => {
            if let Some(old) = old.as_value() {
                *value.decor_mut() = old.decor().clone();
            }
            table.insert_formatted(old_key, Item::Value(value));
        }
        None => {
            table.insert(key, Item::Value(value));
        }
    }
}

/// Makes `name` a `[name]` section if it isn't one: a new one goes before the
/// buttons, and an inline `name = { ... }` (written by 0.5.1's window) becomes one.
fn ensure_table(doc: &mut DocumentMut, name: &str) {
    if doc.get(name).is_some_and(Item::is_table) {
        return;
    }
    let mut table = match doc.remove(name) {
        Some(Item::Value(Value::InlineTable(t))) => t.into_table(),
        _ => Table::new(),
    };
    table.decor_mut().set_prefix("\n");
    let buttons = doc.get("button").and_then(Item::as_array_of_tables).and_then(|a| a.get(0)).and_then(Table::position);
    if let Some(at) = buttons {
        // Make room: every section from the first button on moves down one.
        for (_, item) in doc.iter_mut() {
            match item {
                Item::Table(t) => shift(t, at),
                Item::ArrayOfTables(a) => a.iter_mut().for_each(|t| shift(t, at)),
                _ => {}
            }
        }
        table.set_position(Some(at));
    }
    doc.insert(name, Item::Table(table));
}

fn shift(t: &mut Table, from: isize) {
    if let Some(p) = t.position().filter(|p| *p >= from) {
        t.set_position(Some(p + 1));
    }
}

fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// A preview of a button image, or None if it can't be read.
fn preview(path: &Path) -> Option<slint::Image> {
    let img = image::open(path).ok()?.thumbnail(96, 96).to_rgba8();
    let buffer = SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(img.as_raw(), img.width(), img.height());
    Some(slint::Image::from_rgba8(buffer))
}

fn app_matches(app: &SeenApp, entry: &str) -> bool {
    let entry = entry.to_lowercase();
    let id = app.app_id.to_lowercase();
    let id = id.strip_suffix(".desktop").unwrap_or(&id);
    app.name.to_lowercase() == entry || (!id.is_empty() && (id == entry || id.rsplit('.').next() == Some(&entry)))
}

fn disk_title(mount: &str) -> String {
    match mount {
        "/" => "Root".into(),
        "/home" => "Home".into(),
        m => {
            let last = m.rsplit('/').next().unwrap_or(m);
            let mut c = last.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_else(|| m.to_owned())
        }
    }
}

fn size(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1000.0 {
        format!("{:.1} TB", gb / 1000.0)
    } else {
        format!("{gb:.0} GB")
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn ago(secs: u64) -> String {
    match secs {
        0..=59 => "just now".into(),
        60..=119 => "a minute ago".into(),
        120..=3599 => format!("{} minutes ago", secs / 60),
        3600..=7199 => "an hour ago".into(),
        7200..=86399 => format!("{} hours ago", secs / 3600),
        86400..=172799 => "yesterday".into(),
        _ => format!("{} days ago", secs / 86400),
    }
}

fn or_dash(s: &str) -> &str {
    if s.is_empty() {
        "—"
    } else {
        s
    }
}

fn problem(ui: &SettingsWindow, text: String) {
    log::warn!("{text}");
    ui.global::<Backend>().set_problem(text.into());
}

/// Run a service call off the UI thread; show a failure in the header.
fn call(ui: &SettingsWindow, f: impl FnOnce() -> zbus::Result<()> + Send + 'static) {
    let ui = ui.as_weak();
    thread::spawn(move || {
        if let Err(e) = f() {
            let msg = match e {
                zbus::Error::MethodError(_, Some(msg), _) => msg,
                zbus::Error::MethodError(name, None, _) if name.contains("ServiceUnknown") => {
                    "The nerd-deck service isn't running".into()
                }
                e => e.to_string(),
            };
            let _ = ui.upgrade_in_event_loop(move |ui| problem(&ui, msg));
        }
    });
}

fn show_status(ui: &SettingsWindow, status: Option<&Status>) {
    let b = ui.global::<Backend>();
    let s = status.cloned().unwrap_or_default();
    b.set_service_running(status.is_some());
    b.set_connected(s.connected);
    b.set_port(s.port.into());
    b.set_serial(s.serial.into());
    b.set_firmware(s.firmware.into());
    b.set_board(s.board.into());
    b.set_proto(s.proto as i32);
    b.set_bundled(
        if s.bundled.is_empty() { crate::firmware::bundled().map(|f| f.version.to_owned()).unwrap_or_default() } else { s.bundled }
            .into(),
    );
    b.set_version(if s.version.is_empty() { env!("CARGO_PKG_VERSION").into() } else { s.version }.into());
    b.set_update(s.update.into());
    b.set_flashing(s.flashing);
    b.set_display_unsupported(s.connected && (s.proto as i32) < DISPLAY_PROTO);
}

/// Follow the service's status: its signal, plus a slow poll.
fn watch_status(ui: &SettingsWindow, state: &Rc<RefCell<State>>) {
    let push = {
        let ui = ui.as_weak();
        move |status: Option<Status>| {
            let _ = ui.upgrade_in_event_loop(move |ui| {
                // Runs on the UI thread, where the state lives.
                STATE.with(|s| {
                    if let Some(state) = s.borrow().as_ref().and_then(|w| w.upgrade()) {
                        let mut state = state.borrow_mut();
                        if state.status != status {
                            show_status(&ui, status.as_ref());
                            state.status = status;
                            state.refresh_known(&ui);
                            ui.global::<Backend>().set_sysinfo(state.sysinfo().into());
                        }
                    }
                });
            });
        }
    };
    STATE.with(|s| *s.borrow_mut() = Some(Rc::downgrade(state)));

    let deck = state.borrow().deck.clone();
    {
        let (deck, push) = (deck.clone(), push.clone());
        thread::spawn(move || {
            if let Ok(signals) = deck.receive_status_changed() {
                signals.for_each(|_| push(deck.status().ok()));
            }
        });
    }
    thread::spawn(move || loop {
        thread::sleep(STATUS_POLL);
        push(deck.status().ok());
    });
}

thread_local! {
    static STATE: RefCell<Option<std::rc::Weak<RefCell<State>>>> = const { RefCell::new(None) };
}

/// Wire the window's requests to the config and the service.
fn connect(ui: &SettingsWindow, state: &Rc<RefCell<State>>) {
    let b = ui.global::<Backend>();
    state.borrow_mut().refresh_buttons(ui);
    let swatches: Vec<Swatch> =
        SWATCHES.iter().map(|&c| Swatch { color: slint_color(c), hex: format!("#{c:06X}").into() }).collect();
    b.set_swatches(ModelRc::new(VecModel::from(swatches)));

    // Each handler gets the window and the state, and runs on the UI thread.
    macro_rules! on {
        ($setter:ident, |$ui:ident, $st:ident $(, $arg:ident)*| $body:block) => {{
            let ui_weak = ui.as_weak();
            let state = state.clone();
            b.$setter(move |$($arg),*| {
                let Some($ui) = ui_weak.upgrade() else { return };
                let mut guard = state.borrow_mut();
                #[allow(unused_variables)]
                let $st: &mut State = &mut guard;
                $body
            });
        }};
    }

    // ---- Buttons ----
    on!(on_add_button, |ui, st, _slot| {
        let Some(slot) = st.buttons.iter().position(Option::is_none) else { return };
        let id = st.unique_id("New button", slot);
        st.buttons[slot] = Some(Button {
            id,
            label: "New button".into(),
            kind: Kind::Icon,
            icon: Some("bell".into()),
            image: None,
            command: String::new(),
            color: None,
            fresh: true,
        });
        ui.global::<Backend>().set_selected(slot as i32);
        st.refresh_buttons(&ui);
        st.save(&ui);
    });
    on!(on_remove_button, |ui, st, slot| {
        st.buttons[slot as usize] = None;
        st.compact();
        st.refresh_buttons(&ui);
        st.save(&ui);
    });
    on!(on_move_button, |ui, st, slot, delta| {
        let (from, to) = (slot as usize, (slot + delta) as usize);
        if to >= MAX_BUTTONS || st.buttons[to].is_none() {
            return;
        }
        st.buttons.swap(from, to);
        ui.global::<Backend>().set_selected(to as i32);
        st.refresh_buttons(&ui);
        st.save(&ui);
    });
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        b.on_set_label(move |slot, text| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let slot = slot as usize;
            let mut label = text.to_string();
            while label.len() > MAX_LABEL_LEN {
                label.pop();
            }
            let fresh_id = match &st.buttons[slot] {
                Some(b) if b.fresh => Some(st.unique_id(&label, slot)),
                _ => None,
            };
            if let Some(b) = st.buttons[slot].as_mut() {
                b.label = label;
                if let Some(id) = fresh_id {
                    b.id = id;
                }
            }
            st.refresh_buttons(&ui);
            st.save_later(&ui, &state);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        b.on_set_command(move |slot, text| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            if let Some(b) = st.buttons[slot as usize].as_mut() {
                b.command = text.to_string();
            }
            st.refresh_buttons(&ui);
            st.save_later(&ui, &state);
        });
    }
    on!(on_set_kind, |ui, st, slot, kind| {
        if let Some(b) = st.buttons[slot as usize].as_mut() {
            b.kind = match kind {
                0 => Kind::Icon,
                1 => Kind::Image,
                _ => Kind::Text,
            };
            if b.kind == Kind::Icon && b.icon.is_none() {
                b.icon = Some("bell".into());
            }
        }
        st.refresh_buttons(&ui);
        st.save(&ui);
    });
    on!(on_set_icon, |ui, st, slot, name| {
        if let Some(b) = st.buttons[slot as usize].as_mut() {
            b.kind = Kind::Icon;
            b.icon = Some(name.to_string());
        }
        st.refresh_buttons(&ui);
        st.save(&ui);
    });
    on!(on_set_color, |ui, st, slot, text| {
        let text = text.trim();
        let color = if text.is_empty() {
            None
        } else {
            match config::parse_color(text) {
                Some(rgb) => Some(format!("#{rgb:06X}")),
                None => return problem(&ui, format!("{text:?} isn't a colour like #FFAA00")),
            }
        };
        if let Some(b) = st.buttons[slot as usize].as_mut() {
            b.color = color;
        }
        st.refresh_buttons(&ui);
        st.save(&ui);
    });
    on!(on_filter_icons, |ui, st, text| {
        st.icon_filter = text.to_string();
        ui.global::<Backend>().set_icons(st.icon_model());
    });
    on!(on_choose_image, |ui, st, slot| {
        let start = st.config_dir().to_path_buf();
        let ui_weak = ui.as_weak();
        thread::spawn(move || {
            let picked = pick_image(&start);
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                STATE.with(|s| {
                    let Some(state) = s.borrow().as_ref().and_then(|w| w.upgrade()) else { return };
                    let mut st = state.borrow_mut();
                    match picked {
                        Ok(Some(path)) => match images::load(&path) {
                            Ok(_) => {
                                let shown = tilde(&path);
                                st.images.remove(&path);
                                if let Some(b) = st.buttons[slot as usize].as_mut() {
                                    b.kind = Kind::Image;
                                    b.image = Some(shown);
                                }
                                st.refresh_buttons(&ui);
                                st.save(&ui);
                            }
                            Err(e) => problem(&ui, format!("{e:#}")),
                        },
                        Ok(None) => {}
                        Err(e) => problem(&ui, format!("{e:#}")),
                    }
                });
            });
        });
    });
    on!(on_run_command, |ui, st, slot| {
        let Some(command) = st.buttons[slot as usize].as_ref().map(|b| b.command.clone()) else { return };
        let b = ui.global::<Backend>();
        b.set_running(true);
        b.set_run_result("".into());
        let ui_weak = ui.as_weak();
        thread::spawn(move || {
            let started = Instant::now();
            let result = Command::new("sh").arg("-c").arg(&command).status();
            let ms = started.elapsed().as_millis();
            let (ok, text) = match result {
                Ok(s) if s.success() => (true, format!("Exit 0 · {ms} ms")),
                Ok(s) => (false, match s.code() {
                    Some(code) => format!("Exit {code} · {ms} ms"),
                    None => "Stopped by a signal".into(),
                }),
                Err(e) => (false, format!("Couldn't run it: {e}")),
            };
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                let b = ui.global::<Backend>();
                b.set_running(false);
                b.set_run_ok(ok);
                b.set_run_result(text.into());
            });
        });
    });

    // ---- Stats ----
    on!(on_set_interval, |ui, st, index| {
        let Some((ms, _)) = INTERVALS.get(index as usize) else { return };
        st.set("stats", "interval_ms", *ms as i64);
        st.save(&ui);
    });
    on!(on_set_interface, |ui, st, index| {
        if index == 0 {
            st.remove("stats", "interface");
        } else {
            let name = ui.global::<Backend>().get_interfaces().row_data(index as usize).unwrap_or_default();
            st.set("stats", "interface", name.as_str());
        }
        st.save(&ui);
    });
    on!(on_set_disk, |ui, st, index, on| {
        let rows = st.disk_rows();
        let mut mounts: Vec<String> = rows.iter().filter(|r| r.on).map(|r| r.mount.to_string()).collect();
        let mount = rows[index as usize].mount.to_string();
        if on {
            if mounts.len() >= MAX_DISKS {
                return problem(&ui, format!("The deck shows up to {MAX_DISKS} disks; turn one off first"));
            }
            mounts.push(mount);
        } else {
            mounts.retain(|m| *m != mount);
        }
        st.set_list("stats", "mounts", &mounts);
        st.save(&ui);
        ui.global::<Backend>().set_disks(ModelRc::new(VecModel::from(st.disk_rows())));
    });
    on!(on_move_disk, |ui, st, index, delta| {
        let mut mounts = st.cfg.stats.mounts.clone();
        let (from, to) = (index as usize, (index + delta) as usize);
        if to >= mounts.len() {
            return;
        }
        mounts.swap(from, to);
        st.set_list("stats", "mounts", &mounts);
        st.save(&ui);
        ui.global::<Backend>().set_disks(ModelRc::new(VecModel::from(st.disk_rows())));
    });

    // ---- Display ----
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        b.on_set_brightness(move |percent| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let percent = percent.clamp(5, 100);
            st.set("display", "brightness", percent as i64);
            ui.global::<Backend>().set_brightness(percent);
            // The slider sends a value per step; save once it settles.
            st.save_later(&ui, &state);
        });
    }
    on!(on_set_start, |ui, st, index| {
        let start = if index <= 0 { "first" } else { config::TABS.get(index as usize - 1).copied().unwrap_or("first") };
        st.set("display", "start", start);
        st.save(&ui);
        st.refresh_display(&ui);
    });
    on!(on_set_dim, |ui, st, index| {
        let Some((minutes, _)) = DIM_AFTER.get(index as usize) else { return };
        st.set("display", "dim_after", *minutes as i64);
        st.save(&ui);
    });
    on!(on_set_off, |ui, st, index| {
        let Some((minutes, _)) = OFF_AFTER.get(index as usize) else { return };
        st.set("display", "off_after", *minutes as i64);
        st.save(&ui);
    });
    on!(on_move_tab, |ui, st, index, delta| {
        let mut order: Vec<String> = st.cfg.display.tab_order().iter().map(|t| (*t).to_owned()).collect();
        let (from, to) = (index as usize, (index + delta) as usize);
        if to >= order.len() {
            return;
        }
        order.swap(from, to);
        st.set_list("display", "tabs", &order);
        st.save(&ui);
        st.refresh_display(&ui);
    });
    on!(on_reset_tabs, |ui, st| {
        st.remove("display", "tabs");
        st.save(&ui);
        st.refresh_display(&ui);
    });

    // ---- Switches ----
    on!(on_set_flag, |ui, st, key, on| {
        match key.as_str() {
            "notifications.all" => {
                // Carry the shown apps over: "all" hides the hidden ones,
                // "only" shows the shown ones.
                let rows = st.app_rows();
                if on {
                    let hidden: Vec<String> = rows.iter().filter(|(_, s)| !s).map(|(a, _)| a.name.clone()).collect();
                    st.set_list("notifications", "apps", &[]);
                    st.set_list("notifications", "ignore", &hidden);
                } else {
                    let shown: Vec<String> = rows.iter().filter(|(_, s)| *s).map(|(a, _)| a.name.clone()).collect();
                    if shown.is_empty() {
                        return problem(&ui, "No apps have sent a notification yet, so there are none to pick".into());
                    }
                    st.set_list("notifications", "apps", &shown);
                    st.set_list("notifications", "ignore", &[]);
                }
            }
            key => {
                let Some((table, name)) = key.split_once('.') else { return };
                st.set(table, name, on);
            }
        }
        st.save(&ui);
        st.refresh_all(&ui);
    });
    on!(on_set_app_shown, |ui, st, index, shown| {
        let rows = st.app_rows();
        let Some((app, _)) = rows.get(index as usize) else { return };
        let n = &st.cfg.notifications;
        if n.apps.is_empty() {
            let mut ignore: Vec<String> = n.ignore.iter().filter(|e| !app_matches(app, e)).cloned().collect();
            if !shown {
                ignore.push(app.name.clone());
            }
            st.set_list("notifications", "ignore", &ignore);
        } else {
            let mut apps: Vec<String> = n.apps.iter().filter(|e| !app_matches(app, e)).cloned().collect();
            if shown {
                apps.push(app.name.clone());
            } else if apps.is_empty() {
                return problem(&ui, "Keep at least one app, or turn notifications off".into());
            }
            st.set_list("notifications", "apps", &apps);
        }
        st.save(&ui);
        st.refresh_all(&ui);
    });
    on!(on_test_notification, |ui, st, critical| {
        let deck = st.deck.clone();
        call(&ui, move || deck.test_notification(critical));
    });

    // ---- Firmware ----
    on!(on_reinstall_firmware, |ui, st| {
        let deck = st.deck.clone();
        call(&ui, move || deck.reinstall_firmware());
    });
    on!(on_forget_deck, |ui, st, index| {
        let mut known = KnownDevices::load();
        let Some(serial) = known.serials().nth(index as usize).map(String::from) else { return };
        known.remove(&serial);
        st.refresh_known(&ui);
        if st.status.is_some() {
            let deck = st.deck.clone();
            call(&ui, move || deck.reload());
        }
    });

    b.on_open_url(|url| {
        if let Err(e) = Command::new("xdg-open").arg(url.as_str()).spawn() {
            log::warn!("opening {url}: {e}");
        }
    });
}

/// Ask the desktop for an image file: KDE's dialog, else GNOME's.
fn pick_image(start: &Path) -> Result<Option<PathBuf>> {
    let start = start.display().to_string();
    let output = if which("kdialog") {
        Command::new("kdialog")
            .args(["--title", "Choose a button image", "--getopenfilename", &start])
            .arg("Images (*.png *.jpg *.jpeg *.bmp)")
            .output()
    } else if which("zenity") {
        Command::new("zenity")
            .args(["--file-selection", "--title=Choose a button image"])
            .arg(format!("--filename={start}/"))
            .arg("--file-filter=Images | *.png *.jpg *.jpeg *.bmp")
            .output()
    } else {
        bail!("No file dialog found; install kdialog or zenity, or type the path into the config");
    }
    .context("opening the file dialog")?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok((output.status.success() && !path.is_empty()).then(|| PathBuf::from(path)))
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// `~/…` for paths in the home directory, as the config writes them.
fn tilde(path: &Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_disks() {
        assert_eq!(disk_title("/"), "Root");
        assert_eq!(disk_title("/mnt/data"), "Data");
    }

    #[test]
    fn matches_apps_like_the_filter() {
        let geary = SeenApp { name: "Geary".into(), app_id: "org.gnome.Geary".into(), last: 0 };
        assert!(app_matches(&geary, "geary"));
        assert!(app_matches(&geary, "org.gnome.Geary"));
        assert!(!app_matches(&geary, "Thunderbird"));
    }

    fn button(id: &str, label: &str, icon: &str) -> Option<Button> {
        Some(Button {
            id: id.into(),
            label: label.into(),
            kind: Kind::Icon,
            icon: Some(icon.into()),
            image: None,
            command: "true".into(),
            color: None,
            fresh: false,
        })
    }

    #[test]
    fn rewrites_buttons_keeping_the_rest() {
        let mut doc: DocumentMut = config::EXAMPLE.parse().unwrap();
        // Reorder two, drop the rest, add one.
        let buttons = [button("mute", "Mute", "mute"), button("lock", "Lock", "eye_close"), button("new", "New", "bell")];
        write_buttons(&mut doc, &buttons);
        let text = doc.to_string();
        let cfg: Config = toml::from_str(&text).unwrap();
        cfg.validate().unwrap();
        let ids: Vec<_> = cfg.buttons.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["mute", "lock", "new"]);
        // Everything outside the buttons is untouched, comments included.
        let head = |t: &str| t[..t.find("[[button]]").unwrap()].to_owned();
        assert_eq!(head(&text), head(config::EXAMPLE));
    }

    #[test]
    fn unchanged_buttons_leave_the_file_alone() {
        let mut doc: DocumentMut = config::EXAMPLE.parse().unwrap();
        let cfg: Config = toml::from_str(config::EXAMPLE).unwrap();
        let buttons: Vec<_> = cfg
            .buttons
            .iter()
            .map(|b| {
                Some(Button {
                    id: b.id.clone(),
                    label: b.label.clone(),
                    kind: if b.icon.is_some() { Kind::Icon } else { Kind::Text },
                    icon: b.icon.clone(),
                    image: None,
                    command: b.command.clone(),
                    color: None,
                    fresh: false,
                })
            })
            .collect();
        write_buttons(&mut doc, &buttons);
        assert_eq!(doc.to_string(), config::EXAMPLE);
    }

    #[test]
    fn new_sections_go_before_the_buttons() {
        let mut doc: DocumentMut = "[stats]\nbattery = true\n\n[[button]]\nid = 'a'\n".parse().unwrap();
        ensure_table(&mut doc, "display");
        doc["display"]["brightness"] = toml_edit::value(70);
        assert_eq!(
            doc.to_string(),
            "[stats]\nbattery = true\n\n[display]\nbrightness = 70\n\n[[button]]\nid = 'a'\n"
        );

        let mut doc: DocumentMut = "display = { brightness = 70 }\n[stats]\n".parse().unwrap();
        ensure_table(&mut doc, "display");
        let cfg: Config = toml::from_str(&doc.to_string()).unwrap();
        assert_eq!(cfg.display.brightness, 70);
        assert!(doc.to_string().contains("[display]"));
    }

    #[test]
    fn edits_keep_comments() {
        let mut doc: DocumentMut = "[stats]\ninterval_ms = 1000 # keep me\n".parse().unwrap();
        let mut value = Value::from(500i64);
        *value.decor_mut() = doc["stats"]["interval_ms"].as_value().unwrap().decor().clone();
        doc["stats"]["interval_ms"] = Item::Value(value);
        assert_eq!(doc.to_string(), "[stats]\ninterval_ms = 500 # keep me\n");
    }
}
