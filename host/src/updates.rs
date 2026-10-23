//! Checks GitHub for a newer release of the app, once at startup and then
//! daily, for a notice on the device. The settings window looks up what's new
//! in it and, for a packaged install, downloads the package for the system's
//! package manager to install. The device firmware has releases of its own
//! (firmware-v* tags), which the settings window, and the service when it keeps
//! the deck up to date, download into the cache (firmware.rs).

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::firmware::{self, Image};
use crate::protocol::PROTO_VERSION;

const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// After a failed check (offline at boot, rate limited), try again sooner.
const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);
const TIMEOUT: Duration = Duration::from_secs(15);
/// Downloads get longer; an image is under a megabyte.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_IMAGE: u64 = 16 * 1024 * 1024;
/// The app's packages are a few megabytes.
const MAX_PACKAGE: u64 = 64 * 1024 * 1024;
/// How often the firmware download looks for a newly seen board.
const BOARD_POLL: Duration = Duration::from_secs(30);

/// The newest release, when it's newer than this build. `seq` goes up with
/// every check that finds one, so the notice is shown again each day.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Available {
    pub version: String,
    pub seq: u64,
}

pub type Shared = Arc<Mutex<Option<Available>>>;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
    /// The release notes, in Markdown (.github/scripts/release-notes.sh).
    #[serde(default)]
    body: Option<String>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    /// "sha256:<hex>"; GitHub fills it in for assets uploaded since mid 2025.
    #[serde(default)]
    digest: Option<String>,
}

/// The newest app release, when it's newer than this build.
#[derive(Debug, Clone)]
pub struct AppUpdate {
    /// The features and fixes in every release since this build, newest first.
    pub changes: Vec<String>,
    /// The package for this system, when the app was installed from one.
    pub package: Option<Package>,
}

#[derive(Debug, Clone)]
pub struct Package {
    pub name: String,
    url: String,
    digest: Option<String>,
}

/// Firmware in a GitHub release that this host can talk to.
#[derive(Debug, Clone)]
pub struct FirmwareRelease {
    pub version: String,
    pub board: String,
    name: String,
    url: String,
    digest: Option<String>,
}

/// What the releases offer for the deck.
#[derive(Debug, Default)]
pub struct FirmwareCheck {
    /// The newest firmware that speaks this host's protocol.
    pub firmware: Option<FirmwareRelease>,
    /// A newer firmware that needs a newer host first, and that host's version.
    pub needs_app: Option<String>,
}

/// Starts the checker thread and returns what it has found so far.
pub fn watch() -> Shared {
    let found: Shared = Arc::default();
    let shared = found.clone();
    let spawned = thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            let mut seq = 0;
            loop {
                let wait = match latest() {
                    Ok(release) => {
                        let version = app_version(&release.tag_name).unwrap_or_default().to_owned();
                        if is_newer(&version, env!("CARGO_PKG_VERSION")) {
                            log::info!("nerd-deck {version} is available: {}", release.html_url);
                            seq += 1;
                            *shared.lock().unwrap() = Some(Available { version, seq });
                        } else {
                            log::debug!("up to date (latest release {version})");
                        }
                        CHECK_EVERY
                    }
                    Err(e) => {
                        log::debug!("update check failed: {e:#}");
                        RETRY_AFTER
                    }
                };
                thread::sleep(wait);
            }
        });
    if let Err(e) = spawned {
        log::warn!("can't start the update checker: {e}");
    }
    found
}

/// The app's version in a release tag: host-v1.2.3, or v1.2.3 from before the
/// app and the firmware were released apart. Firmware releases (firmware-v*)
/// never take "Latest", but they aren't the app's either way.
fn app_version(tag: &str) -> Option<&str> {
    tag.strip_prefix("host-v").or_else(|| tag.strip_prefix('v'))
}

/// The release GitHub marks "Latest", which is always the app's.
fn latest() -> Result<Release> {
    api("releases/latest").context("parsing the release")
}

