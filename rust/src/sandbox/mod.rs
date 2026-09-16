//! The boundary: the policy a confined agent is given, the two backends that
//! enforce it, and the spawn that starts a session.

use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use crate::log::Logger;

pub mod backend;
pub mod bailey;
pub mod broker;
pub mod paths;
pub mod podman;
pub mod policy;
pub mod runtime;
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

/// How one running sandbox is ended, per backend.
pub enum HandleStop {
    /// Baileys are ended by signalling the process group and untrusting.
    Bailey(Box<BaileyStop>),
    /// Podman containers are stopped by asking podman, then removed.
    Podman(Box<PodmanStop>),
}

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
    /// The sandbox's discoverable name.
    pub fn name(&self) -> String {
        match self {
            SandboxHandle::Bailey(stop) => stop.name.clone(),
            SandboxHandle::Podman(stop) => stop.name.clone(),
        }
    }

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
                    &crate::log::fields([
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

        #[allow(clippy::cast_sign_loss)]
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
            &crate::log::fields([
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
    /// Which backend this is.
    pub fn name(&self) -> crate::config::schema::SandboxBackend {
        match self {
            Backend::Bailey(_) => crate::config::schema::SandboxBackend::Bailey,
            Backend::Podman(_) => crate::config::schema::SandboxBackend::Podman,
        }
    }

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
            Backend::Podman(podman) => podman.launch(launch),
        }
    }

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

/// Joins onto a root and resolves, for paths built from a name and a root.
pub(crate) fn join_resolved(root: &str, name: &str) -> String {
    resolve_root(&Path::new(root).join(name).to_string_lossy())
}
