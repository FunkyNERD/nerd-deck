//! What custom tabs' panels show. Each panel is refreshed on its own
//! interval, only while the deck shows its tab, by a thread of its own: a slow
//! URL or command never holds up stats or button presses. The main loop gets
//! a panel's content only when it changes.
//!
//! Panels that ask the same URL, with the same headers, share one request:
//! those due together wait on one answer, and one due while the last answer
//! is younger than its interval takes that, so a page several panels read
//! from is fetched once per refresh.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::{PanelConfig, PanelKind, TabConfig};
use crate::notifications::clean;
use crate::protocol::PanelValue;

/// The most of a panel's text the deck gets (a quote wraps), and of its note.
/// Well inside a protocol line.
const MAX_TEXT: usize = 255;
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
    /// A web request the panels waiting on it share.
    Answered { generation: u64, request: Request, answer: Result<Answer, String> },
}

/// What a web panel asks: panels asking the same share one request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Request {
    url: String,
    headers: Vec<(String, String)>,
}

impl Request {
    /// None for panels that don't ask a web server.
    fn of(p: &PanelConfig) -> Option<Self> {
        matches!(p.kind, PanelKind::Url | PanelKind::Status).then(|| Self {
            url: p.url.clone().unwrap_or_default(),
            headers: p.headers.iter().filter(|(k, _)| !k.trim().is_empty()).cloned().collect(),
        })
    }
}

/// A web server's answer: its status, its body, and how long it took.
#[derive(Debug, Clone)]
struct Answer {
    status: u16,
    body: String,
    ms: u128,
}

