//! Mount table change notifications: the kernel wakes a poll() on
//! /proc/self/mountinfo with POLLPRI whenever something is mounted, unmounted
//! or remounted, so the disk list only needs rescanning then.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

/// Starts a watcher thread. The flag is set after every mount table change;
/// whoever rescans clears it.
pub fn watch() -> Arc<AtomicBool> {
    let changed = Arc::new(AtomicBool::new(false));
    let flag = changed.clone();
    let spawned = thread::Builder::new()
        .name("mount-watch".into())
        .spawn(move || {
            if let Err(e) = watch_loop(&flag) {
                log::warn!("not watching mounts ({e}); drives mounted later appear after a restart");
            }
        });
    if let Err(e) = spawned {
        log::warn!("can't start the mount watcher: {e}");
    }
    changed
}

fn watch_loop(changed: &AtomicBool) -> io::Result<()> {
    let mountinfo = File::open("/proc/self/mountinfo")?;
    let mut pfd = libc::pollfd {
        fd: mountinfo.as_raw_fd(),
        events: libc::POLLPRI,
        revents: 0,
    };
    loop {
        // SAFETY: one valid pollfd, and `mountinfo` keeps the fd open.
        if unsafe { libc::poll(&mut pfd, 1, -1) } < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        // A change reports POLLPRI | POLLERR, then poll blocks until the next one.
        if pfd.revents & (libc::POLLPRI | libc::POLLERR) != 0 {
            log::debug!("mount table changed");
            changed.store(true, Ordering::Relaxed);
        }
    }
}
