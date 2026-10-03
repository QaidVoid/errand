//! The narrow contract a sandbox backend implements.
//!
//! Sized to exactly what a backend does: start a confined agent and hand back
//! its pipes, say what it can enforce on this host, tear a session down, and
//! find sandboxes a previous run left behind. It is not a plugin system.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::config::schema::{PluginConfig, SandboxBackend};

/// Where a session's project appears to the agent, under every backend.
///
/// The agent is never shown a host path. A path carries the operator's name
/// and the shape of their machine, which is not access the sandbox can take
/// back once the agent has read it, and it would then appear in anything the
/// agent writes or says.
pub const WORKSPACE_PATH: &str = "/workspace";

/// Where a session's own state directory appears to the agent.
pub const STATE_PATH: &str = "/state";

/// The agent's home, inside the session state it is allowed to write.
pub const AGENT_HOME: &str = "/state/home";

/// The one directory ahead of the system ones on a session's PATH.
///
/// Holds the wrappers a session runs in place of the real program, and nothing
/// else. It is inside the state the agent may write, so a wrapper there is a
/// habit to break rather than a boundary to enforce.
pub const AGENT_BIN: &str = "/state/home/bin";

/// Label marking every sandbox this system owns, for discovery and cleanup.
pub const SYSTEM_LABEL: &str = "errand.system";

/// Label carrying the session a sandbox belongs to.
pub const SESSION_LABEL: &str = "errand.session";

/// Prefix for the name given to a session's sandbox.
pub const SANDBOX_NAME_PREFIX: &str = "errand-";

/// Everything a backend needs to start one session.
#[derive(Debug, Clone, Default)]
pub struct SandboxLaunch {
    /// Stable identifier for this session.
    pub session_id: String,
    /// Absolute host path of the project the agent works in.
    pub project_path: String,
    /// Absolute host path of this session's own state directory.
    pub state_dir: String,
    /// Environment given to the agent, holding the provider credential.
    pub env: BTreeMap<String, String>,
    /// Absolute host path of a file appended to the system prompt.
    pub system_prompt_path: Option<String>,
    /// Provider id the agent is started with.
    pub provider: String,
    /// Model pattern, or none to use the provider's default.
    pub model: Option<String>,
    /// Providers the operator defined, written into the agent's configuration.
    ///
    /// Absent where the operator defined none, which is the ordinary case: the
    /// agent then knows only the providers it ships with.
    pub providers: Map<String, Value>,
    /// The variable each provider's key travels in, by provider, for those
    /// that name one. The agent's configuration points keyless providers at
    /// these variables.
    pub credential_names: BTreeMap<String, String>,
    /// Kage plugins handed to the session: copied into the agent's own
    /// plugin directory, with their capability grants written into the
    /// agent's configuration.
    pub plugins: Vec<PluginConfig>,
    /// Commands and tools the agent may not use, written into the agent's
    /// configuration as rules that refuse them.
    ///
    /// Every tool is allowed otherwise. The agent asks before running one,
    /// and a thread has no way to answer: a chat prompt cannot be a modal
    /// the agent waits on, and a permission ask nobody answers is a session
    /// that stalls until it times out. These are what the prompt was for.
    pub denied: Denials,
    /// Continue the conversation already stored in the state directory.
    pub resume: bool,
}

/// What the agent may not run, in the two shapes the agent refuses it in.
///
/// A command pattern is refused against the whole command line, which is
/// what `rm -rf *` needs. A tool is refused whatever its input carries,
/// which is the blunt end: refusing a capability rather than a call.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Denials {
    /// Glob patterns refused against a command line.
    pub commands: Vec<String>,
    /// Tools refused whatever they are asked to do.
    pub tools: Vec<String>,
}

/// What a backend can and cannot enforce on this host.
#[derive(Debug, Clone)]
pub struct CapabilityReport {
    /// Which backend the report is about.
    pub backend: SandboxBackend,
    /// Guarantees the backend cannot enforce here. Reported at startup, and
    /// fatal when the configuration requires full enforcement.
    pub gaps: Vec<String>,
    /// Facts worth stating that are not gaps.
    pub notes: Vec<String>,
}

