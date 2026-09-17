//! Finding and reading the configuration file.
//!
//! Separate from validating it, so that "the file is not there" and "the file
//! says something impossible" are different failures with different messages.
//!
//! Where it is read from is a search rather than one path, because the same
//! program is run in three ways: from a checkout while it is being worked on,
//! as somebody's own daemon, and as a system service. Naming the file outright
//! always wins, so none of that has to be guessed at when it matters.

use std::collections::BTreeMap;
use std::path::Path;

use jsonc_parser::ParseOptions;
use serde_json::Value;

use crate::config::resolve;
use crate::config::schema::{Config, ConfigError};
use crate::config::validate::validate_config;

/// What the process sees of its environment, by variable name.
pub type Environment = BTreeMap<String, String>;

/// Environment variable naming the configuration file.
pub const CONFIG_VARIABLE: &str = "ERRAND_CONFIG";

/// The directory name used under a configuration root.
pub const CONFIG_DIRECTORY: &str = "errand";

/// The filename, wherever it is found.
pub const CONFIG_FILENAME: &str = "config.json";

/// The same file with the extension that says it may carry comments.
pub const CONFIG_FILENAME_JSONC: &str = "config.jsonc";

/// Both accepted basenames, plain JSON first so an existing setup is unchanged.
const CONFIG_BASENAMES: [&str; 2] = [CONFIG_FILENAME, CONFIG_FILENAME_JSONC];

/// Where a system service keeps it.
pub const SYSTEM_CONFIG_PATH: &str = "/etc/errand/config.json";

/// House rules looked for beside the configuration when none were named.
pub const RULES_FILENAME: &str = "AGENTS.md";

/// Every place the configuration is looked for, in order.
///
/// A person's own configuration comes before the system's, so running the
/// daemon by hand on a host that also serves one does not silently pick up the
/// service's token. The working directory is last: it is a convenience for a
/// checkout, not somewhere a daemon should be configured from by accident.
pub fn config_candidates(env: &Environment) -> Vec<String> {
    if let Some(named) = env
        .get(CONFIG_VARIABLE)
        .map(|value| value.trim())
        .filter(|named| !named.is_empty())
    {
        return vec![named.to_owned()];
    }

    let home = env.get("HOME").map_or("", |value| value.trim());
    let xdg = env
        .get("XDG_CONFIG_HOME")
        .map(|value| value.trim())
        .filter(|named| !named.is_empty());
    let root = match xdg {
        Some(xdg) => std::path::PathBuf::from(xdg),
        None => std::path::PathBuf::from(home).join(".config"),
    };

    let mut candidates = Vec::new();
    for dir in [
        root.join(CONFIG_DIRECTORY),
        std::path::PathBuf::from("/etc").join(CONFIG_DIRECTORY),
    ] {
        for name in CONFIG_BASENAMES {
            candidates.push(dir.join(name).to_string_lossy().into_owned());
        }
    }
    for name in CONFIG_BASENAMES {
        candidates.push(name.to_owned());
    }
    candidates
}

/// Where the configuration will be read from.
///
/// Returns the first candidate that exists, or the first candidate when none
/// do, so that a failure names the place somebody most likely meant.
pub fn config_path(env: &Environment, exists: impl Fn(&str) -> bool) -> String {
    let candidates = config_candidates(env);
    candidates
        .iter()
        .find(|candidate| exists(candidate))
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}

/// Reads and validates the configuration.
///
/// Returns a [`ConfigError`] with something actionable: where it looked, the
/// place the file could not be parsed, or every field that was wrong.
pub fn load_config(
    path: &str,
    read: impl Fn(&str) -> std::io::Result<String>,
    env: &Environment,
    exists: impl Fn(&str) -> bool,
) -> Result<Config, ConfigError> {
    let text = match read(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Every place that was tried, since "not at that path" is not much
            // help when the path was chosen by a search somebody did not run
            // themselves.
            let looked = config_candidates(env);
            return Err(ConfigError {
                problems: {
                    let mut problems = vec![format!("there is no configuration file at {path}")];
                    if looked.len() > 1 {
                        problems.push(format!("looked in: {}", looked.join(", ")));
                    }
                    problems.push(format!(
                        "write one there, or name it with {CONFIG_VARIABLE}"
                    ));
                    problems
                },
            });
        }
        Err(error) => {
            return Err(ConfigError {
                problems: vec![format!(
                    "the configuration file at {path} could not be read: {error}"
                )],
            });
        }
    };

    // Parsed as JSONC so a file may carry comments and trailing commas, which
    // is the difference between a config someone can annotate and one they
    // cannot. Plain JSON is a subset, so an existing file still reads.
    let parsed = jsonc_parser::parse_to_serde_value::<Value>(
        &text,
        &ParseOptions {
            allow_trailing_commas: true,
            ..ParseOptions::default()
        },
    );
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            return Err(ConfigError {
                problems: vec![format!(
                    "the configuration file at {path} is not valid JSON or JSONC: {error}"
                )],
            });
        }
    };

    let config = validate_config(&parsed)?;
    match config.agent.rules_path {
        Some(_) => Ok(config),
        None => Ok(with_rules_beside(config, path, &exists)),
    }
}

/// Takes house rules from beside the configuration file, when none were named.
///
/// Beside the configuration rather than at a fixed path, so the rules follow
/// whichever of the candidates was actually loaded: an operator with a file in
/// their home directory and another in `/etc` gets the one belonging to the
/// configuration in force, not whichever the search happened to reach first.
///
/// Only when the file is there. An absent one is not a refusal, because a
/// default nobody asked for must not be able to stop the daemon; naming a path
/// that is wrong still is, since that is somebody saying they want rules.
fn with_rules_beside(config: Config, path: &str, exists: &impl Fn(&str) -> bool) -> Config {
    let dir = Path::new(path).parent().unwrap_or(Path::new("/"));
    let candidate = resolve(&dir.join(RULES_FILENAME).to_string_lossy());
    if exists(&candidate) {
        let mut config = config;
        config.agent.rules_path = Some(candidate);
        return config;
    }
    config
}

/// Whether a path is a file, which is what a candidate must be to be used.
pub fn file_exists(path: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

#[cfg(test)]
mod tests;
