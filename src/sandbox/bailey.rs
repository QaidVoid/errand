//! Confines a session with Landlock and seccomp, using no container.
//!
//! The agent runs as a host process with its own namespaces and a filesystem
//! policy that names everything it may touch. What it may reach is bounded by
//! the generated policy rather than by an image, which is why the policy is a
//! module of its own.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::config::schema::EgressMode;
use crate::config::schema::NetworkMode;
use crate::config::schema::SandboxBackend;
use crate::config::schema::SandboxConfig;
use crate::log::fields;
use crate::log::{LogValue, Logger};
use crate::sandbox::BaileyStop;
use crate::sandbox::SandboxHandle;
use crate::sandbox::agent_config::{BrokeredProvider, write_agent_config};
use crate::sandbox::backend::{
    AGENT_SESSIONS, AgentCommand, CapabilityReport, SandboxLaunch, SandboxLaunchError,
    SandboxUnavailableError, agent_command, fresh_disk_tmp, placed_prompt_path, sandbox_name,
};
use crate::sandbox::policy::{
    AGENT_PROFILE, OFFLINE_PROFILE, PolicyOptions, RESOLV_CONF, RESOLV_FILENAME, policy_contents,
    policy_path,
};
use crate::sandbox::runtime::which;
use crate::sandbox::runtime::{AgentRuntime, Lookup, agent_runtime};
use crate::sandbox::spawn::spawn_agent;

/// What the tool prints when it read a policy and then ignored it.
const NOT_APPLYING: &str = "not applying";

/// What crosses from the daemon's environment into the sandbox tool's.
///
/// `BAILEY_CGROUP_ROOT` names a cgroup the tool may create children in, which
/// is the only way per-session memory, cpu, and process limits are applied at
/// all. It is a path rather than a secret, and without it here an operator can
/// set it on the service and watch it have no effect.
const INHERITED_VARIABLES: [&str; 5] = ["PATH", "LANG", "LC_ALL", "TERM", "BAILEY_CGROUP_ROOT"];

