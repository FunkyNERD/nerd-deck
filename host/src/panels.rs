//! What custom tabs' panels show. Each panel is refreshed on its own
//! interval, only while the deck shows its tab, by a thread of its own: a slow
//! URL or command never holds up stats or button presses. The main loop gets
//! a panel's content only when it changes.

use std::collections::HashSet;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::{PanelConfig, PanelKind, TabConfig};
use crate::notifications::clean;
use crate::protocol::PanelValue;

/// The most of a panel's text the deck gets, and of its note.
const MAX_TEXT: usize = 63;
const MAX_SUB: usize = 31;
/// The most of a page or a command's output that's read.
const MAX_BODY: u64 = 1 << 20;
/// How often a running command is checked on.
const COMMAND_POLL: Duration = Duration::from_millis(50);
/// The longest the thread waits with nothing due, in case the clock jumps.
const IDLE: Duration = Duration::from_secs(60);

/// A panel's new content.
pub struct Update {
    pub tab: String,
    pub index: usize,
    pub value: PanelValue,
}

enum Event {
    Config(Vec<TabConfig>),
    /// The custom tabs the deck shows, by id; the rest aren't refreshed.
    Shown(HashSet<String>),
    Done { generation: u64, tab: usize, panel: usize, result: Result<PanelValue, String> },
}

pub struct Panels {
    events: Sender<Event>,
    updates: Receiver<Update>,
}

impl Panels {
    pub fn start(tabs: Vec<TabConfig>) -> Self {
        let (events, rx) = mpsc::channel();
        let (updates_tx, updates) = mpsc::channel();
        let done = events.clone();
        thread::Builder::new()
            .name("panels".into())
            .spawn(move || run(rx, done, updates_tx))
            .expect("starting the panels thread");
        let panels = Self { events, updates };
        panels.configure(tabs);
        panels
    }

    /// New tabs from a reloaded config. Their panels start over.
    pub fn configure(&self, tabs: Vec<TabConfig>) {
        let _ = self.events.send(Event::Config(tabs));
    }

    /// Refresh only these tabs' panels; a tab coming back is refreshed straight
    /// away if it's due.
    pub fn show(&self, tabs: HashSet<String>) {
        let _ = self.events.send(Event::Shown(tabs));
    }

    pub fn updates(&self) -> impl Iterator<Item = Update> + '_ {
        self.updates.try_iter()
    }
}

struct Slot {
    next: Instant,
    busy: bool,
    last: Option<PanelValue>,
}

