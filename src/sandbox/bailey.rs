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
use crate::sandbox::agent_config::write_agent_config;
use crate::sandbox::backend::{
    AgentCommand, CapabilityReport, SandboxLaunch, SandboxLaunchError, SandboxUnavailableError,
    agent_command, agent_memory_path, fresh_disk_tmp, sandbox_name,
};
use crate::sandbox::broker::{BrokerTemplate, SessionBroker};
use crate::sandbox::egress::{
    add_proxy_env, brokered_providers, session_env, start_session_broker,
};
use crate::sandbox::kage_providers::{KageProviders, LIST_ARGS, Probed};
use crate::sandbox::paths;
use crate::sandbox::policy::{
    AGENT_PROFILE, OFFLINE_PROFILE, POLICY_FILENAME, PolicyOptions, RESOLV_CONF, RESOLV_FILENAME,
    policy_contents, policy_path,
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
    run_program("bailey".to_owned(), args, cwd, true)
}

/// Runs the host's kage, named by its path ahead of the arguments. Injected
/// for tests.
///
/// The environment is emptied: the daemon's own holds the chat token, and
/// asking kage what it knows needs none of it.
pub fn run_kage(args: Vec<String>, cwd: Option<String>) -> super::RunFuture<super::RunResult> {
    let mut args = args.into_iter();
    let Some(program) = args.next() else {
        return Box::pin(async { Err(std::io::Error::other("no kage was named to run")) });
    };
    run_program(program, args.collect(), cwd, false)
}

