//! Container backend using rootless podman.
//!
//! The hardening flags below are not a starting point to tune. Each was
//! measured against a real container, and the network options in particular
//! are the difference between a container that can reach host services and one
//! that cannot.
//!
//! The agent's home is not a tmpfs. A tmpfs is owned by the user namespace's
//! root, and with `--userns=keep-id` the agent is not that user, so it could
//! not write there. The agent stores credential state under its home, so an
//! unwritable home rejects every prompt with a permission error.

use std::sync::Arc;

use crate::config::schema::SandboxConfig;
use crate::config::size::parse_size;
use crate::log::Logger;
use crate::sandbox::SandboxHandle;
use crate::sandbox::backend::{
    AGENT_HOME, AGENT_SESSIONS, AgentCommand, CapabilityReport, SESSION_LABEL, STATE_PATH,
    SYSTEM_LABEL, SandboxLaunch, SandboxLaunchError, SandboxUnavailableError, WORKSPACE_PATH,
    agent_command, sandbox_name,
};
use crate::sandbox::spawn::spawn_agent;

/// Network arguments that leave the model provider reachable and the host not.
///
/// Measured on podman 5.8.2 with pasta 2025.12.15: podman's default maps the
/// host at `169.254.1.2`, from which a host service bound to `0.0.0.0` is
/// reachable. Disabling both mappings closes that path while outbound internet
/// and DNS keep working. The legacy `--no-map-gw` spelling does not close it.
pub const RESTRICTED_NETWORK: &str = "pasta:--map-host-loopback,none,--map-guest-addr,none";

/// Builds the full podman argument list for a session.
///
/// Pure, so a test can assert the exact flags without starting a container.
pub fn podman_args(config: &SandboxConfig, launch: &SandboxLaunch) -> Vec<String> {
    let name = sandbox_name(&launch.session_id);
    let network = if config.network == crate::config::schema::NetworkMode::None {
        "none"
    } else {
        RESTRICTED_NETWORK
    };

    let mut args: Vec<String> = [
        "run",
        "--interactive",
        "--rm",
        "--name",
        &name,
        "--label",
        &format!("{SYSTEM_LABEL}=true"),
        "--label",
        &format!("{SESSION_LABEL}={}", launch.session_id),
        "--userns=keep-id",
        "--read-only",
        "--tmpfs",
        "/tmp",
        "--volume",
        &format!("{}:{WORKSPACE_PATH}:rw,Z", launch.project_path),
        "--volume",
        &format!("{}:{STATE_PATH}:rw,Z", launch.state_dir),
        "--workdir",
        WORKSPACE_PATH,
        "--cap-drop=ALL",
        "--security-opt",
        "no-new-privileges",
        &format!("--network={network}"),
        "--memory",
        &config.memory,
        "--cpus",
    ]
    .iter()
    .map(std::string::ToString::to_string)
    .collect();

    // The runtime takes what was written, said the way a number says itself.
    args.push(format!("{}", config.cpus));
    args.push("--pids-limit".to_owned());
    args.push(config.pids.to_string());
    // An fsize ulimit, in bytes, inherited by every process in the container.
    // It caps one file rather than total usage, which no container runtime can
    // bound without a sized filesystem underneath it.
    args.push("--ulimit".to_owned());
    args.push(format!(
        "fsize={}",
        parse_size(&config.file_max).unwrap_or(0)
    ));
    args.push("--env".to_owned());
    args.push(format!("HOME={AGENT_HOME}"));

    // Ahead of the daemon's own, so a name it sets keeps the daemon's value.
    if let Some(env) = &config.env {
        for (key, value) in env {
            args.push("--env".to_owned());
            args.push(format!("{key}={value}"));
        }
    }

    for (key, value) in &launch.env {
        args.push("--env".to_owned());
        args.push(format!("{key}={value}"));
    }

    args.push(config.image.clone());
    args.extend(agent_command(&AgentCommand {
        session_dir: AGENT_SESSIONS.to_owned(),
        provider: launch.provider.clone(),
        model: launch.model.clone(),
        system_prompt_path: crate::sandbox::backend::placed_prompt_path(
            launch.system_prompt_path.as_ref(),
        ),
        resume: launch.resume,
    }));
    args
}

/// Rootless podman, one container per session.
pub struct PodmanSandbox {
    config: SandboxConfig,
    log: Logger,
    run: super::Run,
}