fn run(events: Receiver<Event>, done: Sender<Event>, updates: Sender<Update>) {
    let mut tabs: Vec<TabConfig> = Vec::new();
    let mut slots: Vec<Vec<Slot>> = Vec::new();
    let mut shown = HashSet::new();
    // Results from before a reload are dropped.
    let mut generation = 0;
    loop {
        let now = Instant::now();
        let mut wake = now + IDLE;
        for (ti, tab) in tabs.iter().enumerate().filter(|(_, t)| shown.contains(&t.id)) {
            for (pi, panel) in tab.panels.iter().enumerate() {
                let slot = &mut slots[ti][pi];
                if slot.busy {
                    continue;
                }
                if slot.next > now {
                    wake = wake.min(slot.next);
                    continue;
                }
                slot.busy = true;
                slot.next = now + Duration::from_secs(panel.interval(tab));
                let (panel, done) = (panel.clone(), done.clone());
                let spawned = thread::Builder::new().name("panel".into()).spawn(move || {
                    let result = fetch(&panel);
                    let _ = done.send(Event::Done { generation, tab: ti, panel: pi, result });
                });
                if spawned.is_err() {
                    slot.busy = false;
                }
            }
        }

        match events.recv_timeout(wake.saturating_duration_since(Instant::now())) {
            Ok(Event::Config(t)) => {
                generation += 1;
                let now = Instant::now();
                slots = t
                    .iter()
                    .map(|tab| tab.panels.iter().map(|_| Slot { next: now, busy: false, last: None }).collect())
                    .collect();
                tabs = t;
            }
            Ok(Event::Shown(s)) => shown = s,
            Ok(Event::Done { generation: g, tab, panel, result }) if g == generation => {
                let slot = &mut slots[tab][panel];
                slot.busy = false;
                let config = &tabs[tab].panels[panel];
                let value = match result {
                    Ok(v) => v,
                    Err(e) => {
                        if slot.last.as_ref().is_none_or(|l| l.state != Some("stale") && l.state != Some("err")) {
                            log::warn!("tab {:?}: panel {:?}: {e}", tabs[tab].id, config.title);
                        }
                        failed(config, slot.last.as_ref(), &e)
                    }
                };
                if slot.last.as_ref() != Some(&value) {
                    slot.last = Some(value.clone());
                    let update = Update { tab: tabs[tab].id.clone(), index: panel, value };
                    if updates.send(update).is_err() {
                        return;
                    }
                }
            }
            Ok(Event::Done { .. }) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// A panel that couldn't be refreshed: a status panel is down; the others keep
/// what they showed, greyed, with the reason under it.
fn failed(p: &PanelConfig, last: Option<&PanelValue>, error: &str) -> PanelValue {
    let sub = clean(error, MAX_SUB);
    match p.kind {
        PanelKind::Status => PanelValue { text: "down".into(), sub, state: Some("err") },
        _ => PanelValue {
            text: last.map_or_else(|| "--".into(), |l| l.text.clone()),
            sub,
            state: Some("stale"),
        },
    }
}

/// What a panel shows now, or why it can't say. Blocks while it asks.
pub fn fetch(p: &PanelConfig) -> Result<PanelValue, String> {
    let timeout = Duration::from_secs(p.timeout);
    match p.kind {
        PanelKind::Text => Ok(PanelValue { text: clean(p.text.as_deref().unwrap_or(""), MAX_TEXT), ..Default::default() }),
        PanelKind::Command => {
            let out = run_command(p.command.as_deref().unwrap_or(""), timeout)?;
            Ok(value(p, first_line(&out), None))
        }
        PanelKind::Url => {
            let (status, body) = get(p, timeout)?;
            if !(200..300).contains(&status) {
                return Err(format!("HTTP {status}"));
            }
            let Some(pointer) = &p.json else {
                return Ok(value(p, first_line(&body), None));
            };
            let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("not JSON: {e}"))?;
            let text = pick(&json, pointer)?;
            let sub = p.sub_json.as_deref().map(|s| pick(&json, s)).transpose()?;
            Ok(value(p, text, sub))
        }
        PanelKind::Status => {
            let started = Instant::now();
            let (status, _) = get(p, timeout)?;
            let ms = started.elapsed().as_millis();
            let state = if status >= 400 {
                "err"
            } else if ms > u128::from(p.slow_ms) {
                "warn"
            } else {
                "ok"
            };
            Ok(PanelValue { text: status.to_string(), sub: format!("{ms} ms"), state: Some(state) })
        }
    }
}

/// The text to show for `raw`: a number with `decimals`, and `unit` after it,
/// coloured by `warn` and `crit`.
fn value(p: &PanelConfig, raw: String, sub: Option<String>) -> PanelValue {
    let number: Option<f64> = raw.trim().parse().ok().filter(|n: &f64| n.is_finite());
    let shown = match (number, p.decimals) {
        (Some(n), Some(d)) => format!("{n:.*}", usize::from(d)),
        _ => raw,
    };
    PanelValue {
        text: clean(&format!("{shown}{}", p.unit), MAX_TEXT),
        sub: sub.map(|s| clean(&s, MAX_SUB)).unwrap_or_default(),
        state: number.and_then(|n| level(p, n)),
    }
}

/// "err" from `crit`, "warn" from `warn`, else "ok"; None without either.
/// When `crit` is below `warn`, lower is worse.
fn level(p: &PanelConfig, n: f64) -> Option<&'static str> {
    if p.warn.is_none() && p.crit.is_none() {
        return None;
    }
    let low = matches!((p.warn, p.crit), (Some(w), Some(c)) if c < w);
    let past = |limit: f64| if low { n <= limit } else { n >= limit };
    Some(if p.crit.is_some_and(past) {
        "err"
    } else if p.warn.is_some_and(past) {
        "warn"
    } else {
        "ok"
    })
}

/// The value at `pointer`: a string as it is, anything else as JSON.
fn pick(json: &serde_json::Value, pointer: &str) -> Result<String, String> {
    match json.pointer(pointer) {
        Some(serde_json::Value::String(s)) => Ok(s.clone()),
        Some(v) => Ok(v.to_string()),
        None => Err(format!("nothing at {pointer}")),
    }
}

fn first_line(text: &str) -> String {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_owned()
}

/// GETs the panel's URL: its status and body. A 4xx or 5xx is an answer, not
/// an error.
fn get(p: &PanelConfig, timeout: Duration) -> Result<(u16, String), String> {
    let url = p.url.as_deref().unwrap_or("");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .user_agent(concat!("nerd-deck/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut request = agent.get(url);
    for (name, value) in &p.headers {
        request = request.header(name, value);
    }
    let mut response = request.call().map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let body = response.body_mut().with_config().limit(MAX_BODY).read_to_string().unwrap_or_default();
    Ok((status, body))
}

/// Runs `command` with `sh -c` and returns what it prints, or why it failed.
/// One that runs past `timeout` is killed, with anything it started.
fn run_command(command: &str, timeout: Duration) -> Result<String, String> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().expect("piped");
    let reader = thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.take(MAX_BODY).read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(COMMAND_POLL),
            Ok(None) => {
                // The whole group, so a pipeline's other commands go too and
                // the reader sees the end of their output.
                let pgid = child.id() as libc::pid_t;
                unsafe { libc::kill(-pgid, libc::SIGKILL) };
                let _ = child.wait();
                let _ = reader.join();
                return Err("timed out".into());
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(match status.code() {
            Some(code) => format!("exit {code}"),
            None => "killed".into(),
        });
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(toml: &str) -> PanelConfig {
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn formats_values() {
        let p = panel("kind='url'\nurl='x'\nunit='%'\ndecimals=1\nwarn=80.0\ncrit=95.0");
        let v = value(&p, "81.234".into(), Some("up".into()));
        assert_eq!((v.text.as_str(), v.sub.as_str(), v.state), ("81.2%", "up", Some("warn")));
        assert_eq!(value(&p, "99".into(), None).state, Some("err"));
        assert_eq!(value(&p, "hello".into(), None), PanelValue { text: "hello%".into(), ..Default::default() });

        // Lower is worse when crit is below warn, say for disk space left.
        let low = panel("kind='url'\nurl='x'\nwarn=20.0\ncrit=10.0");
        assert_eq!(value(&low, "15".into(), None).state, Some("warn"));
        assert_eq!(value(&low, "50".into(), None).state, Some("ok"));
    }

    #[test]
    fn picks_json() {
        let json = serde_json::json!({"chart": {"result": [{"price": 8412.3, "name": "ASX"}]}});
        assert_eq!(pick(&json, "/chart/result/0/price").unwrap(), "8412.3");
        assert_eq!(pick(&json, "/chart/result/0/name").unwrap(), "ASX");
        assert!(pick(&json, "/nope").is_err());
    }

    #[test]
    fn runs_commands() {
        let p = panel("kind='command'\ncommand='printf \"\\n  42 \\nmore\"'");
        assert_eq!(fetch(&p).unwrap().text, "42");
        assert_eq!(run_command("exit 3", Duration::from_secs(5)).unwrap_err(), "exit 3");
        let started = Instant::now();
        assert_eq!(run_command("sleep 5 | cat", Duration::from_millis(200)).unwrap_err(), "timed out");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn failures_keep_the_last_value() {
        let p = panel("kind='url'\nurl='x'");
        let last = PanelValue { text: "8412.3".into(), ..Default::default() };
        let v = failed(&p, Some(&last), "HTTP 503");
        assert_eq!((v.text.as_str(), v.sub.as_str(), v.state), ("8412.3", "HTTP 503", Some("stale")));
        let status = panel("kind='status'\nurl='x'");
        assert_eq!(failed(&status, None, "timed out").state, Some("err"));
    }
}