/// GET a path under this repository in the GitHub API, as JSON.
fn api<T: serde::de::DeserializeOwned>(path: &str) -> Result<T> {
    let repo = env!("CARGO_PKG_REPOSITORY").trim_start_matches("https://github.com/");
    let url = format!("https://api.github.com/repos/{repo}/{path}");
    agent(TIMEOUT)
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("GET {url}"))?
        .body_mut()
        .read_json()
        .map_err(Into::into)
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(concat!("nerd-deck/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Looks through the recent releases for app releases newer than this build.
/// Prereleases are left out.
pub fn check_app() -> Result<Option<AppUpdate>> {
    let releases: Vec<Release> = api("releases?per_page=30").context("parsing the releases")?;
    Ok(newer_app(releases, env!("CARGO_PKG_VERSION"), package_kind()))
}

fn newer_app(releases: Vec<Release>, current: &str, kind: Option<&str>) -> Option<AppUpdate> {
    let mut newer: Vec<(String, Release)> = releases
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| Some((app_version(&r.tag_name)?.to_owned(), r)))
        .filter(|(v, _)| is_newer(v, current))
        .collect();
    newer.sort_by(|a, b| if is_newer(&a.0, &b.0) { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater });
    let changes = newer.iter().flat_map(|(_, r)| release_changes(r.body.as_deref().unwrap_or(""))).collect();
    let (_, latest) = newer.into_iter().next()?;
    let package = kind.and_then(|ext| {
        let asset = latest.assets.into_iter().find(|a| a.name.ends_with(ext))?;
        Some(Package { name: asset.name, url: asset.browser_download_url, digest: asset.digest })
    });
    Some(AppUpdate { changes, package })
}

/// The Features and Fixes bullets of release notes, as plain text: no commit
/// hash, scope, breaking mark or wrapped details.
fn release_changes(notes: &str) -> Vec<String> {
    let mut wanted = false;
    let mut changes = Vec::new();
    for line in notes.lines() {
        if let Some(heading) = line.strip_prefix("### ") {
            wanted = matches!(heading.trim(), "Features" | "Fixes" | "Performance");
        } else if let Some(item) = line.strip_prefix("- ").filter(|_| wanted) {
            let item = match item.rsplit_once(" (") {
                Some((text, hash)) if hash.trim_end_matches(')').chars().all(|c| c.is_ascii_hexdigit()) => text,
                _ => item,
            };
            // "**Breaking:** **scope:** text": just the text.
            let mut text = item;
            while let Some(rest) = text.strip_prefix("**").and_then(|t| Some(t.split_once(":** ")?.1)) {
                text = rest;
            }
            let mut chars = text.chars();
            if let Some(first) = chars.next() {
                changes.push(first.to_uppercase().chain(chars).collect());
            }
        }
    }
    changes
}

/// The package type to offer: only when this app came from a package (it
/// runs from /usr), so it's never installed over a source install, and only
/// for a package manager the system has.
fn package_kind() -> Option<&'static str> {
    if !packaged() {
        return None;
    }
    if Path::new("/usr/bin/dpkg").exists() {
        Some(".deb")
    } else if Path::new("/usr/bin/rpm").exists() {
        Some(".rpm")
    } else {
        None
    }
}

/// This app was installed from a package, not built by install.sh.
pub fn packaged() -> bool {
    std::env::current_exe().is_ok_and(|exe| exe.starts_with("/usr"))
}

/// Downloads the package into the Downloads folder, checked against the
/// release's digest, and returns its path.
pub fn download_package(package: &Package) -> Result<PathBuf> {
    let dir = dirs::download_dir().or_else(dirs::home_dir).context("no Downloads folder")?;
    let path = dir.join(&package.name);
    log::info!("downloading {}", package.name);
    let mut data = Vec::new();
    agent(DOWNLOAD_TIMEOUT)
        .get(&package.url)
        .call()
        .with_context(|| format!("GET {}", package.url))?
        .into_body()
        .into_reader()
        .take(MAX_PACKAGE)
        .read_to_end(&mut data)
        .context("downloading the package")?;
    if let Some(digest) = &package.digest {
        check_digest(&data, digest)?;
    }
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    fs::write(&path, data).with_context(|| format!("saving {}", path.display()))?;
    Ok(path)
}

fn check_digest(data: &[u8], digest: &str) -> Result<()> {
    let Some(want) = digest.strip_prefix("sha256:") else {
        bail!("unknown digest {digest}");
    };
    let got: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
    ensure!(got.eq_ignore_ascii_case(want), "the download doesn't match the release's checksum");
    Ok(())
}