/// Raised when a backend cannot run at all, so the daemon refuses to start.
#[derive(Debug, thiserror::Error)]
#[error("the {backend} sandbox backend is unavailable:\n{}", reasons.iter().map(|reason| format!("  - {reason}")).collect::<Vec<_>>().join("\n"))]
pub struct SandboxUnavailableError {
    /// The backend that cannot run.
    pub backend: SandboxBackend,
    /// Why it cannot.
    pub reasons: Vec<String>,
}

/// Raised when starting one session's sandbox fails.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SandboxLaunchError(pub String);

/// The sandbox name for a session, used for discovery and teardown.
pub fn sandbox_name(session_id: &str) -> String {
    format!("{SANDBOX_NAME_PREFIX}{session_id}")
}

/// The host directory bound at `/tmp` when `sandbox.diskTmp` is on.
pub fn disk_tmp_dir(state_dir: &str) -> String {
    format!("{state_dir}/tmp")
}

/// Empties the disk-backed `/tmp` of a session, making it when missing.
///
/// Called on every launch, so a session continued after its scratch filled
/// does not reopen the same full `/tmp`. `remove_dir_all` does not follow
/// symlinks, so one planted by the session removes the link, not its target.
pub async fn fresh_disk_tmp(state_dir: &str) -> Result<(), SandboxLaunchError> {
    let dir = disk_tmp_dir(state_dir);
    let failed = |error: std::io::Error| SandboxLaunchError(format!("emptying {dir}: {error}"));
    match tokio::fs::remove_dir_all(&dir).await {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(failed(error)),
        _ => {}
    }
    tokio::fs::create_dir_all(&dir).await.map_err(failed)
}

/// Where kage keeps everything it writes: sessions, configuration, caches.
///
/// One directory for all four XDG roots, so nothing it records escapes the
/// session state. The daemon writes the provider configuration beneath the
/// same directory on the host; kage adds its own `kage` segment under each
/// root, so the file lands at `{KAGE_HOME}/kage/config.toml` as the agent
/// sees it.
pub const KAGE_HOME: &str = "/state/kage";

/// The host-side directory holding what the agent reads as [`KAGE_HOME`]:
/// joined onto the session state directory.
pub const KAGE_DIR: &str = "kage";

/// The role text kage runs with when nothing overrides it, from kage-loop's
/// `DEFAULT_ROLE` at the pinned kage. Repeated here so memory can ride along:
/// `--system` replaces the role rather than adding to it, so sending memory
/// alone would drop the posture line the agent is built around. If kage
/// rewords it, the worst case is a stale sentence, not a broken launch.
pub const KAGE_ROLE: &str = "You are kage, a coding agent. Use the provided tools when they help and ask only when blocked.";

/// What the agent is started with inside any sandbox.
///
/// The provider is always passed. The agent picks its own default otherwise,
/// which has nothing to do with whichever credential the configuration
/// supplies, so omitting it yields a session that starts and then cannot reach
/// a model. History continues over ACP rather than on the command line, so
/// resuming takes no flag.
pub fn agent_command(launch: &AgentCommand) -> Vec<String> {
    let mut command = vec!["kage".to_owned(), "rpc".to_owned()];
    if let Some(model) = launch.model.as_ref().filter(|model| !model.is_empty()) {
        // Kage addresses `provider/model` and knows no `:level` suffix; the
        // level rides a session option after the switch instead.
        let (bare, _) = crate::session::model::split_level(model);
        command.push("-m".to_owned());
        command.push(format!("{}/{}", launch.provider, bare));
    }
    // Memory rides the system prompt, which costs the agent no tool call to
    // read and no round trip to a daemon it cannot reach.
    if let Some(system) = launch
        .system_text
        .as_ref()
        .filter(|text| !text.trim().is_empty())
    {
        command.push("--system".to_owned());
        command.push(format!("{KAGE_ROLE}\n\n{system}"));
    }
    command
}

/// What [`agent_command`] is built from.
#[derive(Debug, Clone, Default)]
pub struct AgentCommand {
    /// The provider the agent is started with.
    pub provider: String,
    /// The model asked for, with any `:level` still on it, or none for the
    /// provider's default.
    pub model: Option<String>,
    /// Memory for the system prompt, or none when there is nothing to say.
    pub system_text: Option<String>,
}

#[cfg(test)]
mod tests;
