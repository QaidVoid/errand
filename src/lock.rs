//! Ensures one daemon per state directory.
//!
//! Two daemons on one bot token both receive every message and both act on
//! it: two sandboxes, two model bills, and a race to create the thread that
//! one of them loses with a confusing error in the channel. Nothing about the
//! chat connection prevents that, so it is prevented here.
//!
//! The lock holds a process id. One left by a process that no longer exists
//! is stale and is taken over, because a daemon that was killed must not stop
//! the next one from starting.

use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;

/// Filename of the lock inside the daemon state directory.
pub const LOCK_FILENAME: &str = "daemon.lock";

/// Raised when another daemon is already running against this state
/// directory.
#[derive(Debug, thiserror::Error)]
#[error(
    "another errand daemon is already running as process {pid}. Two daemons on one bot token \
     both act on every message. Stop that one first, or remove {path} if you are certain it is \
     gone."
)]
pub struct AlreadyRunningError {
    /// The daemon already holding the lock.
    pub pid: i32,
    /// Where the lock file sits, so an operator can clear a stale one.
    pub path: String,
}

/// Whether a process with this id exists.
///
/// Probed with SIGURG, whose default disposition is to be ignored, so the
/// target is not disturbed by being asked about. Being refused permission
/// counts as running: the process is there, it just belongs to somebody else,
/// and that is not evidence that the lock is stale.
pub fn is_running(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    match signal::kill(Pid::from_raw(pid), Signal::SIGURG) {
        // Delivered, or refused because it belongs to somebody else: either
        // way the process is there.
        Err(nix::errno::Errno::EPERM) | Ok(()) => true,
        Err(_) => false,
    }
}

/// A held lock, released on shutdown.
#[derive(Debug)]
pub struct DaemonLock {
    path: PathBuf,
    released: bool,
}

impl DaemonLock {
    /// Where the lock file sits.
    #[allow(
        dead_code,
        reason = "read by this module's tests, which assert on state the daemon never asks for"
    )]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Lets go of the lock. Twice is not an error.
    pub fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        // Already gone; the next daemon starts either way.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Takes the single-instance lock.
///
/// Fails with [`AlreadyRunningError`] when a live daemon already holds it.
pub fn acquire_lock(state_dir: &str, pid: i32) -> Result<DaemonLock, AlreadyRunningError> {
    let path = Path::new(state_dir).join(LOCK_FILENAME);

    for _ in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write;
                let _ = file.write_all(format!("{pid}\n").as_bytes());
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let held = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| text.trim().parse::<i32>().ok())
                    .unwrap_or(0);
                if is_running(held) {
                    return Err(AlreadyRunningError {
                        pid: held,
                        path: path.display().to_string(),
                    });
                }

                // The holder is gone, so the lock is stale. Clearing it and
                // retrying is what keeps a killed daemon from blocking every
                // restart after it.
                let _ = std::fs::remove_file(&path);
            }
            Err(error) => {
                return Err(AlreadyRunningError {
                    pid: 0,
                    path: format!("{}: {error}", path.display()),
                });
            }
        }
    }

    Ok(DaemonLock {
        path,
        released: false,
    })
}

#[cfg(test)]
#[path = "lock/tests.rs"]
mod tests;