/// Runs the installed tool. Injected for tests.
pub fn run_podman(args: Vec<String>, cwd: Option<String>) -> super::RunFuture<super::RunResult> {
    Box::pin(async move {
        let mut command = tokio::process::Command::new("podman");
        command
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let output = command.output().await?;
        Ok(super::RunResult {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    })
}

/// The daemon's own runner for the installed tool.
pub fn run_podman_arc() -> super::Run {
    Arc::new(run_podman)
}

impl PodmanSandbox {
    /// A backend over the installed tool.
    pub fn new(config: SandboxConfig, log: Logger, run: super::Run) -> Self {
        Self { config, log, run }
    }

    async fn call(&self, args: &[&str]) -> super::RunResult {
        let args: Vec<String> = args.iter().map(std::string::ToString::to_string).collect();
        (self.run)(args, None)
            .await
            .unwrap_or_else(|error| super::RunResult {
                code: -1,
                stdout: String::new(),
                stderr: error.to_string(),
            })
    }

    /// Checks that this backend can run here and reports what it can enforce.
    ///
    /// Returns [`SandboxUnavailableError`] when it cannot run at all. It never
    /// falls back to another backend or to running unconfined.
    pub async fn probe(&self) -> Result<CapabilityReport, SandboxUnavailableError> {
        let mut reasons = Vec::new();

        let info = self
            .call(&["info", "--format", "{{.Host.Security.Rootless}}"])
            .await;
        if info.code != 0 {
            reasons.push("podman is not installed, or `podman info` failed".to_owned());
        } else if info.stdout.trim() != "true" {
            reasons.push("podman is not running rootless, which this backend requires".to_owned());
        }

        if reasons.is_empty() {
            let image = self.call(&["image", "exists", &self.config.image]).await;
            if image.code != 0 {
                reasons.push(format!(
                    "the configured image {} is not present; build it before starting",
                    self.config.image
                ));
            }
        }

        if !reasons.is_empty() {
            return Err(SandboxUnavailableError {
                backend: crate::config::schema::SandboxBackend::Podman,
                reasons,
            });
        }

        let notes = vec![
            format!(
                "sessions run in rootless containers from {}",
                self.config.image
            ),
            if self.config.network == crate::config::schema::NetworkMode::None {
                "sessions have no network, so the agent cannot reach a model provider".to_owned()
            } else {
                "sessions reach the model provider, and host services are unreachable".to_owned()
            },
            format!(
                "limits per session: memory {}, cpus {}, pids {}",
                self.config.memory, self.config.cpus, self.config.pids
            ),
            format!(
                "no single file may exceed {}, set as an fsize ulimit on the container",
                self.config.file_max
            ),
            // A note rather than a gap: the daemon never claims to enforce a
            // disk total, so calling it an unenforceable guarantee would make
            // requireFullEnforcement refuse to start on every host forever.
            format!(
                "a session is stopped once it has written {}, which is measured rather than \
                 enforced",
                self.config.disk
            ),
        ];

        Ok(CapabilityReport {
            backend: crate::config::schema::SandboxBackend::Podman,
            gaps: Vec::new(),
            notes,
        })
    }

    /// Starts one session's sandbox.
    pub fn launch(&self, launch: &SandboxLaunch) -> Result<SandboxHandle, SandboxLaunchError> {
        let name = sandbox_name(&launch.session_id);
        let args = podman_args(&self.config, launch);
        let spawned = spawn_agent("podman", &args, None, None)
            .map_err(|error| SandboxLaunchError(error.to_string()))?;
        let spawned = Arc::new(spawned);
        self.log.info(
            "container started",
            &crate::log::fields([
                ("session", launch.session_id.as_str().into()),
                ("name", name.as_str().into()),
            ]),
        );

        Ok(SandboxHandle::Podman(Box::new(super::PodmanStop {
            session_id: launch.session_id.clone(),
            name,
            project_path: launch.project_path.clone(),
            spawned,
            run: Arc::clone(&self.run),
            grace_ms: self.config.grace_period_ms,
            log: self.log.clone(),
            stopped: std::sync::atomic::AtomicBool::new(false),
        })))
    }

    /// Names of sandboxes this system owns that no live session claims.
    pub async fn list_orphans(&self) -> Result<Vec<String>, SandboxLaunchError> {
        let result = self
            .call(&[
                "ps",
                "--all",
                "--filter",
                &format!("label={SYSTEM_LABEL}=true"),
                "--format",
                "{{.Names}}",
            ])
            .await;
        if result.code != 0 {
            return Err(SandboxLaunchError(format!(
                "could not list containers: {}",
                result.stderr.trim()
            )));
        }
        Ok(result
            .stdout
            .split('\n')
            .map(|line| line.trim().to_owned())
            .filter(|line| !line.is_empty())
            .collect())
    }

    /// Removes the named sandboxes, returning how many were removed.
    pub async fn remove_orphans(&self, names: &[String]) -> usize {
        let mut removed = 0;
        for name in names {
            let result = self.call(&["rm", "--force", name]).await;
            if result.code == 0 {
                removed += 1;
            } else {
                self.log.warn(
                    "could not remove a leftover container",
                    &crate::log::fields([("name", name.as_str().into())]),
                );
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests;
