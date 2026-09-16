//! The client that drives one agent process.
//!
//! The framing, protocol types, and state arrive with the agent layer (group
//! 7); the process handle is here now because the sandbox backends hand one
//! back.

use std::sync::Mutex;

use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::oneshot;

/// A launcher that has been started, and the pipes the protocol speaks over.
pub struct AgentProcess {
    /// Commands go out here.
    pub stdin: ChildStdin,
    /// Events come in here.
    pub stdout: ChildStdout,
    /// Whatever the launcher itself says about failing.
    pub stderr: ChildStderr,
    /// The launcher's process id, which is also its process group id.
    pub pid: i32,
    exited: Mutex<Option<oneshot::Receiver<Option<i32>>>>,
}

impl AgentProcess {
    /// The launcher's exit code, or none when a signal ended it.
    pub async fn exited(&self) -> Option<i32> {
        let receiver = self.exited.lock().expect("the exit channel").take()?;
        receiver.await.ok().flatten()
    }

    /// Ends the launcher itself, when its group is already gone.
    pub fn kill(&self) {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;

        // The group is what ends everything the agent started; the launcher
        // alone is the fallback when the group never formed.
        let _ = kill(Pid::from_raw(-self.pid), Signal::SIGKILL);
    }
}

/// Starts the launcher, wiring its pipes and waiting for its exit.
pub(crate) fn start_child(mut child: Child) -> AgentProcess {
    let (sender, receiver) = oneshot::channel();
    let pid = child.id().unwrap_or(0);
    let stdin = child.stdin.take().expect("stdin is piped");
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    tokio::spawn(async move {
        let status = child.wait().await.ok().and_then(|status| status.code());
        let _ = sender.send(status);
    });
    AgentProcess {
        stdin,
        stdout,
        stderr,
        #[allow(clippy::cast_possible_wrap)]
        pid: pid as i32,
        exited: Mutex::new(Some(receiver)),
    }
}
