//! The narrow contract a sandbox backend implements.
//!
//! Sized to exactly what a backend does: start a confined agent and hand back
//! its pipes, say what it can enforce on this host, tear a session down, and
//! find sandboxes a previous run left behind. It is not a plugin system.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::config::schema::SandboxBackend;

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

/// Where the session's agent history lives, as the agent sees it.
pub const AGENT_SESSIONS: &str = "/state/sessions";

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
    /// Host directories of pi extensions, copied into the session so the
    /// sandboxed agent loads them.
    pub extensions: Vec<String>,
    /// Continue the conversation already stored in the state directory.
    pub resume: bool,
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

/// What the agent is started with inside any sandbox.
///
/// The provider is always passed. The agent picks its own default otherwise,
/// which has nothing to do with whichever credential the configuration
/// supplies, so omitting it yields a session that starts and then cannot reach
/// a model.
pub fn agent_command(launch: &AgentCommand) -> Vec<String> {
    let mut command = vec![
        "pi".to_owned(),
        "--mode".to_owned(),
        "rpc".to_owned(),
        "--session-dir".to_owned(),
        launch.session_dir.clone(),
    ];
    command.push("--provider".to_owned());
    command.push(launch.provider.clone());
    // Memory is appended to the system prompt from a file, which costs the
    // agent no tool call to read and no round trip to a daemon it cannot
    // reach.
    if let Some(system_prompt_path) = &launch.system_prompt_path {
        command.push("--append-system-prompt".to_owned());
        command.push(system_prompt_path.clone());
    }
    if launch.model.as_ref().is_some_and(|model| !model.is_empty()) {
        command.push("--model".to_owned());
        command.push(launch.model.clone().expect("checked above"));
    }
    // The agent keeps its history in the session directory, so continuing
    // there is what makes a resumed thread pick up the conversation rather
    // than start a new one that happens to share a directory.
    if launch.resume {
        command.push("--continue".to_owned());
    }
    command
}

/// What [`agent_command`] is built from.
#[derive(Debug, Clone, Default)]
pub struct AgentCommand {
    /// Where the agent keeps its own history.
    pub session_dir: String,
    /// The provider the agent is started with.
    pub provider: String,
    /// The model asked for, or none for the provider's default.
    pub model: Option<String>,
    /// A file appended to the system prompt, or none.
    pub system_prompt_path: Option<String>,
    /// Whether the stored conversation continues.
    pub resume: bool,
}

/// The path of a file inside the session state, as the agent sees it.
///
/// Written into the state directory by the daemon, so the agent reaches it
/// where the state is placed rather than where the host keeps it.
pub fn placed_prompt_path(system_prompt_path: Option<&String>) -> Option<String> {
    let path = system_prompt_path?;
    let filename = std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned());
    Some(format!("{STATE_PATH}/{filename}"))
}

#[cfg(test)]
mod tests;
