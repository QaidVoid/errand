//! Incus backend: one unprivileged system container per session.
//!
//! The instance is created from an image alias or from a stopped golden
//! instance, configured before it starts, and driven through the `incus`
//! command line. The agent runs inside the guest as `guestUid`, through a
//! wrapper that reads its environment from a tmpfs file and, when egress is
//! brokered, relays the session's proxy port to the broker's Unix socket.
//!
//! The guest shares the host kernel. That is the boundary: a kernel bug
//! reachable from the guest is a host problem, which the startup report says
//! rather than leaving implicit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::config::schema::{EgressMode, IncusConfig, NetworkMode, SandboxBackend, SandboxConfig};
use crate::config::size::parse_size;
use crate::log::Logger;
use crate::log::fields;
use crate::sandbox::Run;
use crate::sandbox::SandboxHandle;
use crate::sandbox::agent_config::write_agent_config;
use crate::sandbox::backend::{
    AGENT_HOME, AgentCommand, CapabilityReport, LaunchSettings, LiveSettings, SandboxLaunch,
    SandboxLaunchError, SandboxUnavailableError, WORKSPACE_PATH, agent_command, agent_memory_path,
    disk_tmp_dir, sandbox_name,
};
use crate::sandbox::broker::{BrokerListen, BrokerTemplate, SessionBroker};
use crate::sandbox::egress::{
    add_proxy_env, brokered_providers, session_env, start_session_broker,
};
use crate::sandbox::kage_providers::{KageProviders, Probed};
use crate::sandbox::spawn::spawn_agent;

/// The property an image and a golden carry, so the backend knows what the
/// guest provides before it starts.
pub const CONTRACT_PROPERTY: &str = "user.errand.contract";

/// The contract this backend understands.
pub const CONTRACT_VERSION: u32 = 1;

/// Where the guest mounts the broker's directory.
pub const BROKER_MOUNT: &str = "/mnt/errand-broker";

/// The socket inside the broker directory.
pub const BROKER_SOCKET: &str = "broker.sock";

/// Where the environment file waits for the wrapper, in the guest's tmpfs.
///
/// The directory is the agent's own, so the wrapper can remove the file and
/// the directory after reading it. A file in `/run` itself could not be
/// removed by the agent uid, since removal needs write permission on the
/// directory.
pub const ENV_FILE: &str = "/run/errand/env";

/// The loopback port the wrapper relays the broker on inside the guest.
pub const RELAY_PORT: u16 = 3128;

/// The wrapper the image provides to read the environment and exec the agent.
pub const EXEC_WRAPPER: &str = "/usr/local/bin/errand-exec";

/// The instance name prefix plus the session id is all Incus accepts.
const INSTANCE_NAME_MAX: usize = 63;

/// The guest uid is the uid and gid the agent runs as.
#[derive(Debug, Clone, Copy)]
struct Owner {
    uid: u32,
    gid: u32,
}

/// The host facts the probe reads, injected so a test needs no `/etc`.
#[derive(Clone, Default)]
pub struct HostFacts {
    /// `/etc/subuid`, empty when it could not be read.
    pub subuid: String,
    /// `/etc/subgid`, empty when it could not be read.
    pub subgid: String,
    /// `/sys/fs/cgroup/cgroup.controllers`, or none when it could not be
    /// read.
    pub cgroup_controllers: Option<String>,
}

impl HostFacts {
    /// Reads what the probe needs from this host.
    pub fn read() -> Self {
        Self {
            subuid: std::fs::read_to_string("/etc/subuid").unwrap_or_default(),
            subgid: std::fs::read_to_string("/etc/subgid").unwrap_or_default(),
            cgroup_controllers: std::fs::read_to_string("/sys/fs/cgroup/cgroup.controllers").ok(),
        }
    }
}

/// One image or golden contract, as the image build writes it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageContract {
    version: u32,
    guest_uid: u32,
    docker_gid: u32,
    kage_providers: Value,
}

/// The commands a launch runs before the agent starts.
pub struct IncusCommandPlan {
    /// The `init` or `copy` command.
    pub create: Vec<String>,
    /// Configuration keys to set, in order.
    pub config: Vec<(String, String)>,
    /// The size of the root disk.
    pub root_size: String,
    /// Device additions, each already a complete command.
    pub devices: Vec<Vec<String>>,
    /// The command that starts the instance.
    pub start: Vec<String>,
}

/// Incus system containers, one per session.
pub struct IncusSandbox {
    settings: LiveSettings,
    log: Logger,
    project_root: String,
    state_dir: String,
    host: HostFacts,
    run: Run,
    kage_providers: Probed,
}