/// Runs `program` with `args` and collects what it wrote, inheriting the
/// daemon's environment only when `inherit` says so.
fn run_program(
    program: String,
    args: Vec<String>,
    cwd: Option<String>,
    inherit: bool,
) -> super::RunFuture<super::RunResult> {
    Box::pin(async move {
        let mut command = tokio::process::Command::new(program);
        if !inherit {
            command.env_clear();
        }
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
    // bailey reads its global config, profiles, and trust store from the XDG
    // directories, falling back to HOME. HOME here is the session's private
    // home, which the agent writes, so leaving them unset would let a planted
    // file run hooks or plant trust as the daemon user before any
    // confinement. Point them at the operator's own directories instead.
    if let Some(dir) = operator_dir(source, "XDG_CONFIG_HOME", ".config") {
        env.insert("XDG_CONFIG_HOME".to_owned(), dir);
    }
    if let Some(dir) = operator_dir(source, "XDG_DATA_HOME", ".local/share") {
        env.insert("XDG_DATA_HOME".to_owned(), dir);
    }
    for (name, value) in launch_env {
        env.insert(name.clone(), value.clone());
    }
    env
}

/// The operator's directory for an XDG variable bailey reads user config or
/// data from: the daemon's own value when it names an absolute path, else the
/// standard subdirectory of the daemon's home. Nothing when neither resolves,
/// in which case the variable stays unset.
fn operator_dir(source: &BTreeMap<String, String>, name: &str, subdir: &str) -> Option<String> {
    if let Some(dir) = source.get(name)
        && !dir.is_empty()
        && std::path::Path::new(dir).is_absolute()
    {
        return Some(dir.clone());
    }
    source
        .get("HOME")
        .filter(|home| !home.is_empty() && std::path::Path::new(home).is_absolute())
        .map(|home| format!("{home}/{subdir}"))
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
    // The tool reports either `landlock: ABI <n>` when the kernel provides it
    // or `landlock: unavailable` when it does not. Matching that shape, and
    // not a substring of it, is what makes a host without Landlock refuse to
    // start instead of reporting full enforcement.
    if landlock.is_none_or(|line| !line.starts_with("landlock: ABI")) {
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

/// Extras the daemon supplies, which a test has no need of.
#[derive(Default)]
pub struct BaileyOptions {
    /// What each session's broker is built from, under `egress.mode = proxy`.
    pub broker: Option<BrokerTemplate>,
    /// Finds the agent. Injected so a test needs no agent installed.
    pub lookup: Option<Lookup>,
    /// Runs the host's kage, its path first, to ask which providers it knows.
    /// Injected so a test needs no agent installed.
    pub kage: Option<super::Run>,
    /// The host store's definitions of each defined provider's models, by
    /// provider, for an entry naming one of them to be laid over.
    pub built_in: BTreeMap<String, Vec<Value>>,
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
        provider: launch.provider.clone(),
        model: launch.model.clone(),
        // The host path only says the block was written. The agent reads it
        // where the state directory is placed, not where the daemon put it.
        system_file: launch
            .system_prompt_path
            .as_ref()
            .map(|_| agent_memory_path()),
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
    kage_providers: Probed,
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

    /// The operator env, with the proxy variables added under a brokered
    /// session, which reaches the network only through the broker. Outside
    /// proxy mode this is the operator env unchanged.
    fn egress_env(&self, broker: Option<&SessionBroker>) -> Option<BTreeMap<String, String>> {
        let mut base = self.config.env.clone().unwrap_or_default();
        if self.config.egress.mode == EgressMode::Proxy
            && let Some(broker) = broker
        {
            add_proxy_env(&mut base, EGRESS_MAP_ADDRESS, broker);
        }
        if base.is_empty() { None } else { Some(base) }
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
        let Some(launcher) = agent_runtime(&lookup).and(lookup("kage")) else {
            return Err(SandboxUnavailableError {
                backend: SandboxBackend::Bailey,
                reasons: vec![
                    "the kage agent is not on PATH, and this backend runs the host's own \
                     installation"
                        .to_owned(),
                ],
            });
        };
        let kage_providers = self.ask_kage(&launcher).await?;

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
            kage_providers,
        })
    }

    /// Asks the kage at `launcher`, the one sessions run, which providers it
    /// knows, and keeps the answer for every launch.
    async fn ask_kage(&self, launcher: &str) -> Result<KageProviders, SandboxUnavailableError> {
        let run = self
            .options
            .kage
            .clone()
            .unwrap_or_else(|| Arc::new(run_kage));
        let mut args = vec![launcher.to_owned()];
        args.extend(LIST_ARGS.iter().map(|arg| (*arg).to_owned()));
        let result = run(args, None)
            .await
            .unwrap_or_else(|error| super::RunResult {
                code: -1,
                stdout: String::new(),
                stderr: error.to_string(),
            });
        let listed =
            KageProviders::from_run(&result).map_err(|reason| SandboxUnavailableError {
                backend: SandboxBackend::Bailey,
                reasons: vec![reason],
            })?;
        self.kage_providers.keep(listed.clone());
        Ok(listed)
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
                "the kage agent is not on PATH, so there is nothing for a confined session to run"
                    .to_owned(),
            ));
        };

        // Started first and dropped on any failure below, which closes it, so a
        // launch that does not finish leaves no listener honouring its nonces.
        let broker = self.start_broker(launch).await?;

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
            &brokered_providers(EGRESS_MAP_ADDRESS, broker.as_ref()),
            &self.options.built_in,
            &launch.credential_names,
            &self.log,
            &self.kage_providers.get()?,
        )
        .await?;
        let launch = SandboxLaunch {
            env: session_env(&launch.env, broker.as_ref()),
            ..launch.clone()
        };
        let resolv = self.write_resolv_conf().await;
        let policy = policy_path(&launch);
        // Written beneath the state directory rather than by name. The
        // directory is granted to the session, so a link planted at the file
        // name would otherwise have this write the policy through to whatever
        // it points at, as the daemon, before any confinement exists.
        paths::write_beneath(
            &launch.state_dir,
            POLICY_FILENAME,
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
                env: self.egress_env(broker.as_ref()).as_ref(),
                path_extra: self.config.path_extra.as_deref(),
            })
            .as_bytes(),
        )
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
        let session_env_source = std::env::vars().collect();
        let session_env = session_environment(
            &launch.env,
            &session_env_source,
            &format!("{}/home", launch.state_dir),
        );
        let args = bailey_args(
            &self.config,
            &launch,
            &policy,
            broker.as_ref().map(|broker| broker.port),
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
            broker: std::sync::Mutex::new(broker.map(|broker| broker.broker)),
            stopped: std::sync::atomic::AtomicBool::new(false),
        })))
    }

    /// Starts this session's own egress broker, when egress is brokered.
    async fn start_broker(
        &self,
        launch: &SandboxLaunch,
    ) -> Result<Option<SessionBroker>, SandboxLaunchError> {
        start_session_broker(
            self.options.broker.as_ref(),
            &launch.session_id,
            launch.grants.clone(),
            EGRESS_MAP_ADDRESS,
            &self.log,
        )
        .await
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
    /// Rewritten on every launch rather than once, so a change to the
    /// resolver does not keep handing out the file written first.
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
    /// session. The check is one vendor's warning phrase plus the exit code,
    /// not a proof: what it catches is the known shape of refusal, and a run
    /// that failed outright.
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
        if check.code != 0 {
            return Err(SandboxLaunchError(format!(
                "the confined check command failed at {policy}: {}",
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

#[cfg(test)]
mod live_tests;