/// Looks through the recent releases for the newest firmware for a board that
/// this host can talk to. Prereleases are left out.
pub fn check_firmware(board: &str) -> Result<FirmwareCheck> {
    let releases: Vec<Release> = api("releases?per_page=20").context("parsing the releases")?;
    Ok(newest_firmware(releases, board))
}

fn newest_firmware(releases: Vec<Release>, board: &str) -> FirmwareCheck {
    let mut check = FirmwareCheck::default();
    let releases = releases.into_iter().filter(|r| !r.draft && !r.prerelease);
    for asset in releases.flat_map(|r| r.assets) {
        let Some(firmware::Name { version, board: b, proto }) = firmware::parse_name(&asset.name) else {
            continue;
        };
        if b != board {
            continue;
        }
        if proto != PROTO_VERSION {
            let newer = |v: &Option<String>| v.as_ref().is_none_or(|n| is_newer(&version, n));
            if proto > PROTO_VERSION && newer(&check.needs_app) {
                check.needs_app = Some(version);
            }
            continue;
        }
        let newer = check.firmware.as_ref().is_none_or(|f| is_newer(&version, &f.version));
        if newer {
            check.firmware = Some(FirmwareRelease {
                version,
                board: board.to_owned(),
                name: asset.name,
                url: asset.browser_download_url,
                digest: asset.digest,
            });
        }
    }
    // Only worth a mention when it's newer than what this host can install.
    if let (Some(needs), Some(fw)) = (&check.needs_app, &check.firmware) {
        if !is_newer(needs, &fw.version) {
            check.needs_app = None;
        }
    }
    check
}

/// Downloads the firmware into the cache, unless it's there already, and
/// returns its path. The image is checked against the release's digest and
/// must be for the board and the version it claims.
pub fn download_firmware(release: &FirmwareRelease) -> Result<PathBuf> {
    let dir = firmware::cache_dir().context("no cache directory")?;
    let path = dir.join(&release.name);
    if Image::load(&path).is_ok_and(|image| image.version == release.version) {
        return Ok(path);
    }
    log::info!("downloading firmware {}", release.version);
    let mut data = Vec::new();
    agent(DOWNLOAD_TIMEOUT)
        .get(&release.url)
        .call()
        .with_context(|| format!("GET {}", release.url))?
        .into_body()
        .into_reader()
        .take(MAX_IMAGE)
        .read_to_end(&mut data)
        .context("downloading the firmware")?;
    if let Some(digest) = &release.digest {
        check_digest(&data, digest)?;
    }
    let image = Image::parse(data).context("checking the download")?;
    ensure!(
        image.version == release.version && image.board == release.board,
        "the download is firmware {} for {}, not {} for {}",
        image.version,
        image.board,
        release.version,
        release.board
    );
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let partial = path.with_extension("part");
    fs::write(&partial, image.data())
        .and_then(|()| fs::rename(&partial, &path))
        .with_context(|| format!("saving {}", path.display()))?;
    Ok(path)
}

/// The boards to keep firmware downloaded for: those of the decks seen so far.
pub type Boards = Arc<Mutex<BTreeSet<String>>>;

/// For the service while it keeps the deck up to date: downloads new firmware
/// for each board in `boards` at startup and then daily, and straight away
/// for a board that's added, and leaves installing it to firmware::Flashing.
pub fn fetch_firmware(boards: Boards) {
    let spawned = thread::Builder::new()
        .name("firmware-fetch".into())
        .spawn(move || {
            let mut fetched = BTreeSet::new();
            let mut next = Instant::now();
            loop {
                let wanted = boards.lock().unwrap().clone();
                if Instant::now() >= next || wanted != fetched {
                    let mut ok = true;
                    for board in &wanted {
                        let result = check_firmware(board).and_then(|check| match check.firmware {
                            Some(release) => download_firmware(&release).map(Some),
                            None => Ok(None),
                        });
                        if let Err(e) = result {
                            log::debug!("firmware check for {board} failed: {e:#}");
                            ok = false;
                        }
                    }
                    fetched = wanted;
                    next = Instant::now() + if ok { CHECK_EVERY } else { RETRY_AFTER };
                }
                thread::sleep(BOARD_POLL);
            }
        });
    if let Err(e) = spawned {
        log::warn!("can't start the firmware download: {e}");
    }
}

