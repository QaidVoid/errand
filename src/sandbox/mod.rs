//! The boundary: the policy a confined agent is given, the backends that
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

/// What a sandboxed agent is told about providers and plugins.
pub mod agent_config;
/// What a backend must offer, and what it reports it enforces.
pub mod backend;
/// The bailey backend: a confined process on this host.
pub mod bailey;
/// The egress broker every confined session dials out through.
pub mod broker;
/// What a brokered session is handed to reach its broker.
pub mod egress;
/// The incus backend: a session in an unprivileged system container.
pub mod incus;
/// The provider ids the installed kage knows.
pub mod kage_providers;
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
///
/// The arguments are the command line, the working directory, and what to
/// write to the tool's standard input. The last is `None` for tools that read
/// nothing, which is most of them.
pub(crate) type Run =
    Arc<dyn Fn(Vec<String>, Option<String>, Option<String>) -> RunFuture<RunResult> + Send + Sync>;

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
    /// The session's own egress broker, closed when the session is stopped.
    pub broker: std::sync::Mutex<Option<broker::Broker>>,
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
    /// The session's own egress broker, closed when the session is stopped.
    pub broker: std::sync::Mutex<Option<broker::Broker>>,
    /// Whether the stop already ran.
    pub stopped: std::sync::atomic::AtomicBool,
}

/// Everything the incus stop path needs.
pub struct IncusStop {
    /// The session being stopped.
    pub session_id: String,
    /// The sandbox's name.
    pub name: String,
    /// The Incus project the sandbox lives in.
    pub project: String,
    /// The project the sandbox works in, for path translation.
    pub project_path: String,
    /// The directory holding the broker socket, when one was started.
    pub broker_dir: Option<String>,
    /// The launcher, whose group is ended after the instance stops.
    pub spawned: Arc<spawn::SpawnedAgent>,
    /// The tool runner.
    pub run: Run,
    /// How long the instance may take to stop before it is deleted by force.
    pub grace_ms: u64,
    /// The logger the warnings go to.
    pub log: Logger,
    /// The session's own egress broker, closed when the session is stopped.
    pub broker: std::sync::Mutex<Option<broker::Broker>>,
    /// Whether the stop already ran.
    pub stopped: std::sync::atomic::AtomicBool,
}

/// A running sandbox and the handle needed to end it.
pub enum SandboxHandle {
    /// A confined host process.
    Bailey(Box<BaileyStop>),
    /// A rootless container.
    Podman(Box<PodmanStop>),
    /// An unprivileged system container.
    Incus(Box<IncusStop>),
}

impl SandboxHandle {
    /// The project path the sandbox was started on.
    pub fn project_path(&self) -> String {
        match self {
            SandboxHandle::Bailey(stop) => stop.project_path(),
            SandboxHandle::Podman(stop) => stop.project_path(),
            SandboxHandle::Incus(stop) => stop.project_path(),
        }
    }

    /// Translates a path as the agent sees it into a path on the host.
    ///
    /// A spelling check only: a leading separator is read as the workspace,
    /// and any other absolute path as project-relative, so `/etc/passwd`
    /// names a file inside the project rather than the host's. It answers
    /// where a project-relative path would land, and says nothing about what
    /// sits there now; a read or a write the daemon performs on the answer
    /// goes through `paths::open_beneath`, which is what refuses a link
    /// planted between the two.
    pub fn to_host_path(&self, agent_path: &str) -> Option<String> {
        let project = self.project_path();
        paths::host_path_under(backend::WORKSPACE_PATH, &project, agent_path)
    }

