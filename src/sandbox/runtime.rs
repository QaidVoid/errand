//! Where the agent's own program lives on this host.
//!
//! A confined session runs the agent the operator installed, and the sandbox
//! hands the target its own PATH rather than the caller's. A policy granting
//! only the project therefore produces a session that starts and immediately
//! fails to exec, which is why these directories are resolved and granted
//! explicitly.
//!
//! Only read access is granted, and only to the directories the agent is
//! loaded from. The operator's home is not granted, and neither is any parent
//! of these.

use std::path::Path;
use std::sync::Arc;

/// The PATH a target is given when the policy sets none.
pub const SANDBOX_PATH: [&str; 3] = ["/usr/local/bin", "/usr/bin", "/bin"];

/// The directories a session needs in order to load and run the agent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentRuntime {
    /// Directories the policy grants read access to.
    pub read_paths: Vec<String>,
    /// Directories placed ahead of the sandbox PATH.
    pub path_entries: Vec<String>,
}

/// Looks a program up the way a shell would. Injected so tests need no host.
pub type Lookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Whether a path is a file this user can execute.
fn is_executable(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|info| info.is_file() && (info.permissions().mode() & 0o111) != 0)
}

/// Finds a program on PATH, which is what a shell would run for that name.
pub fn which(name: &str) -> Option<String> {
    which_on_path(name, &std::env::var("PATH").unwrap_or_default())
}

/// The same lookup against a PATH given by the caller.
pub fn which_on_path(name: &str, path: &str) -> Option<String> {
    for directory in path.split(':') {
        if directory.is_empty() {
            continue;
        }
        let candidate = Path::new(directory)
            .join(name)
            .to_string_lossy()
            .into_owned();
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Adds a path once, keeping the order entries were discovered in.
fn add(into: &mut Vec<String>, value: Option<String>) {
    if let Some(value) = value
        && !into.contains(&value)
    {
        into.push(value);
    }
}

fn real_path(path: &str) -> String {
    std::fs::canonicalize(path).map_or_else(
        |_| path.to_owned(),
        |resolved| resolved.to_string_lossy().into_owned(),
    )
}

/// Resolves what a confined session must read in order to run the agent.
///
/// The agent is one self-contained binary: its own directory is granted for
/// the exec and the PATH entry, and a link is followed to the directory that
/// holds the real file. Returns nothing when the agent is not installed,
/// which is a reason to refuse to start rather than something to work around.
pub fn agent_runtime(lookup: &Lookup) -> Option<AgentRuntime> {
    let launcher = lookup("kage")?;

    let mut runtime = AgentRuntime::default();

    add(&mut runtime.path_entries, Some(parent_of(&launcher)));
    add(&mut runtime.read_paths, Some(parent_of(&launcher)));

    // A launcher installed by a package manager is usually a link into the
    // package it belongs to, and the link's own directory holds none of the
    // code.
    let real = real_path(&launcher);
    if let Some(parent) = Path::new(&real).parent() {
        add(
            &mut runtime.read_paths,
            Some(parent.to_string_lossy().into_owned()),
        );
    }

    Some(runtime)
}

fn parent_of(path: &str) -> String {
    Path::new(path).parent().map_or_else(
        || path.to_owned(),
        |parent| parent.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests;
