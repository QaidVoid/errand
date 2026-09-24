//! The shape of the daemon's configuration, and its documented defaults.
//!
//! Every optional field on disk is a required field here, filled from
//! [`defaults`], so nothing downstream reasons about absence.
//!
//! This grows with the daemon. A field is added when something reads it, not
//! in anticipation of something that might.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Map;

/// Which sandbox confines a session's agent. There is no unsandboxed option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxBackend {
    /// Rootless podman containers.
    Podman,
    /// The Landlock and seccomp sandbox, with no container overhead.
    Bailey,
}

impl std::fmt::Display for SandboxBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxBackend::Podman => write!(f, "podman"),
            SandboxBackend::Bailey => write!(f, "bailey"),
        }
    }
}

/// How much network a session gets. `none` disables it entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    /// Outbound on the configured ports.
    Restricted,
    /// No network at all.
    None,
}

/// How outbound is bounded for a session that has a network.
///
/// `open` is the port-only rule the bailey backend has always applied: a
/// session may reach any host on an allowed port. `proxy` forces every
/// connection through a broker the daemon runs outside the sandbox, which
/// permits only an allowlist of hosts and injects the provider credential
/// itself, so the key never enters the sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EgressMode {
    /// Port-only outbound.
    Open,
    /// Everything through the daemon's broker.
    Proxy,
}

/// What a session may reach outbound, and how that is enforced.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressConfig {
    /// Whether egress is port-only or forced through the broker.
    pub mode: EgressMode,
    /// Hosts the broker permits, on top of the provider. A leading `*.`
    /// matches subdomains; a lone `*` admits any host.
    pub allow: Vec<String>,
    /// Whether the broker may dial an address on the host's own network.
    pub allow_internal: bool,
}

/// Chat connection and who may drive the bot.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatConfig {
    /// Bot token. Secret. Never enters a sandbox.
    pub token: String,
    /// The single channel the daemon serves. Everything else is ignored.
    pub channel_id: String,
    /// Accounts permitted to drive sessions. Must not be empty.
    pub allowed_user_ids: Vec<String>,
    /// Accounts refused before anything else is considered.
    pub blocked_user_ids: Vec<String>,
    /// Accounts that may control any session, not only their own.
    pub operator_user_ids: Vec<String>,
    /// Require a message to mention the bot before it starts a session.
    pub start_on_mention: bool,
}

/// A cheaper model the session's model may ask about one artefact.
///
/// Absent means no delegation at all: the session's own model does everything.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegateConfig {
    /// The model asked. Must be one the provider serves under the same key.
    pub model: String,
    /// How many delegations one turn may make before the rest stay at home.
    pub per_turn: u32,
    /// How long one delegation may take before it is abandoned.
    pub deadline_ms: u64,
    /// Where the provider is reached, when the model store does not say.
    pub base_url: Option<String>,
}

