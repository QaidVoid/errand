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

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::config::schema::EgressMode;
use crate::config::schema::NetworkMode;
use crate::config::schema::SandboxBackend;
use crate::config::schema::SandboxConfig;
use crate::config::size::parse_size;
use crate::log::Logger;
use crate::log::fields;
use crate::sandbox::SandboxHandle;
use crate::sandbox::agent_config::write_agent_config;
use crate::sandbox::backend::{
    AGENT_HOME, AgentCommand, CapabilityReport, SESSION_LABEL, STATE_PATH, SYSTEM_LABEL,
    SandboxLaunch, SandboxLaunchError, SandboxUnavailableError, WORKSPACE_PATH, agent_command,
    agent_memory_path, disk_tmp_dir, fresh_disk_tmp, sandbox_name,
};
use crate::sandbox::broker::{BrokerTemplate, SessionBroker};
use crate::sandbox::egress::{
    PROXY_VARIABLES, add_proxy_env, brokered_providers, session_env, start_session_broker,
};
use crate::sandbox::kage_providers::{KageProviders, LIST_ARGS, Probed};
use crate::sandbox::spawn::spawn_agent;
use crate::session::github::TOKEN_VARIABLE;

/// Network arguments for a session that is not brokered: the internet
/// reachable, and the host not.
///
/// Measured on podman 5.8.2 with pasta 2025.12.15: podman's default maps the
/// host at `169.254.1.2`, from which a host service bound to `0.0.0.0` is
/// reachable. Disabling both mappings closes that path while outbound internet
/// and DNS keep working. The legacy `--no-map-gw` spelling does not close it.
pub const RESTRICTED_NETWORK: &str = "pasta:--map-host-loopback,none,--map-guest-addr,none";

/// Where a brokered container reaches its broker: its own loopback, which
/// pasta forwards to the broker's port on the host's.
pub const BROKER_ADDRESS: &str = "127.0.0.1";

/// Network arguments for a brokered session: the broker's one port and
/// nothing else.
///
/// Measured on podman 6.1.4 with pasta `2026_09_25`. `--splice-only` gives the
/// container loopback and no other interface or route, and `-T` forwards its
/// loopback port to the same port on the host's, where the broker listens. A
/// connect to any other address is unreachable (`ENETUNREACH`): a public one,
/// IPv6, and every address of the host itself, its bridges and VPNs included,
/// which a container with a route out could reach whatever its source
/// address. Nothing resolves. The broker resolves names itself.
pub fn brokered_network(port: u16) -> String {
    format!("pasta:--splice-only,-T,{port}")
}

/// A session's `podman run` arguments and the secrets they name.
pub struct PodmanCommand {
    /// The arguments. A secret appears as a bare `--env NAME`, so its value
    /// never sits on a command line any local user can read; every other
    /// variable is written out.
    pub args: Vec<String>,
    /// The value of every secret the arguments name, which podman reads from
    /// its own environment.
    pub env: BTreeMap<String, String>,
}