/// Runs the installed tool. Injected for tests.
pub fn run_bailey(args: Vec<String>, cwd: Option<String>) -> super::RunFuture<super::RunResult> {
    Box::pin(async move {
        let mut command = tokio::process::Command::new("bailey");
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
pub fn run_bailey_arc() -> super::Run {
    Arc::new(run_bailey)
}

/// The environment a session runs with.
///
/// Rebuilt from a named list rather than inherited. The daemon's own
/// environment holds the chat token, and inheriting it wholesale would put
/// that token inside the sandbox.
pub fn session_environment(
    launch_env: &BTreeMap<String, String>,
    source: &BTreeMap<String, String>,
    home: &str,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for name in INHERITED_VARIABLES {
        if let Some(value) = source.get(name) {
            env.insert(name.to_owned(), value.clone());
        }
    }
    // The tool builds its own world from the caller's HOME and must be able to
    // create it, so it is given a host path. The target's HOME is set in the
    // policy instead, to the placed path, which overrides this.
    env.insert("HOME".to_owned(), home.to_owned());
    for (name, value) in launch_env {
        env.insert(name.clone(), value.clone());
    }
    env
}

/// Reads gaps out of what the tool reports about this host.
///
/// Parsed rather than hardcoded, so this does not drift from what the tool
/// actually does.
pub fn parse_doctor(doctor: &str) -> (Vec<String>, Vec<String>) {
    let mut gaps = Vec::new();
    let mut unavailable = Vec::new();
    let lines: Vec<String> = doctor
        .split('\n')
        .map(|line| line.trim().to_owned())
        .collect();

    let landlock = lines.iter().find(|line| line.starts_with("landlock:"));
    if landlock.is_none_or(|line| line.contains("no")) {
        unavailable
            .push("the kernel does not provide Landlock, which this backend requires".to_owned());
    }

    let userns = lines
        .iter()
        .find(|line| line.starts_with("user namespaces:"));
    if userns.is_some_and(|line| line.ends_with("no")) {
        unavailable.push(
            "the kernel does not allow user namespaces, which this backend requires".to_owned(),
        );
    }

    let cgroups = lines
        .iter()
        .find(|line| line.starts_with("cgroup delegation:"));
    if cgroups.is_some_and(|line| line.ends_with("no")) {
        gaps.push(
            "per-session memory, cpu, and process limits are not applied: this host reports no \
             cgroup delegation. A limit on the daemon as a whole still applies to it and every \
             session together"
                .to_owned(),
        );
    }

    (gaps, unavailable)
}

/// The address the private namespace reaches the egress broker at.
///
/// A link-local address that routes nowhere on its own, deliberately not the
/// cloud metadata address.
pub const EGRESS_MAP_ADDRESS: &str = "169.254.169.1";

/// Path the broker answers as the provider on, under its own address.
pub const PROVIDER_PREFIX: &str = "/provider";

/// The path the broker answers one provider on.
pub fn provider_prefix(provider: &str) -> String {
    format!("{PROVIDER_PREFIX}/{provider}")
}

/// What a session's agent is told a provider's base URL is.
pub fn provider_broker_url(port: u16, provider: &str) -> String {
    format!(
        "http://{EGRESS_MAP_ADDRESS}:{port}{}",
        provider_prefix(provider)
    )
}

/// What the daemon holds back from a session, and what it gives instead.
///
/// The credential never crosses into a sandbox: the broker puts it on at the
/// other end, so what a session carries is a nonce that only the broker
/// honours.
#[derive(Debug, Clone)]
pub struct ProviderBrokering {
    /// The variable the agent reads each provider's key from, by provider
    /// name, for those that name one.
    pub credential_names: BTreeMap<String, String>,
    /// What stands in for each brokered provider's credential, by provider
    /// name.
    pub nonces: BTreeMap<String, String>,
}

/// A session's environment with every provider key taken out.
///
/// The session is handed the key of the provider it starts on, whichever that
/// is. Under a broker that variable carries the provider's nonce instead,
/// and a provider the broker has no route to loses the variable altogether,
/// so no real key crosses into the sandbox.
pub fn brokered_env(
    env: &BTreeMap<String, String>,
    brokering: &ProviderBrokering,
) -> BTreeMap<String, String> {
    let mut env = env.clone();
    for (provider, name) in &brokering.credential_names {
        match brokering.nonces.get(provider) {
            Some(nonce) => env.insert(name.clone(), nonce.clone()),
            None => env.remove(name),
        };
    }
    env
}

/// Extras the daemon supplies, which a test has no need of.
#[derive(Default)]
pub struct BaileyOptions {
    /// Host loopback port of the broker, under `egress.mode = proxy`.
    pub egress_proxy_port: Option<u16>,
    /// What stands in for the provider credential inside a session.
    pub brokering: Option<ProviderBrokering>,
    /// Finds the agent. Injected so a test needs no agent installed.
    pub lookup: Option<Lookup>,
    /// The host store's definitions of each defined provider's models, by
    /// provider, for an entry naming one of them to be laid over.
    pub built_in: BTreeMap<String, Vec<Value>>,
}

/// The proxy URL a brokered session's tools use, for a given broker port.
pub fn egress_proxy_url(port: u16) -> String {
    format!("http://{EGRESS_MAP_ADDRESS}:{port}")
}

/// The `--egress-proxy` value for a given broker port.
pub fn egress_proxy_endpoint(port: u16) -> String {
    format!("{EGRESS_MAP_ADDRESS}:{port}")
}

/// The arguments the tool is run with for one session.
///
/// Proxy mode forces every connection through the broker; --egress-proxy
/// implies --proxy-net, so the plain hide-address flag is not added on top.
pub fn bailey_args(
    config: &SandboxConfig,
    launch: &SandboxLaunch,
    policy: &str,
    egress_proxy_port: Option<u16>,
) -> Vec<String> {
    let brokered = config.egress.mode == EgressMode::Proxy && egress_proxy_port.is_some();
    let mut args: Vec<String> = vec!["run".to_owned(), "--isolate".to_owned()];
    if brokered {
        if let Some(port) = egress_proxy_port {
            args.push("--egress-proxy".to_owned());
            args.push(egress_proxy_endpoint(port));
        }
    } else if config.hide_host_address {
        args.push("--proxy-net".to_owned());
    }
    args.push("--config".to_owned());
    args.push(policy.to_owned());
    args.push("--profile".to_owned());
    let profile = if config.network == NetworkMode::None {
        OFFLINE_PROFILE
    } else {
        AGENT_PROFILE
    };
    args.push(profile.to_owned());
    args.push("--".to_owned());
    args.extend(agent_command(&AgentCommand {
        session_dir: AGENT_SESSIONS.to_owned(),
        provider: launch.provider.clone(),
        model: launch.model.clone(),
        system_prompt_path: placed_prompt_path(launch.system_prompt_path.as_ref()),
        resume: launch.resume,
    }));
    args
}

/// Confines sessions as host processes.
pub struct BaileySandbox {
    config: SandboxConfig,
    log: Logger,
    state_root: String,
    run: super::Run,
    options: BaileyOptions,
}

impl BaileySandbox {
    /// A backend over the installed tool.
    pub fn new(
        config: SandboxConfig,
        log: Logger,
        state_root: String,
        run: super::Run,
        options: BaileyOptions,
    ) -> Self {
        Self {
            config,
            log,
            state_root,
            run,
            options,
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

    /// The broker's route for each provider it stands in front of.
    fn brokered_providers(&self) -> BTreeMap<String, BrokeredProvider> {
        let (Some(brokering), Some(port)) =
            (&self.options.brokering, self.options.egress_proxy_port)
        else {
            return BTreeMap::new();
        };
        brokering
            .nonces
            .iter()
            .map(|(name, nonce)| {
                let through = BrokeredProvider {
                    base_url: provider_broker_url(port, name),
                    nonce: nonce.clone(),
                };
                (name.clone(), through)
            })
            .collect()
    }

    /// The operator env, with the proxy variables added under a brokered
    /// session.
    ///
    /// A brokered session reaches the network only through the broker, so its
    /// tools are pointed at it with the standard proxy variables, lower and
    /// upper case, since programs read one or the other. Outside proxy mode
    /// this is the operator env unchanged.
    fn egress_env(&self) -> Option<BTreeMap<String, String>> {
        let mut base = self.config.env.clone().unwrap_or_default();
        if self.config.egress.mode != EgressMode::Proxy {
            return if base.is_empty() { None } else { Some(base) };
        }
        let Some(port) = self.options.egress_proxy_port else {
            return if base.is_empty() { None } else { Some(base) };
        };
        let url = egress_proxy_url(port);
        base.insert("HTTPS_PROXY".to_owned(), url.clone());
        base.insert("https_proxy".to_owned(), url.clone());
        base.insert("HTTP_PROXY".to_owned(), url.clone());
        base.insert("http_proxy".to_owned(), url);
        // The broker answers as the provider on its own address, so that one
        // is reached directly rather than tunnelled through itself.
        base.insert("NO_PROXY".to_owned(), EGRESS_MAP_ADDRESS.to_owned());
        base.insert("no_proxy".to_owned(), EGRESS_MAP_ADDRESS.to_owned());
        // The agent runs on Node, whose built-in fetch ignores the proxy
        // variables unless this is set. Without it a session bypasses the
        // broker, reaches nothing under the netns lockdown, and stalls on the
        // provider.
        base.insert("NODE_USE_ENV_PROXY".to_owned(), "1".to_owned());
        Some(base)
    }

    /// Checks that this backend can run here and reports what it can enforce.
    ///
    /// Returns [`SandboxUnavailableError`] when it cannot run at all. It never
    /// falls back to another backend or to running unconfined.
    pub async fn probe(&self) -> Result<CapabilityReport, SandboxUnavailableError> {
        let doctor = self.call(&["doctor"]).await;
        if doctor.code != 0 {
            return Err(SandboxUnavailableError {
                backend: SandboxBackend::Bailey,
                reasons: vec!["bailey is not installed, or `bailey doctor` failed".to_owned()],
            });
        }

        let combined = format!("{}\n{}", doctor.stdout, doctor.stderr);
        let (gaps, unavailable) = parse_doctor(&combined);
        if !unavailable.is_empty() {
            return Err(SandboxUnavailableError {
                backend: SandboxBackend::Bailey,
                reasons: unavailable,
            });
        }

        // Without this, a version too old for the generated policy surfaces as
        // every session failing to launch rather than once at startup, where
        // it is actionable.
        if !self.accepts_generated_policy().await {
            return Err(SandboxUnavailableError {
                backend: SandboxBackend::Bailey,
                reasons: vec![
                    "the installed bailey does not accept the policy this backend writes, which \
                     needs resources.file_max and relocatable grants; update bailey"
                        .to_owned(),
                ],
            });
        }

        // This backend runs the host's own agent rather than one baked into an
        // image, so an agent that is not installed is a reason to refuse to
        // start.
        let lookup = self.lookup();
        if agent_runtime(&lookup).is_none() {
            return Err(SandboxUnavailableError {
                backend: SandboxBackend::Bailey,
                reasons: vec![
                    "the pi agent is not on PATH, and this backend runs the host's own \
                     installation"
                        .to_owned(),
                ],
            });
        }

        let mut notes = vec![
            "sessions run as confined host processes using the host's own tools".to_owned(),
            format!(
                "no single file may exceed {}, enforced as an rlimit and so holding with or \
                 without cgroups",
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
            if self.config.network == NetworkMode::None {
                "sessions have no network, so the agent cannot reach a model provider".to_owned()
            } else {
                "sessions reach the model provider over TCP 443, and outbound access is not \
                 restricted by destination"
                    .to_owned()
            },
        ];

        // Said out loud, so the report never describes a tighter boundary than
        // the one actually applied. Write is named separately: it is the grant
        // that lets a session change something outside its own project.
        if let Some(extra) = &self.config.policy_extra {
            let granted = extra.read.len() + extra.write.len() + extra.execute.len();
            notes.push(format!(
                "sandbox.policyExtra grants {granted} path(s) beyond the generated policy"
            ));
            if !extra.write.is_empty() {
                notes.push(format!(
                    "  {} of them writable, so a session can change what is outside its \
                     project: {}",
                    extra.write.len(),
                    extra.write.join(", ")
                ));
            }
        }

        let on_path = self.config.path_extra.clone().unwrap_or_default();
        if !on_path.is_empty() {
            notes.push(format!(
                "sessions find programs in {}, ahead of the system copies",
                on_path.join(", ")
            ));
        }

        // Names only. A value is the operator's own and may be anything, and a
        // report is read in places a configuration file is not.
        if self.config.env.as_ref().is_some_and(|env| !env.is_empty()) {
            let env = self.config.env.as_ref().expect("checked above");
            let mut names: Vec<String> = env.keys().cloned().collect();
            names.sort();
            notes.push(format!(
                "sessions are given {} from configuration",
                names.join(", ")
            ));
        }

        Ok(CapabilityReport {
            backend: SandboxBackend::Bailey,
            gaps,
            notes,
        })
    }

    fn lookup(&self) -> Lookup {
        self.options
            .lookup
            .clone()
            .unwrap_or_else(|| Arc::new(which))
    }

    /// Starts one session's sandbox.
    pub async fn launch(
        self: &Arc<Self>,
        launch: &SandboxLaunch,
    ) -> Result<SandboxHandle, SandboxLaunchError> {
        let lookup = self.lookup();
        let Some(runtime) = agent_runtime(&lookup) else {
            return Err(SandboxLaunchError(
                "the pi agent is not on PATH, so there is nothing for a confined session to run"
                    .to_owned(),
            ));
        };

        tokio::fs::create_dir_all(&launch.state_dir)
            .await
            .map_err(|error| SandboxLaunchError(error.to_string()))?;
        if self.config.disk_tmp {
            // The grant and TMPDIR point here, so it has to exist before the
            // policy is applied.
            fresh_disk_tmp(&launch.state_dir).await?;
        }
        write_agent_config(
            launch,
            &self.brokered_providers(),
            &self.options.built_in,
            self.config.egress.mode != EgressMode::Proxy,
        )
        .await?;
        let env = match &self.options.brokering {
            Some(brokering) => brokered_env(&launch.env, brokering),
            None => launch.env.clone(),
        };
        let launch = SandboxLaunch {
            env,
            ..launch.clone()
        };
        let resolv = self.write_resolv_conf().await;
        let policy = policy_path(&launch);
        tokio::fs::write(
            &policy,
            policy_contents(&PolicyOptions {
                launch: &launch,
                network: self.config.network,
                egress_ports: Some(&self.config.egress_ports),
                runtime: &runtime,
                file_max: &self.config.file_max,
                tmp_size: &self.config.tmp_size,
                shm_size: &self.config.shm_size,
                disk_tmp: self.config.disk_tmp,
                resolv_conf: &resolv,
                extra: self.config.policy_extra.as_ref(),
                env: self.egress_env().as_ref(),
                path_extra: self.config.path_extra.as_deref(),
            }),
        )
        .await
        .map_err(|error| SandboxLaunchError(error.to_string()))?;

        let trusted = self.call(&["trust", &policy]).await;
        if trusted.code != 0 {
            return Err(SandboxLaunchError(format!(
                "could not trust the generated policy at {policy}: {}",
                trusted.stderr.trim()
            )));
        }
        self.verify_policy_applies(&policy).await?;

        // Started from the project, so the tool enters it after the pivot. It
        // is the agent's working directory, and without this the agent starts
        // in a private home rather than the project it was asked to work in.
        let mut session_env_source = BTreeMap::new();
        for (name, value) in std::env::vars() {
            session_env_source.insert(name, value);
        }
        let session_env = session_environment(
            &launch.env,
            &session_env_source,
            &format!("{}/home", launch.state_dir),
        );
        let args = bailey_args(
            &self.config,
            &launch,
            &policy,
            self.options.egress_proxy_port,
        );
        let cwd = launch.project_path.clone();
        let run = Arc::clone(&self.run);
        let spawned = spawn_agent("bailey", &args, Some(&session_env), Some(&cwd))
            .map_err(|error| SandboxLaunchError(error.to_string()))?;
        let spawned = Arc::new(spawned);
        let name = sandbox_name(&launch.session_id);

        self.log.info(
            "confined process started",
            &fields([
                ("session", launch.session_id.as_str().into()),
                ("name", name.as_str().into()),
                ("pid", LogValue::Number(i64::from(spawned.pid))),
            ]),
        );

        Ok(SandboxHandle::Bailey(Box::new(BaileyStop {
            session_id: launch.session_id.clone(),
            name,
            project_path: launch.project_path.clone(),
            spawned,
            policy,
            run,
            grace_ms: self.config.grace_period_ms,
            log: self.log.clone(),
            stopped: std::sync::atomic::AtomicBool::new(false),
        })))
    }

    /// Names of sandboxes this system owns that no live session claims.
    ///
    /// The tool runs inside a PID namespace, so killing the launcher removes
    /// every process the agent started. There is nothing to reap afterwards,
    /// and a previous daemon's processes died with it.
    #[expect(
        clippy::unused_self,
        reason = "the signature matches the other backend's, which is the contract"
    )]
    pub fn list_orphans(&self) -> Vec<String> {
        Vec::new()
    }

    /// Removes the named sandboxes, returning how many were removed.
    #[expect(
        clippy::unused_self,
        reason = "the signature matches the other backend's, which is the contract"
    )]
    pub fn remove_orphans(&self, _names: &[String]) -> usize {
        0
    }

    /// Writes the resolver a session is given, and returns its path.
    ///
    /// Rewritten on every launch rather than once, so a daemon whose idea of
    /// the resolver changed does not keep handing out the file it wrote first.
    async fn write_resolv_conf(&self) -> String {
        tokio::fs::create_dir_all(&self.state_root)
            .await
            .expect("the state root is writable");
        let path = std::path::Path::new(&self.state_root)
            .join(RESOLV_FILENAME)
            .to_string_lossy()
            .into_owned();
        tokio::fs::write(&path, format!("{RESOLV_CONF}\n"))
            .await
            .expect("the resolver file is writable");
        path
    }

    /// Whether the installed tool understands the policy this backend writes.
    ///
    /// It rejects a config holding a key it does not know, so a version older
    /// than a feature used here fails every launch. Checking the shape rather
    /// than one key means a later addition is covered by the same check.
    async fn accepts_generated_policy(&self) -> bool {
        let probe_dir = std::path::Path::new(&self.state_root).join("probe");
        let probe = probe_dir.to_string_lossy().into_owned();
        tokio::fs::create_dir_all(&probe_dir)
            .await
            .expect("the probe directory is writable");
        let policy = probe_dir.join("shape.toml").to_string_lossy().into_owned();
        let launch = SandboxLaunch {
            session_id: "probe".to_owned(),
            project_path: probe.clone(),
            state_dir: probe.clone(),
            env: BTreeMap::new(),
            provider: "probe".to_owned(),
            ..SandboxLaunch::default()
        };
        let resolver = join_resolv(&probe);
        tokio::fs::write(
            &policy,
            policy_contents(&PolicyOptions {
                launch: &launch,
                network: self.config.network,
                egress_ports: None,
                // The probe runs `true`, so it needs nothing of the agent
                // granted.
                runtime: &AgentRuntime::default(),
                file_max: &self.config.file_max,
                tmp_size: &self.config.tmp_size,
                shm_size: &self.config.shm_size,
                disk_tmp: self.config.disk_tmp,
                resolv_conf: &resolver,
                extra: None,
                env: None,
                path_extra: None,
            }),
        )
        .await
        .expect("the probe policy is writable");
        tokio::fs::write(&resolver, format!("{RESOLV_CONF}\n"))
            .await
            .expect("the probe resolver is writable");

        let _ = self.call(&["trust", &policy]).await;
        let result = self
            .call_in(
                &["run", "--config", &policy, "--quiet", "--", "true"],
                Some(&probe),
            )
            .await;
        let _ = self.call(&["untrust", &policy]).await;
        result.code == 0
    }

    /// Confirms the policy will be applied before a session is started.
    ///
    /// The tool does not fail a run whose policy it declined to trust. It
    /// warns and continues without the policy, which would start a session
    /// with no project grant at all. A cheap confined command is run first so
    /// that case becomes a launch failure rather than a silently unconfined
    /// session.
    async fn verify_policy_applies(&self, policy: &str) -> Result<(), SandboxLaunchError> {
        let profile = if self.config.network == NetworkMode::None {
            OFFLINE_PROFILE
        } else {
            AGENT_PROFILE
        };
        let check = self
            .call(&[
                "run",
                "--isolate",
                "--config",
                policy,
                "--profile",
                profile,
                "--quiet",
                "--",
                "true",
            ])
            .await;
        if check.stderr.contains(NOT_APPLYING) {
            return Err(SandboxLaunchError(format!(
                "bailey declined to apply the generated policy at {policy}, which would leave \
                 the session without its project grant: {}",
                check.stderr.trim()
            )));
        }
        Ok(())
    }

    async fn call_in(&self, args: &[&str], cwd: Option<&str>) -> super::RunResult {
        let args: Vec<String> = args.iter().map(std::string::ToString::to_string).collect();
        let cwd = cwd.map(str::to_owned);
        (self.run)(args, cwd)
            .await
            .unwrap_or_else(|error| super::RunResult {
                code: -1,
                stdout: String::new(),
                stderr: error.to_string(),
            })
    }
}

fn join_resolv(root: &str) -> String {
    std::path::Path::new(root)
        .join(RESOLV_FILENAME)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests;