/// Which model the agent talks to, and the credential it reaches it with.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    /// Provider id a session starts on, such as `anthropic` or `zai-coding-cn`.
    ///
    /// Only where `model` does not name one of its own, which is the more
    /// specific thing to have written and settles it instead.
    pub provider: String,
    /// Model pattern or id. Omitted to use the provider's default.
    ///
    /// May name its provider as `provider/id`, may carry a thinking level as
    /// `id:max`, and may be a short name from `aliases`. A leading segment is
    /// only read as a provider when it is one this configuration defines, so
    /// a model id holding a slash of its own stays whole.
    pub model: Option<String>,
    /// Model an image is shown to when the working model cannot see one.
    pub vision_model: Option<String>,
    /// A cheaper model to ask about one artefact, or none.
    pub delegate: Option<DelegateConfig>,
    /// File of standing instructions given to every session, or none.
    pub rules_path: Option<String>,
    /// Every provider, the one a session starts on included.
    ///
    /// Each entry carries the credential errand reaches that provider with,
    /// optionally the `credentialName` the agent reads it from, and whatever
    /// else the agent's own configuration takes: `baseUrl`, `api`, `models`.
    /// Written into the agent's configuration verbatim apart from the
    /// credential, because that schema belongs to the agent. The credential
    /// is put on at the broker under `egress.mode = proxy`, and is otherwise
    /// written as the provider's `apiKey`. The exception to verbatim is
    /// an entry of `models` naming a model the host's store already defines
    /// for that provider: it is laid over that definition, so setting only
    /// `contextWindow` keeps the model's reasoning, thinking levels, and
    /// inputs.
    ///
    /// `discover` asks the provider which models it serves, at startup and on
    /// `!models refresh`, under its `baseUrl`. `true` reads the usual
    /// `/models` listing. An object moves it: `path`, `list` for where the
    /// array sits, `fields` naming where `id`, `name`, `contextWindow`, and
    /// `maxTokens` sit in an entry, and `defaults` for fields every model
    /// found carries. An entry of `models` naming a model found overrides the
    /// fields it names and keeps the rest.
    ///
    /// A `defaultThinkingLevel` on a provider says how hard its models think
    /// when nobody says, and one on an entry of `models` says it for that
    /// model alone. Naming a model the host's store already lists says it for
    /// that model without listing it twice.
    /// A provider entry may instead be marked `extension: true`, which means a
    /// pi extension registers it. Such a provider needs no credential, is not
    /// brokered, and is not written into the agent's configuration: the
    /// extension owns it. Its `models` are read only so a session can list and
    /// switch to them.
    pub providers: Map<String, serde_json::Value>,
    /// Host directories of pi extensions, copied into every session's agent
    /// directory so the sandboxed agent loads them.
    ///
    /// A session runs in a sandbox that cannot see the host's own pi
    /// configuration, so an extension installed there is invisible to it.
    /// Naming its directory here places a copy where the agent looks. An
    /// extension that registers a provider is paired with an `extension: true`
    /// entry under `providers`.
    pub extensions: Vec<String>,
    /// Short names for models, so a session is started without spelling one.
    pub aliases: BTreeMap<String, String>,
}

impl AgentConfig {
    /// Whether a provider is registered by an extension rather than described
    /// here, and so needs no credential and is left out of what errand writes.
    pub fn is_extension_provider(&self, provider: &str) -> bool {
        self.providers
            .get(provider)
            .and_then(|definition| definition.get("extension"))
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    }

    /// What one provider is reached with, or nothing when it is not defined.
    pub fn credential_of(&self, provider: &str) -> Option<&str> {
        self.providers
            .get(provider)?
            .get("credential")?
            .as_str()
            .filter(|credential| !credential.trim().is_empty())
    }

    /// The variable the agent reads a provider's key from, when one is named.
    ///
    /// A provider the agent already knows needs no variable: it is reached
    /// through the base URL and key in the configuration errand writes.
    pub fn credential_name_of(&self, provider: &str) -> Option<&str> {
        self.providers
            .get(provider)?
            .get("credentialName")?
            .as_str()
            .filter(|name| !name.trim().is_empty())
    }

    /// What the provider a session starts on is reached with.
    pub fn credential(&self) -> &str {
        self.credential_of(&self.provider).unwrap_or_default()
    }

    /// Every variable a provider's key is read from, across all of them.
    pub fn credential_names(&self) -> Vec<&str> {
        self.providers
            .keys()
            .filter_map(|provider| self.credential_name_of(provider))
            .collect()
    }
}

/// The GitHub identity a session works with, when one is configured.
///
/// A session with no GitHub configuration still runs, and simply has no
/// credential to reach a repository with.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubConfig {
    /// Token the agent authenticates with. Secret, and reachable by the agent.
    pub token: String,
    /// Name commits are authored with.
    pub user_name: String,
    /// Email commits are authored with.
    pub user_email: String,
}

