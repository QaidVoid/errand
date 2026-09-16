//! Starting a confined launcher and wiring up its pipes.
//!
//! The launcher leads its own process group, so ending a session ends
//! everything the agent started rather than only the launcher, leaving its
//! children behind holding the project open.

use std::collections::BTreeMap;
use std::process::Stdio;

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use crate::agent::client::{AgentProcess, start_child};

/// Spawns a launcher in its own process group and wires up its pipes.
///
/// The environment is cleared to exactly what is passed, so nothing the daemon
/// holds crosses unless it is named.
pub fn spawn_agent(
    command: &str,
    args: &[String],
    env: Option<&BTreeMap<String, String>>,
    cwd: Option<&str>,
) -> std::io::Result<SpawnedAgent> {
    let mut child = tokio::process::Command::new(command);
    child
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        // The launcher leads a group, so one signal reaches everything it
        // started.
        .process_group(0);
    if let Some(env) = env {
        child.envs(env);
    }
    if let Some(cwd) = cwd {
        child.current_dir(cwd);
    }
    let process = start_child(child.spawn()?);

    Ok(SpawnedAgent {
        pid: process.pid,
        process,
        killed: std::sync::atomic::AtomicBool::new(false),
    })
}

/// A launcher that has been started.
pub struct SpawnedAgent {
    /// The launcher's process id, which is also its process group id.
    pub pid: i32,
    /// The pipes the protocol speaks over.
    pub process: AgentProcess,
    killed: std::sync::atomic::AtomicBool,
}

impl SpawnedAgent {
    /// Terminates the whole process group. Safe to call more than once.
    ///
    /// The negative pid is the group, which is the point of leading one. When
    /// the group is already gone, or the process never made it far enough to
    /// lead one, the launcher itself is signalled instead.
    pub fn kill(&self, signal: Signal) {
        self.killed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let group = kill(Pid::from_raw(-self.pid), signal);
        if group.is_err() {
            let _ = kill(Pid::from_raw(self.pid), signal);
        }
        self.process.kill();
    }

    /// Whether the launcher has been asked to stop.
    pub fn is_killed(&self) -> bool {
        self.killed.load(std::sync::atomic::Ordering::Relaxed)
    }
}
