use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Context, Result};

use crate::config::ButtonConfig;

/// Launch a button's command in the background. Returns once it has spawned;
/// a reaper thread waits on it and logs the exit status.
pub fn run(button: &ButtonConfig) -> Result<()> {
    log::info!("running {:?}: {}", button.id, button.command);
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&button.command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawning command for {:?}", button.id))?;

    let id = button.id.clone();
    thread::spawn(move || match child.wait() {
        Ok(status) if status.success() => log::debug!("{id:?} finished"),
        Ok(status) => log::warn!("{id:?} exited with {status}"),
        Err(e) => log::warn!("{id:?} wait failed: {e}"),
    });
    Ok(())
}