/// Paths granted to a session on top of what the daemon already grants.
///
/// Additive only. Every path is absolute, since a relative one in a policy
/// means nothing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyExtraConfig {
    /// Directories or files a session may read.
    pub read: Vec<String>,
    /// Directories or files a session may write.
    pub write: Vec<String>,
    /// Directories a session may execute from.
    pub execute: Vec<String>,
}

/// What a session may consume, and what the backend enforces.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxConfig {
    /// Which backend confines sessions.
    pub backend: SandboxBackend,
    /// Refuse to start when the backend cannot enforce every configured
    /// guarantee on this host, rather than reporting the gap and continuing.
    pub require_full_enforcement: bool,
    /// Network exposure granted to a session.
    pub network: NetworkMode,
    /// Ports a session may open outbound, when the network is not `none`.
    pub egress_ports: Vec<u16>,
    /// What a session may reach outbound, and whether it is brokered.
    pub egress: EgressConfig,
    /// Container image the podman backend runs. Inert under bailey.
    pub image: String,
    /// Memory ceiling per session, in size syntax such as `4g`.
    pub memory: String,
    /// CPU ceiling per session, in cores.
    pub cpus: f64,
    /// Process count ceiling per session.
    pub pids: u32,
    /// Largest single file a session may write, in size syntax.
    pub file_max: String,
    /// Size of a session's private `/tmp`, in size syntax such as `512m`.
    ///
    /// Backed by memory, so it is charged against `memory` when used, not
    /// against the host disk. The backend's small default starves a build
    /// that unpacks or compiles under `/tmp`, which is most of them.
    pub tmp_size: String,
    /// Size of a session's private `/dev/shm`, in size syntax.
    pub shm_size: String,
    /// Put a session's `/tmp` on disk instead of the memory-backed tmpfs, so
    /// a build's scratch is bounded by `disk` rather than by `memory`.
    ///
    /// The backend binds a directory under `/state` at `/tmp`, which still
    /// masks the host's `/tmp` as the tmpfs did, so the whole of `/tmp` is on
    /// disk, not only what honours `TMPDIR`. Replaces the tmpfs, so `tmpSize`
    /// has no effect while this is on. The directory is emptied at every
    /// launch, so a continued session starts on a clean `/tmp`.
    pub disk_tmp: bool,
    /// How much a session may add to its project and state together.
    ///
    /// Measured rather than enforced, because no backend caps what a process
    /// tree writes in aggregate without root. Passing it ends the session.
    pub disk: String,
    /// How often a session's disk use is measured, in milliseconds.
    pub disk_check_ms: u64,
    /// How long a sandbox may take to stop before it is killed.
    pub grace_period_ms: u64,
    /// Hide the host's network identity from a session, when it has a network.
    pub hide_host_address: bool,
    /// Paths granted on top of the generated policy, or none.
    pub policy_extra: Option<PolicyExtraConfig>,
    /// Directories added to a session's PATH, or none.
    pub path_extra: Option<Vec<String>>,
    /// Variables set in every session's environment, or none.
    pub env: Option<BTreeMap<String, String>>,
}

/// Who may power off the host from a chat message.
///
/// Off unless the list has somebody in it. This is the one command that acts
/// on the machine rather than on a session, so it is not covered by any
/// session role.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShutdownConfig {
    /// Account ids permitted to power off the host.
    pub allowed_user_ids: Vec<String>,
}

/// How much of the agent's activity reaches the thread.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputConfig {
    /// Whether tool output bodies are posted, not just that a tool ran.
    pub forward_tool_output: bool,
    /// Longest tool output posted before it is truncated and marked as such.
    pub max_tool_output_chars: usize,
    /// Largest attached file taken into a session, in bytes.
    pub max_attachment_bytes: u64,
    /// Most attached files taken from one message. The rest are refused.
    pub max_attachments_per_message: usize,
    /// Post a diff after the agent changes a file.
    pub post_diffs: bool,
}