/// Builds the full podman command for a session, with `broker` when its
/// egress is brokered.
///
/// Pure, so a test can assert the exact flags without starting a container.
pub fn podman_args(
    config: &SandboxConfig,
    launch: &SandboxLaunch,
    broker: Option<&SessionBroker>,
) -> PodmanCommand {
    let name = sandbox_name(&launch.session_id);
    let brokered = broker.filter(|_| config.egress.mode == EgressMode::Proxy);
    let network = match (config.network, brokered) {
        (NetworkMode::None, _) => "none".to_owned(),
        (_, Some(broker)) => brokered_network(broker.port),
        (_, None) => RESTRICTED_NETWORK.to_owned(),
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
        // The probe checked the image is here. A pull now would go through
        // the session's own proxy settings, which podman reads too.
        "--pull=never",
        "--read-only",
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
    // Sizes cross as bytes: the size syntax allows spellings a mount option
    // does not.
    if config.disk_tmp {
        args.push("--volume".to_owned());
        args.push(format!("{}:/tmp:rw,Z", disk_tmp_dir(&launch.state_dir)));
    } else {
        args.push("--tmpfs".to_owned());
        args.push(format!(
            "/tmp:size={},mode=1777",
            parse_size(&config.tmp_size).unwrap_or(0)
        ));
    }
    args.push("--shm-size".to_owned());
    args.push(parse_size(&config.shm_size).unwrap_or(0).to_string());
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

    // Laid down in order, so a name the daemon sets keeps the daemon's value.
    let mut env = config.env.clone().unwrap_or_default();
    if let Some(broker) = brokered
        && config.network != NetworkMode::None
    {
        add_proxy_env(&mut env, BROKER_ADDRESS, broker);
    }
    env.extend(launch.env.clone());
    // Only a secret crosses by name: a provider key or its stand-in, the
    // GitHub token, and a proxy URL with its credential. A bare name is
    // resolved from podman's own environment, which is then podman's too, so
    // anything else, a session's XDG directories above all, would move where
    // podman keeps its own storage.
    let secret = |name: &str| {
        launch.credential_names.values().any(|key| key == name)
            || name == TOKEN_VARIABLE
            || PROXY_VARIABLES.contains(&name)
    };
    let mut secrets = BTreeMap::new();
    for (name, value) in env {
        args.push("--env".to_owned());
        if secret(&name) {
            args.push(name.clone());
            secrets.insert(name, value);
        } else {
            args.push(format!("{name}={value}"));
        }
    }

    args.push(config.image.clone());
    args.extend(agent_command(&AgentCommand {
        provider: launch.provider.clone(),
        model: launch.model.clone(),
        // The host path only says the block was written. The agent reads it
        // where the state directory is mounted, not where the daemon put it.
        system_file: launch
            .system_prompt_path
            .as_ref()
            .map(|_| agent_memory_path()),
    }));
    PodmanCommand { args, env: secrets }
}

/// The command a session's container is started with: its provider keys
/// swapped for its broker's stand-ins, when it has one.
pub fn session_command(
    config: &SandboxConfig,
    launch: &SandboxLaunch,
    broker: Option<&SessionBroker>,
) -> PodmanCommand {
    let launch = SandboxLaunch {
        env: session_env(&launch.env, broker),
        ..launch.clone()
    };
    podman_args(config, &launch, broker)
}

/// Rootless podman, one container per session.
pub struct PodmanSandbox {
    config: SandboxConfig,
    log: Logger,
    run: super::Run,
    built_in: BTreeMap<String, Vec<Value>>,
    broker: Option<BrokerTemplate>,
    kage_providers: Probed,
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
    ///
    /// `built_in` is the host store's definitions of each defined provider's
    /// models, for an entry naming one of them to be laid over. `broker` is
    /// what each session's egress broker is built from, under
    /// `egress.mode = proxy`.
    pub fn new(
        config: SandboxConfig,
        log: Logger,
        run: super::Run,
        built_in: BTreeMap<String, Vec<Value>>,
        broker: Option<BrokerTemplate>,
    ) -> Self {
        Self {
            config,
            log,
            run,
            built_in,
            broker,
            kage_providers: Probed::default(),
        }
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
                backend: SandboxBackend::Podman,
                reasons,
            });
        }

        // The image's kage, not the host's: it is the one sessions run, and
        // the two need not be the same build.
        let mut ask = vec!["run", "--rm", "--network=none", &self.config.image, "kage"];
        ask.extend(LIST_ARGS);
        let kage_providers = KageProviders::from_run(&self.call(&ask).await).map_err(|reason| {
            SandboxUnavailableError {
                backend: SandboxBackend::Podman,
                reasons: vec![format!("in {}, {reason}", self.config.image)],
            }
        })?;
        self.kage_providers.keep(kage_providers.clone());

        let notes = vec![
            format!(
                "sessions run in rootless containers from {}",
                self.config.image
            ),
            if self.config.network == NetworkMode::None {
                "sessions have no network, so the agent cannot reach a model provider".to_owned()
            } else if self.brokers() {
                "sessions reach the network only through their egress broker, and host \
                 services are unreachable"
                    .to_owned()
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

        // Proxy mode promises the broker, so a backend that would run without
        // one says so rather than pass for brokered.
        let gaps = if self.config.egress.mode == EgressMode::Proxy && !self.brokers() {
            vec![
                "egress.mode is proxy but no broker is wired for this backend: provider keys \
                 and the GitHub token cross whole into the container, and its connections go \
                 anywhere"
                    .to_owned(),
            ]
        } else {
            Vec::new()
        };

        Ok(CapabilityReport {
            backend: SandboxBackend::Podman,
            gaps,
            notes,
            kage_providers,
        })
    }

    /// Whether sessions here are brokered.
    fn brokers(&self) -> bool {
        self.config.egress.mode == EgressMode::Proxy && self.broker.is_some()
    }

    /// Starts one session's sandbox.
    pub async fn launch(
        &self,
        launch: &SandboxLaunch,
    ) -> Result<SandboxHandle, SandboxLaunchError> {
        // Started first and dropped on any failure below, which closes it.
        let broker = start_session_broker(
            self.broker.as_ref(),
            &launch.session_id,
            BROKER_ADDRESS,
            &self.log,
        )
        .await?;
        if self.config.disk_tmp {
            fresh_disk_tmp(&launch.state_dir).await?;
        }
        // The agent reads every provider's key from the environment, which
        // under a broker carries the session's stand-in for it instead, and
        // reaches each brokered provider through the broker.
        write_agent_config(
            launch,
            &brokered_providers(BROKER_ADDRESS, broker.as_ref()),
            &self.built_in,
            &launch.credential_names,
            &self.log,
            &self.kage_providers.get()?,
        )
        .await?;
        let name = sandbox_name(&launch.session_id);
        let mut command = session_command(&self.config, launch, broker.as_ref());
        // podman finds its network helper on its PATH, and is otherwise given
        // only the session's secrets.
        if let Ok(path) = std::env::var("PATH") {
            command.env.entry("PATH".to_owned()).or_insert(path);
        }
        let spawned = spawn_agent("podman", &command.args, Some(&command.env), None)
            .map_err(|error| SandboxLaunchError(error.to_string()))?;
        let spawned = Arc::new(spawned);
        self.log.info(
            "container started",
            &fields([
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
            broker: std::sync::Mutex::new(broker.map(|broker| broker.broker)),
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
                    &fields([("name", name.as_str().into())]),
                );
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests;