/// A request's last answer and when it came, and the panels waiting on the
/// one in flight.
#[derive(Default)]
struct Shared {
    answer: Option<(Instant, Result<Answer, String>)>,
    waiting: Vec<(usize, usize)>,
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
    let mut requests: HashMap<Request, Shared> = HashMap::new();
    // Results from before a reload are dropped.
    let mut generation = 0;
    loop {
        let now = Instant::now();
        let mut wake = now + IDLE;
        let mut due = Vec::new();
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
                slot.next = now + Duration::from_secs(panel.interval(tab));
                wake = wake.min(slot.next);
                due.push((ti, pi));
            }
        }

        for (ti, pi) in due {
            let panel = &tabs[ti].panels[pi];
            let Some(request) = Request::of(panel) else {
                // Its own thread: a command, or text.
                slots[ti][pi].busy = true;
                let (panel, done) = (panel.clone(), done.clone());
                let spawned = thread::Builder::new().name("panel".into()).spawn(move || {
                    let result = fetch(&panel);
                    let _ = done.send(Event::Done { generation, tab: ti, panel: pi, result });
                });
                if spawned.is_err() {
                    slots[ti][pi].busy = false;
                }
                continue;
            };
            let interval = Duration::from_secs(panel.interval(&tabs[ti]));
            let shared = requests.entry(request.clone()).or_default();
            // Answered recently enough for this panel: read that.
            if let Some((at, answer)) = &shared.answer {
                if at.elapsed() < interval && shared.waiting.is_empty() {
                    let result = read(panel, answer);
                    if !settle(&tabs, &mut slots, &updates, ti, pi, result) {
                        return;
                    }
                    continue;
                }
            }
            slots[ti][pi].busy = true;
            shared.waiting.push((ti, pi));
            if shared.waiting.len() > 1 {
                continue; // asked already: it waits for that answer
            }
            let timeout = Duration::from_secs(panel.timeout);
            let done = done.clone();
            let spawned = thread::Builder::new().name("panel".into()).spawn(move || {
                let answer = ask(&request, timeout);
                let _ = done.send(Event::Answered { generation, request, answer });
            });
            if spawned.is_err() {
                for (t, p) in shared.waiting.drain(..) {
                    slots[t][p].busy = false;
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
                requests.clear();
            }
            Ok(Event::Shown(s)) => shown = s,
            Ok(Event::Done { generation: g, tab, panel, result }) if g == generation => {
                slots[tab][panel].busy = false;
                if !settle(&tabs, &mut slots, &updates, tab, panel, result) {
                    return;
                }
            }
            Ok(Event::Answered { generation: g, request, answer }) if g == generation => {
                let shared = requests.entry(request).or_default();
                let waiting = std::mem::take(&mut shared.waiting);
                for &(tab, panel) in &waiting {
                    slots[tab][panel].busy = false;
                    let result = read(&tabs[tab].panels[panel], &answer);
                    if !settle(&tabs, &mut slots, &updates, tab, panel, result) {
                        return;
                    }
                }
                shared.answer = Some((Instant::now(), answer));
            }
            Ok(Event::Done { .. } | Event::Answered { .. }) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Puts a panel's new result in its slot, and sends it to the main loop if
/// what it shows changed. False once the main loop has gone.
fn settle(
    tabs: &[TabConfig],
    slots: &mut [Vec<Slot>],
    updates: &Sender<Update>,
    tab: usize,
    panel: usize,
    result: Result<PanelValue, String>,
) -> bool {
    let slot = &mut slots[tab][panel];
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
    if slot.last.as_ref() == Some(&value) {
        return true;
    }
    slot.last = Some(value.clone());
    updates.send(Update { tab: tabs[tab].id.clone(), index: panel, value }).is_ok()
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
        PanelKind::Url | PanelKind::Status => {
            let request = Request::of(p).expect("a web panel");
            read(p, &ask(&request, timeout))
        }
    }
}

/// What a web panel shows for `answer`.
fn read(p: &PanelConfig, answer: &Result<Answer, String>) -> Result<PanelValue, String> {
    let answer = answer.as_ref().map_err(Clone::clone)?;
    if p.kind == PanelKind::Status {
        let state = if answer.status >= 400 {
            "err"
        } else if answer.ms > u128::from(p.slow_ms) {
            "warn"
        } else {
            "ok"
        };
        let text = if p.status_text { status_reason(answer.status) } else { answer.status.to_string() };
        return Ok(PanelValue { text, sub: format!("{} ms", answer.ms), state: Some(state) });
    }
    if !(200..300).contains(&answer.status) {
        return Err(format!("HTTP {}", answer.status));
    }
    let Some(pointer) = &p.json else {
        return Ok(value(p, first_line(&answer.body), None));
    };
    let json: serde_json::Value = serde_json::from_str(&answer.body).map_err(|e| format!("not JSON: {e}"))?;
    let text = pick(&json, pointer)?;
    let sub = p.sub_json.as_deref().map(|s| pick(&json, s)).transpose()?;
    Ok(value(p, text, sub))
}

/// The text to show for `raw`: a number with `decimals`, `prefix` before it
/// and `unit` after it, coloured by `warn` and `crit`.
fn value(p: &PanelConfig, raw: String, sub: Option<String>) -> PanelValue {
    let number: Option<f64> = raw.trim().parse().ok().filter(|n: &f64| n.is_finite());
    let shown = match (number, p.decimals) {
        (Some(n), Some(d)) => format!("{n:.*}", usize::from(d)),
        _ => raw,
    };
    PanelValue {
        text: clean(&format!("{}{shown}{}", p.prefix, p.unit), MAX_TEXT),
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

/// A status code's reason, like "Not Found"; the code when it has none.
fn status_reason(code: u16) -> String {
    ureq::http::StatusCode::from_u16(code)
        .ok()
        .and_then(|s| s.canonical_reason())
        .map_or_else(|| code.to_string(), str::to_owned)
}

fn first_line(text: &str) -> String {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_owned()
}

/// GETs a URL: its status and body, and how long it took. A 4xx or 5xx is an
/// answer, not an error.
fn ask(request: &Request, timeout: Duration) -> Result<Answer, String> {
    log::debug!("asking {}", request.url);
    let started = Instant::now();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .user_agent(concat!("nerd-deck/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut get = agent.get(&request.url);
    for (name, value) in &request.headers {
        get = get.header(name.trim(), value);
    }
    let mut response = get.call().map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let body = response.body_mut().with_config().limit(MAX_BODY).read_to_string().unwrap_or_default();
    Ok(Answer { status, body, ms: started.elapsed().as_millis() })
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
        let money = panel("kind='url'\nurl='x'\nprefix='$'\ndecimals=2");
        assert_eq!(value(&money, "1.4271".into(), None).text, "$1.43");
        let both = panel("kind='url'\nurl='x'\nprefix='$'\nunit=' AUD'");
        assert_eq!(value(&both, "1.43".into(), None).text, "$1.43 AUD");

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
    fn panels_share_a_request() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        // A server that counts what it's asked.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                counted.fetch_add(1, Ordering::SeqCst);
                let _ = stream.read(&mut [0u8; 4096]);
                let body = r#"{"date":"2026-11-04","rates":{"AUD":1.4271,"EUR":0.92}}"#;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });

        // Three panels on one URL, one on another.
        let url = format!("http://127.0.0.1:{port}/latest");
        let tab: TabConfig = toml::from_str(&format!(
            "id='money'\nname='Money'\ngrid=[2,2]\ninterval=3600\n\
             [[panel]]\nkind='url'\nurl='{url}'\njson='/rates/AUD'\nprefix='$'\n\
             [[panel]]\nkind='url'\nurl='{url}'\njson='/rates/EUR'\n\
             [[panel]]\nkind='status'\nurl='{url}'\n\
             [[panel]]\nkind='url'\nurl='http://127.0.0.1:{port}/other'\njson='/date'"
        ))
        .unwrap();
        let panels = Panels::start(vec![tab]);
        panels.show(HashSet::from(["money".to_owned()]));
        let mut got = HashMap::new();
        let until = Instant::now() + Duration::from_secs(5);
        while got.len() < 4 && Instant::now() < until {
            got.extend(panels.updates().map(|u| (u.index, u.value.text)));
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(got[&0], "$1.4271");
        assert_eq!(got[&1], "0.92");
        assert_eq!(got[&2], "200");
        assert_eq!(got[&3], "2026-11-04");
        assert_eq!(hits.load(Ordering::SeqCst), 2, "one request per URL");
    }

    #[test]
    fn sends_headers() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        // A server that answers with the key it was given.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let key = request.lines().find_map(|l| l.strip_prefix("x-api-key: ")).unwrap_or("none").to_owned();
                seen.lock().unwrap().push(key.clone());
                let body = format!(r#"{{"key":"{key}"}}"#);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        let url = format!("http://127.0.0.1:{port}/v1/x");
        let with_key = |key: &str| -> PanelConfig {
            toml::from_str(&format!("kind='url'\nurl='{url}'\njson='/key'\nheaders={{ X-Api-Key = '{key}' }}")).unwrap()
        };
        assert_eq!(fetch(&with_key("abc123")).unwrap().text, "abc123");
        // Different keys are different requests.
        assert_ne!(Request::of(&with_key("a")), Request::of(&with_key("b")));
        assert_eq!(asked.lock().unwrap().as_slice(), ["abc123"]);
    }

    #[test]
    fn status_shows_its_code_or_reason() {
        let answer = |status| Ok(Answer { status, body: String::new(), ms: 12 });
        let code = panel("kind='status'\nurl='x'");
        assert_eq!(read(&code, &answer(404)).unwrap().text, "404");
        let reason = panel("kind='status'\nurl='x'\nstatus_text=true");
        assert_eq!(read(&reason, &answer(200)).unwrap().text, "OK");
        let v = read(&reason, &answer(503)).unwrap();
        assert_eq!((v.text.as_str(), v.sub.as_str(), v.state), ("Service Unavailable", "12 ms", Some("err")));
        assert_eq!(read(&reason, &answer(599)).unwrap().text, "599");
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