/// The local web interface.
///
/// Absent means no listener at all. There is no login: the address it binds
/// to is the access control, and that address is checked rather than trusted.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebConfig {
    /// Address to bind to. Must be loopback, private, or a tailnet address.
    pub host: String,
    /// Port to listen on.
    pub port: u16,
    /// When true the interface may watch and read but not start, prompt, or
    /// control anything.
    pub observer: bool,
    /// Where the interface is reachable from outside, such as behind a tunnel.
    pub public_url: Option<String>,
}

/// Bounds on how much work exists at once.
#[expect(
    clippy::struct_field_names,
    reason = "the names carry the configuration keys they stand for"
)]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitsConfig {
    /// Sessions that may have a model turn in flight simultaneously.
    pub max_concurrent_turns: u32,
    /// Sessions that may exist at all.
    pub max_live_sessions: u32,
    /// Prompts that may wait for a turn slot.
    pub max_queue_length: u32,
    /// How long a queued prompt may wait before it expires unsent.
    pub max_queue_wait_ms: u64,
}

/// Deadlines that end or unblock a session.
#[expect(
    clippy::struct_field_names,
    reason = "the names carry the configuration keys they stand for"
)]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeoutsConfig {
    /// No message and no agent activity for this long ends the session.
    pub idle_ms: u64,
    /// How long the agent has to become ready before it is abandoned.
    pub startup_ms: u64,
    /// How long a question posted to a thread waits for an answer.
    pub question_ms: u64,
    /// How long an abort waits for the agent before it is forced.
    pub abort_ms: u64,
}

/// The fully resolved configuration the daemon runs on.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Chat connection and who may drive the bot.
    pub chat: ChatConfig,
    /// Which model the agent talks to, and the credential it reaches it with.
    pub agent: AgentConfig,
    /// How a session reaches GitHub, or none when unconfigured.
    pub github: Option<GithubConfig>,
    /// Absolute path under which every session's project directory lives.
    pub project_root: String,
    /// Where per-session state directories are created on the host.
    ///
    /// Neither this nor `projectRoot` may sit inside the other. A session may
    /// write both, and the transcript is kept beside this directory precisely
    /// so that it cannot.
    pub state_dir: String,
    /// What a session may consume, and what the backend enforces.
    pub sandbox: SandboxConfig,
    /// How much of the agent's activity reaches the thread.
    pub output: OutputConfig,
    /// Who may power off the host. Empty means nobody, which is the default.
    pub shutdown: ShutdownConfig,
    /// The web interface, or none when one is not served.
    pub web: Option<WebConfig>,
    /// Bounds on how much work exists at once.
    pub limits: LimitsConfig,
    /// Deadlines that end or unblock a session.
    pub timeouts: TimeoutsConfig,
}

/// Allowlist entry that admits everyone who can post in the served channel.
///
/// Anyone admitted can run code in a sandbox with write access to the project
/// root, so this is only reasonable when the channel itself is the boundary.
pub const ALLOW_EVERY_USER: &str = "*";

/// Field paths whose values must never be logged, posted, or reported.
pub const SECRET_PATHS: [&str; 2] = ["chat.token", "github.token"];

/// Values used for any optional field the configuration file omits.
pub mod defaults {
    use super::{EgressMode, NetworkMode, SandboxBackend};

    /// Whether a top-level message must mention the bot to start a session.
    pub const START_ON_MENTION: bool = false;

