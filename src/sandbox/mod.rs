//! The boundary: the policy a confined agent is given, the two backends that
//! enforce it, and the spawn that starts a session.

use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use crate::agent::client::AgentProcess;
use crate::log::Logger;
use crate::log::fields;
use crate::sandbox::backend::CapabilityReport;
use crate::sandbox::backend::SandboxLaunch;
use crate::sandbox::backend::SandboxLaunchError;
use crate::sandbox::backend::SandboxUnavailableError;
use crate::session::manager::SandboxPool;
use crate::session::session::RunningBox;

/// What a backend must offer, and what it reports it enforces.
pub mod backend;
/// The bailey backend: a confined process on this host.
pub mod bailey;
/// The egress broker every confined session dials out through.
pub mod broker;
/// Whether a path stays inside the directory a session is confined to.
pub mod paths;
/// The podman backend: a session in a container.
pub mod podman;
/// The policy document a bailey launch is given.
pub mod policy;
/// Chooses the backend the configuration asked for.
pub mod runtime;
/// Starts a confined launcher and wires up its pipes.
pub mod spawn;

/// What one run of a backend's tool answered with.
#[derive(Debug, Clone)]
pub struct RunResult {
    /// The exit code.
    pub code: i32,
    /// What the tool wrote to standard output.
    pub stdout: String,
    /// What the tool wrote to standard error.
    pub stderr: String,
}

/// The future a tool run answers with.
pub(crate) type RunFuture<T> = Pin<Box<dyn Future<Output = std::io::Result<T>> + Send>>;

/// Runs one backend tool. Injected, so tests answer from a script.
pub(crate) type Run =
    Arc<dyn Fn(Vec<String>, Option<String>) -> RunFuture<RunResult> + Send + Sync>;

/// Everything the bailey stop path needs.
pub struct BaileyStop {
    /// The session being stopped.
    pub session_id: String,
    /// The sandbox's name.
    pub name: String,
    /// The project the sandbox works in, for path translation.
    pub project_path: String,
    /// The launcher, which leads the process group.
    pub spawned: Arc<spawn::SpawnedAgent>,
    /// The policy that was trusted for it.
    pub policy: String,
    /// The tool runner, for the untrust that follows.
    pub run: Run,
    /// How long a stop waits before it kills.
    pub grace_ms: u64,
    /// The logger the warnings go to.
    pub log: Logger,
    /// Whether the stop already ran.
    pub stopped: std::sync::atomic::AtomicBool,
}

/// Everything the podman stop path needs.
pub struct PodmanStop {
    /// The session being stopped.
    pub session_id: String,
    /// The sandbox's name.
    pub name: String,
    /// The project the sandbox works in, for path translation.
    pub project_path: String,
    /// The launcher, whose group is ended after the container stops.
    pub spawned: Arc<spawn::SpawnedAgent>,
    /// The tool runner.
    pub run: Run,
    /// How long a stop waits before it removes the container.
    pub grace_ms: u64,
    /// The logger the warnings go to.
    pub log: Logger,
    /// Whether the stop already ran.
    pub stopped: std::sync::atomic::AtomicBool,
}

/// A running sandbox and the handle needed to end it.
pub enum SandboxHandle {
    /// A confined host process.
    Bailey(Box<BaileyStop>),
    /// A rootless container.
    Podman(Box<PodmanStop>),
}

impl SandboxHandle {
    /// The project path the sandbox was started on.
    pub fn project_path(&self) -> String {
        match self {
            SandboxHandle::Bailey(stop) => stop.project_path(),
            SandboxHandle::Podman(stop) => stop.project_path(),
        }
    }

    /// Translates a path as the agent sees it into a path on the host.
    ///
    /// Returns nothing for anything outside the project, which is what stops
    /// a crafted path from making the daemon read a file the sandbox itself
    /// could not.
    pub fn to_host_path(&self, agent_path: &str) -> Option<String> {
        let project = self.project_path();
        paths::host_path_under(backend::WORKSPACE_PATH, &project, agent_path)
    }

    /// The agent process inside, for the protocol client to drive.
    pub fn process(&self) -> Arc<dyn AgentProcess> {
        match self {
            SandboxHandle::Bailey(stop) => Arc::clone(&stop.spawned.process),
            SandboxHandle::Podman(stop) => Arc::clone(&stop.spawned.process),
        }
    }

    /// Stops the sandbox and everything in it, escalating to a kill after the
    /// grace period. Safe to call more than once.
    ///
    /// Returns whether a kill was needed.
    pub async fn stop(&self) -> bool {
        match self {
            SandboxHandle::Bailey(stop) => stop.stop().await,
            SandboxHandle::Podman(stop) => stop.stop().await,
        }
    }
}

impl BaileyStop {
    fn project_path(&self) -> String {
        self.project_path.clone()
    }

    async fn stop(&self) -> bool {
        if self
            .stopped
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return false;
        }

        self.spawned.kill(nix::sys::signal::Signal::SIGTERM);
        let exited = tokio::time::timeout(
            std::time::Duration::from_millis(self.grace_ms),
            self.spawned.process.exited(),
        )
        .await;

        let mut killed = false;
        match exited {
            Ok(_code) => {}
            Err(_elapsed) => {
                self.spawned.kill(nix::sys::signal::Signal::SIGKILL);
                killed = true;
                self.log.warn(
                    "confined process did not stop and was killed",
                    &fields([
                        ("session", self.session_id.as_str().into()),
                        ("name", self.name.as_str().into()),
                    ]),
                );
            }
        }

