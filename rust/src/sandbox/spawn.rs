//! Starting a confined launcher and wiring up its pipes.
//!
//! The launcher leads its own process group, so ending a session ends
//! everything the agent started rather than only the launcher, leaving its
//! children behind holding the project open.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex as AsyncMutex, mpsc, watch};

use crate::agent::client::AgentProcess;

/// The real launcher's pipes, as the client's trait sees them.
///
/// Writes go onto a channel that a forwarder task drains into the launcher's
/// stdin, so a write never blocks the caller and a failure surfaces as the
/// process ending.
pub struct TokioProcess {
    stdin: mpsc::UnboundedSender<Vec<u8>>,
    stdout: AsyncMutex<tokio::process::ChildStdout>,
    stderr: AsyncMutex<tokio::process::ChildStderr>,
    exited: watch::Receiver<Option<i32>>,
}

impl AgentProcess for TokioProcess {
    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.stdin
            .send(bytes.to_vec())
            .map_err(|_| std::io::Error::other("the sandbox launcher has been terminated"))
    }

    fn read_stdout<'a>(
        &'a self,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = std::io::Result<usize>> + Send + 'a>> {
        Box::pin(async move {
            let mut pipe = self.stdout.lock().await;
            pipe.read(buf).await
        })
    }

    fn read_stderr<'a>(
        &'a self,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = std::io::Result<usize>> + Send + 'a>> {
        Box::pin(async move {
            let mut pipe = self.stderr.lock().await;
            pipe.read(buf).await
        })
    }

    fn exited(&self) -> Pin<Box<dyn Future<Output = Option<i32>> + Send>> {
        let mut receiver = self.exited.clone();
        Box::pin(async move {
            let _ = receiver.changed().await;
            *receiver.borrow()
        })
    }
}

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
    let mut child = child.spawn()?;

    let (stdin_sender, mut stdin_receiver) = mpsc::unbounded_channel::<Vec<u8>>();
    let mut stdin = child.stdin.take().expect("stdin is piped");
    tokio::spawn(async move {
        while let Some(bytes) = stdin_receiver.recv().await {
            // A failed write ends the forwarder; the client sees the process
            // end through the exit watch.
            if stdin.write_all(&bytes).await.is_err() {
                break;
            }
        }
    });

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let pid = i32::try_from(child.id().unwrap_or(0)).unwrap_or(0);
    let (exit_sender, exit_receiver) = watch::channel(None);
    let mut waiting = child;
    tokio::spawn(async move {
        let status = waiting.wait().await.ok().and_then(|status| status.code());
        let _ = exit_sender.send(status);
    });
    let process = TokioProcess {
        stdin: stdin_sender,
        stdout: AsyncMutex::new(stdout),
        stderr: AsyncMutex::new(stderr),
        exited: exit_receiver,
    };
    Ok(SpawnedAgent {
        pid,
        process: Arc::new(process),
        killed: std::sync::atomic::AtomicBool::new(false),
    })
}

/// A launcher that has been started.
pub struct SpawnedAgent {
    /// The launcher's process id, which is also its process group id.
    pub pid: i32,
    /// The pipes the protocol speaks over.
    pub process: Arc<dyn AgentProcess>,
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
        if kill(Pid::from_raw(-self.pid), signal).is_err() {
            let _ = kill(Pid::from_raw(self.pid), signal);
        }
    }

    /// Whether the launcher has been asked to stop.
    pub fn is_killed(&self) -> bool {
        self.killed.load(std::sync::atomic::Ordering::Relaxed)
    }
}