    /// The sandbox backend sessions run under.
    pub const BACKEND: SandboxBackend = SandboxBackend::Bailey;
    /// Whether a gap in enforcement is a refusal to start.
    pub const REQUIRE_FULL_ENFORCEMENT: bool = true;
    /// The network exposure sessions get.
    pub const NETWORK: NetworkMode = NetworkMode::Restricted;
    /// The ports a session may open outbound.
    pub const EGRESS_PORTS: [u16; 1] = [443];
    /// How outbound is bounded.
    pub const EGRESS_MODE: EgressMode = EgressMode::Proxy;
    /// The broker's allowlist, when none is written.
    pub const EGRESS_ALLOW: [&str; 1] = ["*"];
    /// Whether the broker may dial the host's own network.
    pub const EGRESS_ALLOW_INTERNAL: bool = false;
    /// Whether a session sees the host's network identity.
    pub const HIDE_HOST_ADDRESS: bool = false;
    /// The image the podman backend runs.
    pub const IMAGE: &str = "localhost/errand-agent:latest";
    /// The memory ceiling per session.
    pub const MEMORY: &str = "4g";
    /// The CPU ceiling per session.
    pub const CPUS: f64 = 2.0;
    /// The process count ceiling per session.
    pub const PIDS: u32 = 512;
    /// The largest single file a session may write.
    pub const FILE_MAX: &str = "1g";
    /// Default private `/tmp` size. Larger than the backend's own default, which
    /// is too small for a build that unpacks or compiles under it.
    pub const TMP_SIZE: &str = "512m";
    /// Default private `/dev/shm` size.
    pub const SHM_SIZE: &str = "256m";
    /// Default for backing TMPDIR with disk. Off, to leave the tmpfs default.
    pub const DISK_TMP: bool = false;
    /// How much a session may add to its project and state together.
    pub const DISK: &str = "5g";
    /// How often a session's disk use is measured.
    pub const DISK_CHECK_MS: u64 = 30_000;
    /// How long a sandbox may take to stop before it is killed.
    pub const GRACE_PERIOD_MS: u64 = 10_000;

    /// Whether tool output bodies are posted.
    pub const FORWARD_TOOL_OUTPUT: bool = false;
    /// Longest tool output posted before it is truncated.
    pub const MAX_TOOL_OUTPUT_CHARS: usize = 1_500;
    /// Largest attached file taken into a session.
    pub const MAX_ATTACHMENT_BYTES: u64 = 5 * 1024 * 1024;
    /// Most attached files taken from one message.
    pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = 4;
    /// Whether a diff is posted after the agent changes a file.
    pub const POST_DIFFS: bool = true;

    /// Sessions that may have a model turn in flight simultaneously.
    pub const MAX_CONCURRENT_TURNS: u32 = 2;
    /// Sessions that may exist at all.
    pub const MAX_LIVE_SESSIONS: u32 = 4;
    /// Prompts that may wait for a turn slot.
    pub const MAX_QUEUE_LENGTH: u32 = 32;
    /// How long a queued prompt may wait before it expires unsent.
    pub const MAX_QUEUE_WAIT_MS: u64 = 900_000;

    /// How many delegations one turn may make.
    pub const DELEGATE_PER_TURN: u32 = 8;
    /// How long one delegation may take before it is abandoned.
    pub const DELEGATE_DEADLINE_MS: u64 = 60_000;

    /// The address the interface binds to.
    pub const WEB_HOST: &str = "127.0.0.1";
    /// The port the interface listens on.
    pub const WEB_PORT: u16 = 8787;
    /// Whether the interface may only watch.
    pub const WEB_OBSERVER: bool = false;

    /// How long a session may sit idle before it ends.
    pub const IDLE_MS: u64 = 1_800_000;
    /// How long the agent has to become ready.
    pub const STARTUP_MS: u64 = 60_000;
    /// How long a question waits for an answer.
    pub const QUESTION_MS: u64 = 300_000;
    /// How long an abort waits for the agent before it is forced.
    pub const ABORT_MS: u64 = 15_000;
}

/// Every reason the configuration was rejected, not just the first.
///
/// Reporting one at a time makes a first run a guessing game, so the daemon
/// refuses with the whole list.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigError {
    /// Every problem found, in the order the checks ran.
    pub problems: Vec<String>,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "configuration rejected:")?;
        for problem in &self.problems {
            write!(f, "\n  - {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}