/// Runs the installed client. Injected for tests.
///
/// The environment is cleared to `PATH`, a home that holds no configuration,
/// and `C`, so a remote or an `INCUS_*` variable in the daemon's environment
/// cannot redirect a session.
pub fn run_incus(
    args: Vec<String>,
    cwd: Option<String>,
    stdin: Option<String>,
) -> super::RunFuture<super::RunResult> {
    Box::pin(async move {
        let mut command = tokio::process::Command::new("incus");
        command
            .args(&args)
            .stdin(if stdin.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env_clear()
            .env(
                "PATH",
                std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_owned()),
            )
            .env("HOME", "/nonexistent")
            .env("LC_ALL", "C");
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn()?;
        if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
            use tokio::io::AsyncWriteExt;
            pipe.write_all(bytes.as_bytes()).await?;
        }
        let output = child.wait_with_output().await?;
        Ok(super::RunResult {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    })
}

/// The daemon's own runner for the installed client.
pub fn run_incus_arc() -> Run {
    Arc::new(run_incus)
}

/// The instance name the Incus API accepts for a session.
///
/// Names are 1 to 63 ASCII letters, numbers and dashes, cannot start with a
/// digit or a dash, and cannot end with a dash. The `errand-` prefix handles
/// the start, so only the length and a trailing dash remain.
pub fn instance_name(session_id: &str) -> String {
    let raw = sandbox_name(session_id);
    let mut name: String = raw
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .collect();
    name.truncate(INSTANCE_NAME_MAX);
    while name.ends_with('-') {
        name.pop();
    }
    if name.is_empty() {
        name.push_str("errand");
    }
    name
}

/// Whether a name can be given to an Incus instance.
fn is_instance_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > INSTANCE_NAME_MAX {
        return false;
    }
    if !bytes[0].is_ascii_alphabetic() || bytes[bytes.len() - 1] == b'-' {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
}

/// A byte count in the unit form Incus accepts.
pub fn incus_size(bytes: u64) -> String {
    let kib = bytes.div_ceil(1024).max(1);
    if kib.is_multiple_of(1024 * 1024) {
        format!("{}GiB", kib / (1024 * 1024))
    } else if kib.is_multiple_of(1024) {
        format!("{}MiB", kib / 1024)
    } else {
        format!("{kib}KiB")
    }
}

/// The CPU cap in the time-quota form Incus enforces.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "configuration keeps cpus positive and finite"
)]
pub fn cpu_allowance(cpus: f64) -> String {
    format!("{}ms/100ms", (cpus * 100.0).round().max(1.0) as u64)
}

/// The stop timeout in whole seconds, rounded up and never zero.
pub fn stop_timeout(grace_ms: u64) -> u64 {
    grace_ms.div_ceil(1000).max(1)
}

/// The idmap that makes the workspace owner the agent's uid in the guest.
#[expect(clippy::similar_names, reason = "a uid and a gid are two ids")]
pub fn idmap(host_uid: u32, host_gid: u32, guest_uid: u32) -> String {
    format!("uid {host_uid} {guest_uid}\ngid {host_gid} {guest_uid}")
}

/// The directory holding a session's broker socket.
///
/// A sibling of the session state, not inside it: the state is mounted
/// writable at `/state`, and the agent must not be able to replace the socket
/// it reaches its broker through.
pub fn broker_dir(state_dir: &str) -> String {
    format!("{state_dir}.broker")
}

/// The tmpfs mounts a session gets, in the size syntax the kernel reads.
///
/// Mounted fresh rather than remounted: a remount of the image's own tmpfs
/// failed on the test host with `mount point not mounted or bad option`, and
/// a fresh mount is what the session wants anyway, an empty scratch area at
/// the configured size.
pub fn tmpfs_mounts(config: &SandboxConfig) -> String {
    use std::fmt::Write as _;

    let mut command = String::new();
    if config.disk_tmp {
        // diskTmp reuses a host directory across launches. Guest root owns
        // what the previous instance left there, so the host daemon cannot
        // unlink it. Clear it from inside the guest as root instead.
        command.push_str("find /tmp -mindepth 1 -ignore_readdir_race -delete && ");
    }
    command.push_str("mkdir -p /tmp /dev/shm");
    if !config.disk_tmp {
        let _ = write!(
            command,
            " && mount -t tmpfs -o size={},mode=1777,nosuid,nodev tmpfs /tmp",
            parse_size(&config.tmp_size).unwrap_or(1)
        );
    }
    let _ = write!(
        command,
        " && mount -t tmpfs -o size={},mode=1777,nosuid,nodev tmpfs /dev/shm",
        parse_size(&config.shm_size).unwrap_or(1)
    );
    command
}

/// The command that runs the wrapper as the agent, in the workspace.
///
/// The wrapper is named rather than the agent, so the environment file is
/// read and removed before the agent exists. No value from the environment
/// appears here.
pub fn exec_args(project: &str, name: &str, guest_uid: u32, agent: &[String]) -> Vec<String> {
    let mut args = base(project);
    args.extend([
        "exec".to_owned(),
        name.to_owned(),
        "--cwd".to_owned(),
        WORKSPACE_PATH.to_owned(),
        "--user".to_owned(),
        guest_uid.to_string(),
        "--group".to_owned(),
        guest_uid.to_string(),
        "-T".to_owned(),
        "--".to_owned(),
        EXEC_WRAPPER.to_owned(),
    ]);
    args.extend(agent.iter().cloned());
    args
}

/// The prefix every command carries: the local daemon and the project.
fn base(project: &str) -> Vec<String> {
    vec![
        "--force-local".to_owned(),
        "--project".to_owned(),
        project.to_owned(),
    ]
}

/// The first line of a tool's complaint, for a message a person reads.
fn first_line(text: &str) -> &str {
    text.trim().lines().next().unwrap_or("it said nothing")
}

/// Parses a tool answer, or says which answer did not read.
fn parse_json(result: &super::RunResult, what: &str) -> Result<Value, SandboxLaunchError> {
    serde_json::from_str(result.stdout.trim()).map_err(|error| {
        SandboxLaunchError(format!(
            "{what} did not answer with JSON errand can read: {error}"
        ))
    })
}

/// The uid and gid of a path, falling back to the daemon's when it does not
/// exist yet, because the daemon creates it with its own ownership.
fn owner_of(path: &str) -> Owner {
    use std::os::unix::fs::MetadataExt;

    match std::fs::metadata(path) {
        Ok(info) => Owner {
            uid: info.uid(),
            gid: info.gid(),
        },
        Err(_) => effective_ids(),
    }
}