    /// The agent process inside, for the protocol client to drive.
    pub fn process(&self) -> Arc<dyn AgentProcess> {
        match self {
            SandboxHandle::Bailey(stop) => Arc::clone(&stop.spawned.process),
            SandboxHandle::Podman(stop) => Arc::clone(&stop.spawned.process),
            SandboxHandle::Incus(stop) => Arc::clone(&stop.spawned.process),
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
            SandboxHandle::Incus(stop) => stop.stop().await,
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

        close_broker(&self.broker);

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
        let _ = run(vec!["untrust".to_owned(), policy], None, None).await;
        killed
    }
}

/// Closes a session's broker, if it has one. Done before the session's
/// processes are signalled, so no new connection is admitted, and no nonce
/// honoured on one, once it is asked to stop.
fn close_broker(slot: &std::sync::Mutex<Option<broker::Broker>>) {
    let broker = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(mut broker) = broker {
        broker.close();
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

        close_broker(&self.broker);

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

impl IncusStop {
    fn project_path(&self) -> String {
        self.project_path.clone()
    }

    fn command(&self, args: &[&str]) -> Vec<String> {
        let mut command = vec![
            "--force-local".to_owned(),
            "--project".to_owned(),
            self.project.clone(),
        ];
        command.extend(args.iter().map(|arg| (*arg).to_owned()));
        command
    }

    async fn stop(&self) -> bool {
        if self
            .stopped
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return false;
        }

        close_broker(&self.broker);

        let timeout = incus::stop_timeout(self.grace_ms).to_string();
        let failed = |error: std::io::Error| RunResult {
            code: -1,
            stdout: String::new(),
            stderr: error.to_string(),
        };
        let stopped = (self.run)(
            self.command(&["stop", self.name.as_str(), "--timeout", &timeout]),
            None,
            None,
        )
        .await
        .unwrap_or_else(failed);
        let killed = stopped.code != 0;

        if stopped.code == 0 {
            let deleted = (self.run)(self.command(&["delete", self.name.as_str()]), None, None)
                .await
                .unwrap_or_else(failed);
            if deleted.code != 0 {
                self.log.warn(
                    "the stopped system container could not be deleted",
                    &fields([
                        ("session", self.session_id.as_str().into()),
                        ("name", self.name.as_str().into()),
                        ("detail", deleted.stderr.trim().to_owned().into()),
                    ]),
                );
            }
        } else {
            self.log.warn(
                "the system container did not stop and was deleted",
                &fields([
                    ("session", self.session_id.as_str().into()),
                    ("name", self.name.as_str().into()),
                    ("detail", stopped.stderr.trim().to_owned().into()),
                ]),
            );
            let deleted = (self.run)(
                self.command(&["delete", "--force", self.name.as_str()]),
                None,
                None,
            )
            .await
            .unwrap_or_else(failed);
            if deleted.code != 0 {
                self.log.warn(
                    "the system container could not be deleted even by force",
                    &fields([
                        ("session", self.session_id.as_str().into()),
                        ("name", self.name.as_str().into()),
                        ("detail", deleted.stderr.trim().to_owned().into()),
                    ]),
                );
            }
        }

        self.spawned.kill(nix::sys::signal::Signal::SIGKILL);
        if let Some(directory) = &self.broker_dir {
            let _ = std::fs::remove_dir_all(directory);
        }
        killed
    }
}

/// One sandbox backend, of exactly the three that exist.
pub enum Backend {
    /// Landlock and seccomp, with no container.
    Bailey(Arc<bailey::BaileySandbox>),
    /// Rootless podman containers.
    Podman(Arc<podman::PodmanSandbox>),
    /// Unprivileged Incus system containers.
    Incus(Arc<incus::IncusSandbox>),
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
            Backend::Incus(incus) => incus.probe().await,
        }
    }

    /// What each launch reads from the configuration, for a reload to
    /// replace.
    pub fn settings(&self) -> backend::LiveSettings {
        match self {
            Backend::Bailey(bailey) => bailey.settings(),
            Backend::Podman(podman) => podman.settings(),
            Backend::Incus(incus) => incus.settings(),
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
            Backend::Incus(incus) => incus.launch(launch).await,
        }
    }

    /// Names of sandboxes this system owns that no live session claims.
    pub async fn list_orphans(&self) -> Result<Vec<String>, backend::SandboxLaunchError> {
        match self {
            Backend::Bailey(bailey) => Ok(bailey.list_orphans()),
            Backend::Podman(podman) => podman.list_orphans().await,
            Backend::Incus(incus) => incus.list_orphans().await,
        }
    }

    /// Removes the named sandboxes, returning how many were removed.
    pub async fn remove_orphans(&self, names: &[String]) -> usize {
        match self {
            Backend::Bailey(bailey) => bailey.remove_orphans(names),
            Backend::Podman(podman) => podman.remove_orphans(names).await,
            Backend::Incus(incus) => incus.remove_orphans(names).await,
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

    fn list_orphans(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, SandboxLaunchError>> + Send + '_>> {
        Box::pin(async move { Self::list_orphans(self).await })
    }

    fn remove_orphans<'a>(
        &'a self,
        names: &'a [String],
    ) -> Pin<Box<dyn Future<Output = usize> + Send + 'a>> {
        Box::pin(async move { Self::remove_orphans(self, names).await })
    }
}