/// Compares major.minor.patch; anything after a `-` is ignored.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<(u64, u64, u64)> {
        let mut parts = v.split('-').next()?.split('.').map(|p| p.parse().ok());
        Some((parts.next()??, parts.next()??, parts.next()??))
    }
    matches!((parse(candidate), parse(current)), (Some(a), Some(b)) if a > b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_app_tags() {
        assert_eq!(app_version("host-v0.7.0"), Some("0.7.0"));
        assert_eq!(app_version("v0.6.0"), Some("0.6.0"));
        assert_eq!(app_version("firmware-v0.7.0"), None);
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.3.0", "0.2.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("garbage", "0.2.0"));
    }

    fn release(version: &str, prerelease: bool, assets: &[&str]) -> Release {
        Release {
            tag_name: format!("v{version}"),
            html_url: String::new(),
            draft: false,
            prerelease,
            body: None,
            assets: assets
                .iter()
                .map(|name| Asset {
                    name: name.to_string(),
                    browser_download_url: format!("https://example.invalid/{name}"),
                    digest: None,
                })
                .collect(),
        }
    }

    #[test]
    fn reads_whats_new_from_release_notes() {
        let notes = "## What's changed\n\nEverything since host-v0.8.1.\n\n### Features\n\n\
            - show the power profile under the CPU gauge (a631b0d)\n  The app reads the active profile.\n\n\
            ### Fixes\n\n- **gpu:** let a hybrid laptop's GPU suspend while it's watched (f9d68d9)\n\n\
            ### Chores\n\n- **release:** app 0.8.2 (cc6f745)\n";
        assert_eq!(
            release_changes(notes),
            ["Show the power profile under the CPU gauge", "Let a hybrid laptop's GPU suspend while it's watched"]
        );
    }

    #[test]
    fn finds_newer_app_releases() {
        let mut r = |tag: &str, body: &str, assets: &[&str]| {
            let mut r = release(tag.trim_start_matches("host-v").trim_start_matches('v'), false, assets);
            r.tag_name = tag.into();
            r.body = Some(body.into());
            r
        };
        let releases = vec![
            r("host-v0.8.3", "### Fixes\n- three (aaaaaaa)\n", &["nerd-deck_0.8.3-1_amd64.deb", "nerd-deck-0.8.3-1.x86_64.rpm"]),
            r("firmware-v0.9.0", "### Features\n- firmware (bbbbbbb)\n", &[]),
            r("host-v0.8.2", "### Features\n- two (ccccccc)\n", &[]),
            r("v0.6.0", "### Features\n- old (ddddddd)\n", &[]),
        ];
        let update = newer_app(releases, "0.8.1", Some(".deb")).unwrap();
        assert_eq!(update.changes, ["Three", "Two"]);
        assert_eq!(update.package.unwrap().name, "nerd-deck_0.8.3-1_amd64.deb");
        assert!(newer_app(vec![r("host-v0.8.3", "", &[])], "0.8.3", None).is_none());
    }

    #[test]
    fn picks_the_newest_compatible_firmware() {
        let board = firmware::LEGACY_BOARD;
        let fw = |v: &str, p: u32| firmware::file_name(v, board, p);
        let check = newest_firmware(vec![
            release("0.9.0", true, &[&fw("0.9.0", PROTO_VERSION)]),
            release("0.8.0", false, &[&fw("0.8.0", PROTO_VERSION + 1), "nerd-deck_0.8.0-1_amd64.deb"]),
            release("0.7.1", false, &[&fw("0.7.1", PROTO_VERSION)]),
            release("0.7.0", false, &[&fw("0.7.0", PROTO_VERSION)]),
            release("0.7.2", false, &[&firmware::file_name("0.7.2", "waveshare-c5-touch-35", PROTO_VERSION)]),
        ], board);
        assert_eq!(check.firmware.unwrap().version, "0.7.1");
        assert_eq!(check.needs_app.as_deref(), Some("0.8.0"));

        let check = newest_firmware(vec![release("0.7.0", false, &[&fw("0.7.0", PROTO_VERSION)])], board);
        assert!(check.needs_app.is_none());
        assert!(newest_firmware(vec![], board).firmware.is_none());
    }
}
