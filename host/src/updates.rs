//! Checks GitHub for a newer release, once at startup and then daily. It only
//! tells the device; installing stays with the package manager or install.sh.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// After a failed check (offline at boot, rate limited), try again sooner.
const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);
const TIMEOUT: Duration = Duration::from_secs(15);

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
    let repo = env!("CARGO_PKG_REPOSITORY").trim_start_matches("https://github.com/");
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    agent
        .get(&url)
        .header(
            "User-Agent",
            concat!("nerd-deck/", env!("CARGO_PKG_VERSION")),
        )
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("GET {url}"))?
        .body_mut()
        .read_json()
        .context("parsing the release")
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
}