        let policy = self.policy.clone();
        let run = Arc::clone(&self.run);
        let _ = run(vec!["untrust".to_owned(), policy], None).await;
        killed
    }
}

impl PodmanStop {
    fn project_path(&self) -> String {
        self.project_path.clone()
    }

    async fn stop(&self) -> bool {
        if self
            .stopped
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return false;
        }

        let grace_seconds = (self.grace_ms / 1000).max(1);
        let name = self.name.clone();
        let result = (self.run)(
            vec![
                "stop".to_owned(),
                "--time".to_owned(),
                grace_seconds.to_string(),
                name.clone(),
            ],
            None,
        )
        .await
        .unwrap_or_else(|error| RunResult {
            code: -1,
            stdout: String::new(),
            stderr: error.to_string(),
        });
        if result.code == 0 {
            self.spawned.kill(nix::sys::signal::Signal::SIGKILL);
            return false;
        }

        let _ = (self.run)(
            vec!["rm".to_owned(), "--force".to_owned(), name.clone()],
            None,
        )
        .await;
        self.spawned.kill(nix::sys::signal::Signal::SIGKILL);
        self.log.warn(
            "container did not stop and was killed",
            &fields([
                ("session", self.session_id.as_str().into()),
                ("name", self.name.as_str().into()),
            ]),
        );
        true
    }
}

/// One sandbox backend, of exactly the two that exist.
pub enum Backend {
    /// Landlock and seccomp, with no container.
    Bailey(Arc<bailey::BaileySandbox>),
    /// Rootless podman containers.
    Podman(Arc<podman::PodmanSandbox>),
}

impl Backend {
    /// Checks that this backend can run here and reports what it can enforce.
    ///
    /// Returns [`backend::SandboxUnavailableError`] when it cannot run at all.
    /// It never falls back to another backend or to running unconfined.
    pub async fn probe(
        &self,
    ) -> Result<backend::CapabilityReport, backend::SandboxUnavailableError> {
        match self {
            Backend::Bailey(bailey) => bailey.probe().await,
            Backend::Podman(podman) => podman.probe().await,
        }
    }

    /// Starts one session's sandbox.
    pub async fn launch(
        &self,
        launch: &backend::SandboxLaunch,
    ) -> Result<SandboxHandle, backend::SandboxLaunchError> {
        match self {
            // The bailey handle holds itself behind an arc for its timers, so
            // the launch is reached through the same arc.
            Backend::Bailey(bailey) => bailey.launch(launch).await,
            Backend::Podman(podman) => podman.launch(launch).await,
        }
    }

    /// Names of sandboxes this system owns that no live session claims.
    pub async fn list_orphans(&self) -> Result<Vec<String>, backend::SandboxLaunchError> {
        match self {
            Backend::Bailey(bailey) => Ok(bailey.list_orphans()),
            Backend::Podman(podman) => podman.list_orphans().await,
        }
    }

    /// Removes the named sandboxes, returning how many were removed.
    pub async fn remove_orphans(&self, names: &[String]) -> usize {
        match self {
            Backend::Bailey(bailey) => bailey.remove_orphans(names),
            Backend::Podman(podman) => podman.remove_orphans(names).await,
        }
    }
}

/// Resolves a path against the process's working directory, lexically, which
/// is what a path named in configuration or a policy means.
pub(crate) fn resolve_root(path: &str) -> String {
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    let mut parts: Vec<Component> = Vec::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(Component::RootDir) | None => {}
                Some(Component::Normal(_)) => {
                    parts.pop();
                }
                Some(_) => parts.push(component),
            },
            other => parts.push(other),
        }
    }
    let mut resolved = PathBuf::new();
    for part in parts {
        resolved.push(part.as_os_str());
    }
    resolved.to_string_lossy().into_owned()
}

/// The real backend, offered to the session manager through its trait.
impl SandboxPool for Backend {
    fn probe(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<CapabilityReport, SandboxUnavailableError>> + Send + '_>>
    {
        Box::pin(self.probe())
    }

    fn launch(
        self: Arc<Self>,
        launch: SandboxLaunch,
    ) -> Pin<Box<dyn Future<Output = Result<RunningBox, SandboxLaunchError>> + Send>> {
        let launch = launch;
        Box::pin(async move {
            let handle = std::sync::Arc::new(Self::launch(&self, &launch).await?);
            Ok(RunningBox {
                process: handle.process(),
                to_host_path: Arc::new({
                    let handle = std::sync::Arc::clone(&handle);
                    move |path: &str| handle.to_host_path(path)
                }),
                stop: Arc::new({
                    let handle = std::sync::Arc::clone(&handle);
                    move || {
                        let handle = std::sync::Arc::clone(&handle);
                        Box::pin(async move { handle.stop().await })
                            as Pin<Box<dyn Future<Output = bool> + Send>>
                    }
                }),
            })
        })
    }

    fn list_orphans(&self) -> Pin<Box<dyn Future<Output = Vec<String>> + Send + '_>> {
        Box::pin(async move { Self::list_orphans(self).await.unwrap_or_default() })
    }

    fn remove_orphans<'a>(
        &'a self,
        names: &'a [String],
    ) -> Pin<Box<dyn Future<Output = usize> + Send + 'a>> {
        Box::pin(async move { Self::remove_orphans(self, names).await })
    }
}