/// The daemon's effective uid and gid, read from the process the kernel
/// reports rather than a new dependency.
fn effective_ids() -> Owner {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        status.lines().find_map(|line| {
            line.strip_prefix(name)
                .and_then(|rest| rest.split_whitespace().nth(1))
                .and_then(|value| value.parse().ok())
        })
    };
    Owner {
        uid: field("Uid:").unwrap_or(0),
        gid: field("Gid:").unwrap_or(0),
    }
}

/// Whether one of the project's allowed disk path prefixes covers `path`.
fn path_covered(allowed: &str, path: &str) -> bool {
    allowed
        .split(',')
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
        .any(|prefix| Path::new(path).starts_with(Path::new(prefix)))
}

/// Whether a `restricted.idmap.*` value covers one host id.
fn idmap_covers(ranges: &str, id: u32) -> bool {
    ranges
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .any(|entry| {
            if entry == "*" {
                return true;
            }
            match entry.split_once('-') {
                Some((start, end)) => {
                    match (start.trim().parse::<u32>(), end.trim().parse::<u32>()) {
                        (Ok(start), Ok(end)) => id >= start && id <= end,
                        _ => false,
                    }
                }
                None => entry.parse::<u32>().is_ok_and(|value| value == id),
            }
        })
}

/// Whether `root`'s subuid or subgid allocation covers one host id.
fn subid_covers(contents: &str, id: u32) -> bool {
    contents.lines().any(|line| {
        let mut parts = line.split(':');
        let Some(name) = parts.next() else {
            return false;
        };
        let (Some(start), Some(count)) = (parts.next(), parts.next()) else {
            return false;
        };
        let (Ok(start), Ok(count)) = (start.trim().parse::<u32>(), count.trim().parse::<u32>())
        else {
            return false;
        };
        name == "root" && id >= start && id < start.saturating_add(count)
    })
}

/// What the project must allow for the backend to run safely.
fn project_gaps(project: &Map<String, Value>, owner: Owner, roots: &[&str]) -> Vec<String> {
    let value = |key: &str| project.get(key).and_then(Value::as_str).unwrap_or_default();
    let mut gaps = Vec::new();

    if value("restricted") != "true" {
        gaps.push(
            "the Incus project is not restricted, so the backend's exceptions are not enforced"
                .to_owned(),
        );
    }
    if value("restricted.containers.nesting") != "allow" {
        gaps.push(
            "restricted.containers.nesting is not allow, so Docker and rootful podman cannot run in the guest"
                .to_owned(),
        );
    }
    if value("restricted.containers.lowlevel") != "allow" {
        gaps.push(
            "restricted.containers.lowlevel is not allow, so raw.idmap cannot give the workspace its host ownership"
                .to_owned(),
        );
    }
    if value("restricted.containers.privilege").is_empty()
        || value("restricted.containers.privilege") == "unprivileged"
    {
        // The default and the wanted value.
    } else {
        gaps.push(
            "restricted.containers.privilege is not unprivileged, so a privileged container is possible"
                .to_owned(),
        );
    }
    if value("restricted.devices.disk") == "allow" {
        let allowed = value("restricted.devices.disk.paths");
        for root in roots {
            if !path_covered(allowed, root) {
                gaps.push(format!(
                    "restricted.devices.disk.paths does not cover {root}, so its mount is refused"
                ));
            }
        }
    } else {
        gaps.push(
            "restricted.devices.disk is not allow, so the workspace, state and broker mounts are refused"
                .to_owned(),
        );
    }
    if value("restricted.devices.proxy") == "allow" {
        gaps.push(
            "restricted.devices.proxy is allow, so the project no longer forbids a proxy device"
                .to_owned(),
        );
    }
    if !idmap_covers(value("restricted.idmap.uid"), owner.uid) {
        gaps.push(format!(
            "restricted.idmap.uid does not cover host uid {}, which owns the project",
            owner.uid
        ));
    }
    if !idmap_covers(value("restricted.idmap.gid"), owner.gid) {
        gaps.push(format!(
            "restricted.idmap.gid does not cover host gid {}, which owns the project",
            owner.gid
        ));
    }
    gaps
}

/// What Incus needs from the host to map the workspace owner into a guest.
fn idmap_gaps(owner: Owner, host: &HostFacts) -> Vec<String> {
    let mut gaps = Vec::new();
    if !subid_covers(&host.subuid, owner.uid) {
        gaps.push(format!(
            "/etc/subuid has no allocation for root covering host uid {}; add root:{}:1",
            owner.uid, owner.uid
        ));
    }
    if !subid_covers(&host.subgid, owner.gid) {
        gaps.push(format!(
            "/etc/subgid has no allocation for root covering host gid {}; add root:{}:1",
            owner.gid, owner.gid
        ));
    }
    gaps
}

/// Whether the host exposes the cgroup controllers the limits need.
fn cgroup_gaps(host: &HostFacts) -> Vec<String> {
    let Some(controllers) = &host.cgroup_controllers else {
        return vec![
            "cgroup v2 is not readable at /sys/fs/cgroup, so limits cannot be applied".to_owned(),
        ];
    };
    ["memory", "cpu", "pids"]
        .into_iter()
        .filter(|controller| {
            !controllers
                .split_whitespace()
                .any(|present| present == *controller)
        })
        .map(|controller| format!("the {controller} cgroup controller is not available to Incus"))
        .collect()
}

