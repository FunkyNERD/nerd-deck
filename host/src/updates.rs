//! Checks GitHub for a newer release, once at startup and then daily. A new
//! host only gets a notice on the device; installing it stays with the package
//! manager or install.sh. Device firmware is a release asset of its own, which
//! the settings window, and the service when it keeps the deck up to date,
//! download into the cache (firmware.rs).

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    /// "sha256:<hex>"; GitHub fills it in for assets uploaded since mid 2025.
    #[serde(default)]
    digest: Option<String>,
}

/// Firmware in a GitHub release that this host can talk to.
#[derive(Debug, Clone)]
pub struct FirmwareRelease {
    pub version: String,
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
                        let version = release.tag_name.trim_start_matches('v').to_owned();
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

/// Looks through the recent releases for the newest firmware this host can
/// talk to. Prereleases are left out.
pub fn check_firmware() -> Result<FirmwareCheck> {
    let releases: Vec<Release> = api("releases?per_page=20").context("parsing the releases")?;
    Ok(newest_firmware(releases))
}

fn newest_firmware(releases: Vec<Release>) -> FirmwareCheck {
    let mut check = FirmwareCheck::default();
    let releases = releases.into_iter().filter(|r| !r.draft && !r.prerelease);
    for asset in releases.flat_map(|r| r.assets) {
        let Some((version, proto)) = firmware::parse_name(&asset.name) else {
            continue;
        };
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
/// must be for our board and the version it claims.
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
        let Some(want) = digest.strip_prefix("sha256:") else {
            bail!("unknown digest {digest}");
        };
        let got: String = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
        ensure!(got.eq_ignore_ascii_case(want), "the download doesn't match the release's checksum");
    }
    let image = Image::parse(data).context("checking the download")?;
    ensure!(
        image.version == release.version,
        "the download is firmware {}, not {}",
        image.version,
        release.version
    );
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let partial = path.with_extension("part");
    fs::write(&partial, image.data())
        .and_then(|()| fs::rename(&partial, &path))
        .with_context(|| format!("saving {}", path.display()))?;
    Ok(path)
}

/// For the service while it keeps the deck up to date: downloads new firmware
/// at startup and then daily, and leaves installing it to firmware::Flashing.
pub fn fetch_firmware() {
    let spawned = thread::Builder::new()
        .name("firmware-fetch".into())
        .spawn(|| loop {
            let fetched = check_firmware().and_then(|check| match check.firmware {
                Some(release) => download_firmware(&release).map(Some),
                None => Ok(None),
            });
            let wait = match fetched {
                Ok(_) => CHECK_EVERY,
                Err(e) => {
                    log::debug!("firmware check failed: {e:#}");
                    RETRY_AFTER
                }
            };
            thread::sleep(wait);
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
    fn picks_the_newest_compatible_firmware() {
        let fw = |v: &str, p: u32| firmware::file_name(v, p);
        let check = newest_firmware(vec![
            release("0.9.0", true, &[&fw("0.9.0", PROTO_VERSION)]),
            release("0.8.0", false, &[&fw("0.8.0", PROTO_VERSION + 1), "nerd-deck_0.8.0-1_amd64.deb"]),
            release("0.7.1", false, &[&fw("0.7.1", PROTO_VERSION)]),
            release("0.7.0", false, &[&fw("0.7.0", PROTO_VERSION)]),
        ]);
        assert_eq!(check.firmware.unwrap().version, "0.7.1");
        assert_eq!(check.needs_app.as_deref(), Some("0.8.0"));

        let check = newest_firmware(vec![release("0.7.0", false, &[&fw("0.7.0", PROTO_VERSION)])]);
        assert!(check.needs_app.is_none());
        assert!(newest_firmware(vec![]).firmware.is_none());
    }
}