impl IncusSandbox {
    /// A backend over the installed Incus client.
    #[expect(
        clippy::too_many_arguments,
        reason = "the backend is built once, with the host facts it reads"
    )]
    pub fn new(
        config: SandboxConfig,
        log: Logger,
        project_root: String,
        state_dir: String,
        host: HostFacts,
        run: Run,
        built_in: BTreeMap<String, Vec<Value>>,
        broker: Option<BrokerTemplate>,
    ) -> Self {
        Self {
            settings: LiveSettings::new(LaunchSettings {
                sandbox: config,
                broker,
                built_in,
            }),
            log,
            project_root,
            state_dir,
            host,
            run,
            kage_providers: Probed::default(),
        }
    }

    /// What each launch reads from the configuration, for a reload to
    /// replace.
    pub fn settings(&self) -> LiveSettings {
        self.settings.clone()
    }

    async fn call(&self, args: &[String]) -> super::RunResult {
        (self.run)(args.to_vec(), None, None)
            .await
            .unwrap_or_else(|error| super::RunResult {
                code: -1,
                stdout: String::new(),
                stderr: error.to_string(),
            })
    }

    async fn call_stdin(&self, args: &[String], stdin: String) -> super::RunResult {
        (self.run)(args.to_vec(), None, Some(stdin))
            .await
            .unwrap_or_else(|error| super::RunResult {
                code: -1,
                stdout: String::new(),
                stderr: error.to_string(),
            })
    }

    /// A raw API query. The client refuses `--project` on `query`, so the
    /// project rides the path as a query parameter.
    fn query_args(path: &str, project: Option<&str>) -> Vec<String> {
        let mut args = vec!["--force-local".to_owned(), "query".to_owned()];
        match project {
            Some(project) => {
                let separator = if path.contains('?') { '&' } else { '?' };
                args.push(format!("{path}{separator}project={project}"));
            }
            None => args.push(path.to_owned()),
        }
        args
    }

    async fn query(&self, path: &str, project: Option<&str>) -> super::RunResult {
        self.call(&Self::query_args(path, project)).await
    }

    /// The commands a launch runs before the agent starts.
    #[expect(
        clippy::too_many_lines,
        reason = "one command per setting a session applies"
    )]
    fn command_plan(
        config: &SandboxConfig,
        launch: &SandboxLaunch,
        owner: Owner,
    ) -> IncusCommandPlan {
        let incus = &config.incus;
        let name = instance_name(&launch.session_id);
        let project = incus.project.as_str();
        let create = if let Some(golden) = incus.golden.as_deref() {
            let mut command = base(project);
            command.extend(["copy".to_owned(), golden.to_owned(), name.clone()]);
            command
        } else {
            let mut command = base(project);
            command.extend(["init".to_owned(), incus.image.clone(), name.clone()]);
            command
        };

        let mut config_keys = vec![
            ("user.errand.system".to_owned(), "true".to_owned()),
            ("user.errand.session".to_owned(), launch.session_id.clone()),
            ("security.privileged".to_owned(), "false".to_owned()),
            ("security.nesting".to_owned(), "true".to_owned()),
            ("security.guestapi".to_owned(), "false".to_owned()),
            ("boot.autostart".to_owned(), "false".to_owned()),
            ("boot.autorestart".to_owned(), "false".to_owned()),
            (
                "limits.memory".to_owned(),
                incus_size(parse_size(&config.memory).unwrap_or(1)),
            ),
            ("limits.memory.swap".to_owned(), "false".to_owned()),
            ("limits.processes".to_owned(), config.pids.to_string()),
            (
                "limits.cpu.allowance".to_owned(),
                cpu_allowance(config.cpus),
            ),
            (
                "limits.kernel.fsize".to_owned(),
                parse_size(&config.file_max).unwrap_or(1).to_string(),
            ),
            (
                "raw.idmap".to_owned(),
                idmap(owner.uid, owner.gid, incus.guest_uid),
            ),
        ];
        config_keys.sort();
        config_keys.dedup_by(|left, right| left.0 == right.0);

        let add = |device: &str, kind: &str, settings: &[(&str, &str)]| {
            let mut command = base(project);
            command.extend([
                "config".to_owned(),
                "device".to_owned(),
                "add".to_owned(),
                name.clone(),
                device.to_owned(),
                kind.to_owned(),
            ]);
            for (key, value) in settings {
                command.push(format!("{key}={value}"));
            }
            command
        };

        let mut devices = Vec::new();
        if config.network == NetworkMode::None || config.egress.mode == EgressMode::Proxy {
            devices.push(add("eth0", "none", &[]));
        }
        devices.push(add(
            "workspace",
            "disk",
            &[
                ("source", launch.project_path.as_str()),
                ("path", WORKSPACE_PATH),
            ],
        ));
        devices.push(add(
            "state",
            "disk",
            &[("source", launch.state_dir.as_str()), ("path", "/state")],
        ));
        if config.egress.mode == EgressMode::Proxy && config.network != NetworkMode::None {
            let broker = broker_dir(&launch.state_dir);
            devices.push(add(
                "broker",
                "disk",
                &[
                    ("source", broker.as_str()),
                    ("path", BROKER_MOUNT),
                    ("readonly", "true"),
                ],
            ));
        }
        if config.disk_tmp {
            let tmp = disk_tmp_dir(&launch.state_dir);
            devices.push(add(
                "tmp",
                "disk",
                &[("source", tmp.as_str()), ("path", "/tmp")],
            ));
        }

        let mut start = base(project);
        start.extend(["start".to_owned(), name]);

        IncusCommandPlan {
            create,
            config: config_keys,
            root_size: incus_size(parse_size(&incus.root_size).unwrap_or(1)),
            devices,
            start,
        }
    }

    /// The contract the image or golden declares.
    #[expect(
        clippy::too_many_lines,
        reason = "two sources, each with its own validation"
    )]
    async fn contract(
        &self,
        incus: &IncusConfig,
    ) -> Result<(ImageContract, String), SandboxUnavailableError> {
        let project = incus.project.as_str();
        let unavailable = |reason: String| SandboxUnavailableError {
            backend: SandboxBackend::Incus,
            reasons: vec![reason],
        };

        if let Some(golden) = incus.golden.as_deref() {
            if !is_instance_name(golden) {
                return Err(unavailable(format!(
                    "sandbox.incus.golden {golden} is not a valid Incus instance name"
                )));
            }
            let result = self
                .query(&format!("/1.0/instances/{golden}"), Some(project))
                .await;
            if result.code != 0 {
                return Err(unavailable(format!(
                    "the golden instance {golden} could not be read: {}",
                    first_line(&result.stderr)
                )));
            }
            let instance: Value = serde_json::from_str(result.stdout.trim()).map_err(|error| {
                unavailable(format!(
                    "the golden instance {golden} did not answer with JSON: {error}"
                ))
            })?;
            if instance.get("type").and_then(Value::as_str) != Some("container") {
                return Err(unavailable(format!(
                    "the golden instance {golden} is not a container"
                )));
            }
            if instance.get("status").and_then(Value::as_str) != Some("Stopped") {
                return Err(unavailable(format!(
                    "the golden instance {golden} is not stopped"
                )));
            }
            let config = instance.get("config").and_then(Value::as_object);
            if config
                .and_then(|config| config.get("user.errand.system"))
                .and_then(Value::as_str)
                == Some("true")
            {
                return Err(unavailable(format!(
                    "the golden instance {golden} carries user.errand.system, so the sweep would delete it"
                )));
            }
            let Some(property) = config
                .and_then(|config| config.get(CONTRACT_PROPERTY))
                .and_then(Value::as_str)
            else {
                return Err(unavailable(format!(
                    "the golden instance {golden} does not carry {CONTRACT_PROPERTY}"
                )));
            };
            let contract = serde_json::from_str::<ImageContract>(property).map_err(|error| {
                unavailable(format!(
                    "the golden instance {golden} carries an unreadable {CONTRACT_PROPERTY}: {error}"
                ))
            })?;
            return Ok((contract, format!("golden {golden}")));
        }

        let mut args = base(project);
        args.extend([
            "image".to_owned(),
            "list".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
        ]);
        let result = self.call(&args).await;
        if result.code != 0 {
            return Err(unavailable(format!(
                "the image list could not be read: {}",
                first_line(&result.stderr)
            )));
        }
        let images: Vec<Value> = serde_json::from_str(result.stdout.trim()).map_err(|error| {
            unavailable(format!("the image list did not answer with JSON: {error}"))
        })?;
        let image = images.iter().find(|image| {
            image
                .get("aliases")
                .and_then(Value::as_array)
                .is_some_and(|aliases| {
                    aliases.iter().any(|alias| {
                        alias.get("name").and_then(Value::as_str) == Some(incus.image.as_str())
                    })
                })
        });
        let Some(image) = image else {
            return Err(unavailable(format!(
                "the image alias {} is not present in project {project}",
                incus.image
            )));
        };
        if image.get("type").and_then(Value::as_str) != Some("container") {
            return Err(unavailable(format!(
                "the image alias {} is not a container image",
                incus.image
            )));
        }
        let Some(property) = image
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|properties| properties.get(CONTRACT_PROPERTY))
            .and_then(Value::as_str)
        else {
            return Err(unavailable(format!(
                "the image alias {} does not carry {CONTRACT_PROPERTY}",
                incus.image
            )));
        };
        let contract = serde_json::from_str::<ImageContract>(property).map_err(|error| {
            unavailable(format!(
                "the image alias {} carries an unreadable {CONTRACT_PROPERTY}: {error}",
                incus.image
            ))
        })?;
        Ok((contract, format!("image {}", incus.image)))
    }

    /// The pool the default profile's root disk lives on, and its driver.
    async fn pool(&self, project: &str) -> Result<(String, String), String> {
        let result = self.query("/1.0/profiles/default", Some(project)).await;
        if result.code != 0 {
            return Err(format!(
                "the default profile could not be read: {}",
                first_line(&result.stderr)
            ));
        }
        let profile: Value = serde_json::from_str(result.stdout.trim())
            .map_err(|error| format!("the default profile did not answer with JSON: {error}"))?;
        let pool = profile
            .get("devices")
            .and_then(Value::as_object)
            .and_then(|devices| devices.get("root"))
            .and_then(Value::as_object)
            .and_then(|root| root.get("pool"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                "the default profile has no root disk, so no pool is named".to_owned()
            })?;
        let result = self
            .query(&format!("/1.0/storage-pools/{pool}"), Some(project))
            .await;
        if result.code != 0 {
            return Err(format!(
                "the storage pool {pool} could not be read: {}",
                first_line(&result.stderr)
            ));
        }
        let storage: Value = serde_json::from_str(result.stdout.trim())
            .map_err(|error| format!("the storage pool did not answer with JSON: {error}"))?;
        let driver = storage
            .get("driver")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("the storage pool {pool} does not name its driver"))?;
        Ok((pool.to_owned(), driver.to_owned()))
    }

    /// Checks that this backend can run here and reports what it can enforce.
    #[expect(
        clippy::too_many_lines,
        reason = "one check per prerequisite the backend needs"
    )]
    pub async fn probe(&self) -> Result<CapabilityReport, SandboxUnavailableError> {
        let settings = self.settings.get();
        let config = &settings.sandbox;
        let incus = &config.incus;
        let project = incus.project.as_str();
        let unavailable = |reason: String| SandboxUnavailableError {
            backend: SandboxBackend::Incus,
            reasons: vec![reason],
        };

        let info = self.query("/1.0", None).await;
        if info.code != 0 {
            return Err(unavailable(format!(
                "the incus client could not reach the daemon: {}",
                first_line(&info.stderr)
            )));
        }

        let project_result = self.query(&format!("/1.0/projects/{project}"), None).await;
        if project_result.code != 0 {
            return Err(unavailable(format!(
                "the project {project} could not be read: {}",
                first_line(&project_result.stderr)
            )));
        }
        let project_json: Value =
            serde_json::from_str(project_result.stdout.trim()).map_err(|error| {
                unavailable(format!("the project did not answer with JSON: {error}"))
            })?;
        let project_config = project_json
            .get("config")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        let (contract, source) = self.contract(incus).await?;
        if contract.version != CONTRACT_VERSION {
            return Err(unavailable(format!(
                "{source} declares contract version {}; this backend understands {CONTRACT_VERSION}",
                contract.version
            )));
        }
        let kage_providers = KageProviders::from_json(&contract.kage_providers.to_string())
            .map_err(|error| {
                unavailable(format!(
                    "{source} does not name the kage providers: {error}"
                ))
            })?;
        self.kage_providers.keep(kage_providers.clone());

        let owner = owner_of(&self.project_root);
        let mut gaps = Vec::new();
        if contract.guest_uid != incus.guest_uid {
            gaps.push(format!(
                "{source} runs the agent as uid {} but sandbox.incus.guestUid is {}",
                contract.guest_uid, incus.guest_uid
            ));
        }
        if contract.docker_gid != incus.guest_uid {
            gaps.push(format!(
                "{source} gives the docker socket gid {} but the agent's primary gid is {}, so docker access will fail",
                contract.docker_gid, incus.guest_uid
            ));
        }
        gaps.extend(project_gaps(
            &project_config,
            owner,
            &[&self.project_root, &self.state_dir],
        ));
        gaps.extend(idmap_gaps(owner, &self.host));
        let state_owner = owner_of(&self.state_dir);
        if state_owner.uid != owner.uid || state_owner.gid != owner.gid {
            gaps.push(format!(
                "the state directory is owned by {}:{} but the project directory by {}:{}, and one idmap cannot map both",
                state_owner.uid, state_owner.gid, owner.uid, owner.gid
            ));
        }
        gaps.extend(cgroup_gaps(&self.host));

        let mut pool_note = String::new();
        match self.pool(project).await {
            Ok((pool, driver)) => {
                if driver == "dir" {
                    gaps.push(format!(
                        "the storage pool {pool} uses dir, whose root quota this backend cannot confirm; use a quota-capable pool"
                    ));
                } else {
                    pool_note =
                        format!("the root disk quota is enforced by the {driver} pool {pool}");
                }
            }
            Err(reason) => gaps.push(reason),
        }

        if config.egress.mode == EgressMode::Proxy && settings.broker.is_none() {
            gaps.push(
                "egress.mode is proxy but no broker is wired for this backend: provider keys and the GitHub token cross whole into the guest"
                    .to_owned(),
            );
        }

        let egress = match (config.network, config.egress.mode) {
            (NetworkMode::None, _) => "sessions have no network".to_owned(),
            (_, EgressMode::Proxy) if settings.broker.is_some() => {
                "sessions reach the network only through their egress broker, and the guest has no NIC"
                    .to_owned()
            }
            (_, EgressMode::Proxy) => {
                "sessions have no brokered egress and the guest has no NIC".to_owned()
            }
            (_, EgressMode::Open) => {
                "sessions reach the network directly, and the host allowlist is not enforced"
                    .to_owned()
            }
        };

        let mut notes = vec![
            format!("sessions run in unprivileged Incus system containers from {source}"),
            "the guest shares the host kernel, so a kernel bug reachable from it is a host problem"
                .to_owned(),
            egress,
            format!(
                "limits per session: memory {}, cpus {}, pids {}",
                config.memory, config.cpus, config.pids
            ),
            format!(
                "no single file may exceed {}, set as an fsize limit on the guest",
                config.file_max
            ),
            format!(
                "a session's root filesystem is capped at {}",
                incus.root_size
            ),
            format!(
                "a session is stopped once it has written {}, which is measured rather than enforced",
                config.disk
            ),
            format!(
                "the guest's /tmp and /dev/shm are sized from the configuration{}",
                if config.disk_tmp {
                    ", with /tmp on disk"
                } else {
                    ""
                }
            ),
        ];
        if !pool_note.is_empty() {
            notes.push(pool_note);
        }

        Ok(CapabilityReport {
            backend: SandboxBackend::Incus,
            gaps,
            notes,
            kage_providers,
        })
    }

    /// Starts one session's system container.
    pub async fn launch(
        &self,
        launch: &SandboxLaunch,
    ) -> Result<SandboxHandle, SandboxLaunchError> {
        let settings = self.settings.get();
        let config = &settings.sandbox;
        let incus = &config.incus;
        let project = incus.project.clone();
        let name = instance_name(&launch.session_id);
        let proxy = config.egress.mode == EgressMode::Proxy;
        let socket = PathBuf::from(broker_dir(&launch.state_dir)).join(BROKER_SOCKET);
        let listen = if proxy {
            BrokerListen::Unix {
                path: socket,
                guest_port: RELAY_PORT,
            }
        } else {
            BrokerListen::Tcp
        };
        // Started first and dropped on any failure below, which closes it.
        let broker = start_session_broker(
            settings.broker.as_ref(),
            &launch.session_id,
            launch.grants.clone(),
            "127.0.0.1",
            listen,
            &self.log,
        )
        .await?;

        if config.disk_tmp {
            // The guest's root processes write into this directory, and the
            // host sees them under the outer idmap, not as the daemon's uid.
            // The daemon cannot remove those files. The guest clears /tmp as
            // root after start, before the agent runs.
            let tmp = disk_tmp_dir(&launch.state_dir);
            tokio::fs::create_dir_all(&tmp)
                .await
                .map_err(|error| SandboxLaunchError(format!("creating {tmp}: {error}")))?;
        }
        write_agent_config(
            launch,
            &brokered_providers("127.0.0.1", broker.as_ref()),
            &settings.built_in,
            &launch.credential_names,
            &self.log,
            &self.kage_providers.get()?,
        )
        .await?;

        let owner = owner_of(&launch.project_path);
        let plan = Self::command_plan(config, launch, owner);
        let created = self.call(&plan.create).await;
        if created.code != 0 {
            return Err(SandboxLaunchError(format!(
                "the instance {name} could not be created: {}",
                first_line(&created.stderr)
            )));
        }

        let guest_env = guest_env(config, launch, broker.as_ref());
        let result = self
            .provision(&project, &name, &plan, config, launch, &guest_env)
            .await;
        let spawned = match result {
            Ok(spawned) => spawned,
            Err(error) => {
                let mut delete = base(&project);
                delete.extend(["delete".to_owned(), "--force".to_owned(), name.clone()]);
                let _ = self.call(&delete).await;
                return Err(error);
            }
        };
        self.log.info(
            "system container started",
            &fields([
                ("session", launch.session_id.as_str().into()),
                ("name", name.as_str().into()),
            ]),
        );

        Ok(SandboxHandle::Incus(Box::new(super::IncusStop {
            session_id: launch.session_id.clone(),
            name,
            project,
            project_path: launch.project_path.clone(),
            broker_dir: proxy.then(|| broker_dir(&launch.state_dir)),
            spawned: Arc::new(spawned),
            run: Arc::clone(&self.run),
            grace_ms: config.grace_period_ms,
            log: self.log.clone(),
            broker: std::sync::Mutex::new(broker.map(|broker| broker.broker)),
            stopped: std::sync::atomic::AtomicBool::new(false),
        })))
    }

    /// Configures, starts and enters the instance, returning the agent's
    /// launcher.
    #[expect(
        clippy::too_many_lines,
        reason = "the launch steps are one sequence and read in order"
    )]
    async fn provision(
        &self,
        project: &str,
        name: &str,
        plan: &IncusCommandPlan,
        config: &SandboxConfig,
        launch: &SandboxLaunch,
        guest_env: &BTreeMap<String, String>,
    ) -> Result<super::spawn::SpawnedAgent, SandboxLaunchError> {
        let result = self
            .query(&format!("/1.0/instances/{name}"), Some(project))
            .await;
        if result.code != 0 {
            return Err(SandboxLaunchError(format!(
                "the instance {name} could not be read: {}",
                first_line(&result.stderr)
            )));
        }
        let instance = parse_json(&result, "the instance")?;
        let own_devices = instance
            .get("devices")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for device in ["eth0", "workspace", "state", "broker", "tmp"] {
            if own_devices.contains_key(device) {
                let mut remove = base(project);
                remove.extend([
                    "config".to_owned(),
                    "device".to_owned(),
                    "remove".to_owned(),
                    name.to_owned(),
                    device.to_owned(),
                ]);
                let result = self.call(&remove).await;
                if result.code != 0 {
                    return Err(SandboxLaunchError(format!(
                        "the inherited {device} device could not be removed: {}",
                        first_line(&result.stderr)
                    )));
                }
            }
        }
        if instance
            .get("config")
            .and_then(Value::as_object)
            .is_some_and(|config| config.contains_key("limits.cpu"))
        {
            let mut unset = base(project);
            unset.extend([
                "config".to_owned(),
                "unset".to_owned(),
                name.to_owned(),
                "limits.cpu".to_owned(),
            ]);
            let result = self.call(&unset).await;
            if result.code != 0 {
                return Err(SandboxLaunchError(format!(
                    "the inherited limits.cpu could not be unset: {}",
                    first_line(&result.stderr)
                )));
            }
        }

        for (key, value) in &plan.config {
            let mut command = base(project);
            command.extend([
                "config".to_owned(),
                "set".to_owned(),
                name.to_owned(),
                key.clone(),
                value.clone(),
            ]);
            let result = self.call(&command).await;
            if result.code != 0 {
                return Err(SandboxLaunchError(format!(
                    "{key} could not be set: {}",
                    first_line(&result.stderr)
                )));
            }
        }

        let mut root = base(project);
        root.extend([
            "config".to_owned(),
            "device".to_owned(),
            "override".to_owned(),
            name.to_owned(),
            "root".to_owned(),
            format!("size={}", plan.root_size),
        ]);
        let result = self.call(&root).await;
        if result.code != 0 {
            return Err(SandboxLaunchError(format!(
                "the root disk size could not be set: {}",
                first_line(&result.stderr)
            )));
        }

        for device in &plan.devices {
            let result = self.call(device).await;
            if result.code != 0 {
                return Err(SandboxLaunchError(format!(
                    "a device could not be added: {}",
                    first_line(&result.stderr)
                )));
            }
        }

        let result = self.call(&plan.start).await;
        if result.code != 0 {
            return Err(SandboxLaunchError(format!(
                "the instance {name} did not start: {}",
                first_line(&result.stderr)
            )));
        }

        let mut resize = base(project);
        resize.extend([
            "exec".to_owned(),
            name.to_owned(),
            "-T".to_owned(),
            "--".to_owned(),
            "sh".to_owned(),
            "-c".to_owned(),
            tmpfs_mounts(config),
        ]);
        let result = self.call(&resize).await;
        if result.code != 0 {
            return Err(SandboxLaunchError(format!(
                "the guest's temporary filesystems could not be sized: {}",
                first_line(&result.stderr)
            )));
        }

        let contents = env_file(guest_env)?;
        let mut push = base(project);
        push.extend([
            "exec".to_owned(),
            name.to_owned(),
            "-T".to_owned(),
            "--".to_owned(),
            "sh".to_owned(),
            "-c".to_owned(),
            format!(
                "install -d -o {uid} -g {uid} -m 0700 /run/errand && umask 077 && cat > {ENV_FILE} && chown {uid}:{uid} {ENV_FILE}",
                uid = config.incus.guest_uid
            ),
        ]);
        let result = self.call_stdin(&push, contents).await;
        if result.code != 0 {
            return Err(SandboxLaunchError(format!(
                "the session environment could not be delivered: {}",
                first_line(&result.stderr)
            )));
        }

        let agent = agent_command(&AgentCommand {
            provider: launch.provider.clone(),
            model: launch.model.clone(),
            system_file: launch
                .system_prompt_path
                .as_ref()
                .map(|_| agent_memory_path()),
        });
        let exec = exec_args(project, name, config.incus.guest_uid, &agent);
        let client_env = BTreeMap::from([
            (
                "PATH".to_owned(),
                std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_owned()),
            ),
            ("HOME".to_owned(), "/nonexistent".to_owned()),
            ("LC_ALL".to_owned(), "C".to_owned()),
        ]);
        spawn_agent("incus", &exec, Some(&client_env), None)
            .map_err(|error| SandboxLaunchError(error.to_string()))
    }

    /// Names of system containers this system owns that no live session
    /// claims.
    pub async fn list_orphans(&self) -> Result<Vec<String>, SandboxLaunchError> {
        let settings = self.settings.get();
        let project = settings.sandbox.incus.project.as_str();
        let mut args = base(project);
        args.extend(["list".to_owned(), "--format".to_owned(), "json".to_owned()]);
        let result = self.call(&args).await;
        if result.code != 0 {
            return Err(SandboxLaunchError(format!(
                "could not list instances: {}",
                first_line(&result.stderr)
            )));
        }
        let instances: Vec<Value> =
            serde_json::from_str(result.stdout.trim()).map_err(|error| {
                SandboxLaunchError(format!(
                    "the instance list did not answer with JSON: {error}"
                ))
            })?;
        Ok(instances
            .iter()
            .filter(|instance| {
                instance
                    .get("config")
                    .and_then(Value::as_object)
                    .and_then(|config| config.get("user.errand.system"))
                    .and_then(Value::as_str)
                    == Some("true")
            })
            .filter_map(|instance| {
                instance
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect())
    }

    /// Removes the named sandboxes, returning how many were removed.
    pub async fn remove_orphans(&self, names: &[String]) -> usize {
        let settings = self.settings.get();
        let project = settings.sandbox.incus.project.as_str();
        let mut removed = 0;
        for name in names {
            let mut args = base(project);
            args.extend(["delete".to_owned(), "--force".to_owned(), name.clone()]);
            let result = self.call(&args).await;
            if result.code == 0 {
                removed += 1;
            } else {
                self.log.warn(
                    "could not remove a leftover system container",
                    &fields([
                        ("name", name.as_str().into()),
                        ("detail", first_line(&result.stderr).to_owned().into()),
                    ]),
                );
            }
        }
        removed
    }
}

/// The environment the guest's wrapper exports before it execs the agent.
fn guest_env(
    config: &SandboxConfig,
    launch: &SandboxLaunch,
    broker: Option<&SessionBroker>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([
        ("HOME".to_owned(), AGENT_HOME.to_owned()),
        (
            "PATH".to_owned(),
            "/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin".to_owned(),
        ),
        ("LANG".to_owned(), "C.UTF-8".to_owned()),
    ]);
    if let Some(extra) = &config.env {
        env.extend(extra.clone());
    }
    if let Some(broker) = broker
        && config.egress.mode == EgressMode::Proxy
        && config.network != NetworkMode::None
    {
        add_proxy_env(&mut env, "127.0.0.1", broker);
        env.insert(
            "ERRAND_BROKER_SOCKET".to_owned(),
            format!("{BROKER_MOUNT}/{BROKER_SOCKET}"),
        );
        env.insert("ERRAND_BROKER_PORT".to_owned(), RELAY_PORT.to_string());
    }
    env.extend(session_env(launch, broker));
    env
}

/// One `NAME=VALUE` line per variable, which the wrapper reads without a
/// shell ever evaluating a value.
fn env_file(env: &BTreeMap<String, String>) -> Result<String, SandboxLaunchError> {
    let mut contents = String::new();
    for (key, value) in env {
        if !is_variable_name(key) {
            return Err(SandboxLaunchError(format!(
                "{key} is not a name the guest can export as an environment variable"
            )));
        }
        if value.contains(['\n', '\0']) {
            return Err(SandboxLaunchError(format!(
                "the value of {key} contains a newline or a NUL, which the guest's environment file cannot carry"
            )));
        }
        contents.push_str(key);
        contents.push('=');
        contents.push_str(value);
        contents.push('\n');
    }
    Ok(contents)
}

/// Whether a string is a shell environment variable name.
fn is_variable_name(name: &str) -> bool {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod tests;
