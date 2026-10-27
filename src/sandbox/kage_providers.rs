//! The provider ids the agent knows, as the installed kage reports them.
//!
//! Each provider errand writes into kage's configuration is either an override
//! of one kage knows, which keeps its catalog models, or a custom provider. kage
//! refuses the whole file over a wrong pick, and which is right changes between
//! kage builds that report the same version. So errand asks the kage its
//! sessions run, once, when the backend is probed, rather than keeping a copy.

use std::sync::RwLock;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::sandbox::RunResult;
use crate::sandbox::backend::SandboxLaunchError;

/// The wire protocols of kage providers that have no media endpoints for
/// the image and speech tools to call.
const NO_MEDIA_APIS: [&str; 2] = ["anthropic", "acp"];

/// The arguments that ask kage for its provider list.
pub const LIST_ARGS: [&str; 2] = ["providers", "--json"];

/// One provider kage knows by id.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct KageProvider {
    /// The id, the `provider` in `provider/model`.
    pub id: String,
    /// The wire protocol it speaks, in kage's words: `openai`, `anthropic`,
    /// `gemini`, `openai-responses` or `acp`.
    pub api: String,
    /// Whether kage takes `[providers.<id>]` as an override of it.
    #[serde(rename = "override")]
    pub overridable: bool,
    /// Whether kage takes `[providers.custom.<id>]` under its id.
    pub custom: bool,
}

/// Every provider the installed kage knows by id. kage takes an id it does not
/// list only as a custom provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct KageProviders {
    providers: Vec<KageProvider>,
}

impl KageProviders {
    /// Reads what one run of `kage providers --json` answered with.
    ///
    /// A kage without the command, or an answer that does not read, is a
    /// reason to refuse to start: guessing at the list is how a provider kage
    /// had dropped came to stop every session.
    pub fn from_run(result: &RunResult) -> Result<Self, String> {
        if result.code != 0 {
            return Err(format!(
                "the installed kage could not list its providers: `kage {}` exited {} ({}); \
                 update kage to a build that has the command",
                LIST_ARGS.join(" "),
                result.code,
                result
                    .stderr
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or("it said nothing")
            ));
        }
        let listed: Self = serde_json::from_str(result.stdout.trim()).map_err(|error| {
            format!(
                "the installed kage answered `kage {}` with something errand cannot read: {error}",
                LIST_ARGS.join(" ")
            )
        })?;
        if listed.providers.is_empty() {
            return Err(format!(
                "the installed kage answered `kage {}` with no providers at all",
                LIST_ARGS.join(" ")
            ));
        }
        Ok(listed)
    }

    /// The provider kage lists under `id`, if it lists one.
    pub fn get(&self, id: &str) -> Option<&KageProvider> {
        self.providers.iter().find(|provider| provider.id == id)
    }

    /// Whether errand writes `id` as an override of kage's own provider.
    pub fn overrides(&self, id: &str) -> bool {
        self.get(id).is_some_and(|provider| provider.overridable)
    }

    /// Whether kage takes `id` under neither table, so no provider errand
    /// writes may be called it.
    pub fn reserves(&self, id: &str) -> bool {
        self.get(id)
            .is_some_and(|provider| !provider.overridable && !provider.custom)
    }

    /// What in the operator's provider definitions this kage cannot take, one
    /// problem each, in the words validation uses.
    ///
    /// Checked when the daemon starts and on every reload, since only a probe
    /// knows the list. A provider a plugin registers is never written for kage,
    /// so it is not held to the list.
    pub fn refuses(&self, defined: &Map<String, Value>) -> Vec<String> {
        let mut problems = Vec::new();
        for (name, definition) in defined {
            if definition.get("extension").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            if self.reserves(name) {
                problems.push(format!(
                    "agent.providers.{name} is named for a provider kage reserves, which takes \
                     neither an override nor a custom provider; rename it"
                ));
                continue;
            }
            let api = self.get(name).map(|provider| provider.api.as_str());
            if let Some(api) = api.filter(|api| NO_MEDIA_APIS.contains(api)) {
                for (entry, tool) in [("images", "image"), ("speech", "speech")] {
                    if definition.get(entry).is_some() {
                        problems.push(format!(
                            "agent.providers.{name}.{entry} cannot use kage's {name} provider, \
                             which speaks the {api} API: the {tool} tool reaches only \
                             OpenAI-compatible providers"
                        ));
                    }
                }
            }
        }
        problems
    }
}

/// The list a backend's last probe read, which every launch writes the agent's
/// configuration by.
#[derive(Debug, Default)]
pub struct Probed(RwLock<Option<KageProviders>>);

impl Probed {
    /// Keeps what a probe read, in place of what an earlier one did.
    pub fn keep(&self, listed: KageProviders) {
        *self.0.write().expect("the provider list lock") = Some(listed);
    }

    /// The list the last probe read. A backend that was never probed has
    /// none, and its launch is refused rather than written by a guess.
    pub fn get(&self) -> Result<KageProviders, SandboxLaunchError> {
        self.0
            .read()
            .expect("the provider list lock")
            .clone()
            .ok_or_else(|| {
                SandboxLaunchError(
                    "the sandbox backend was not probed, so which providers kage knows is unknown"
                        .to_owned(),
                )
            })
    }
}

/// What `kage providers --json` printed on the build this was written against,
/// for tests that stand in for a kage.
#[cfg(test)]
pub(crate) const INSTALLED_JSON: &str = include_str!("kage_providers/installed.json");

/// [`INSTALLED_JSON`] read, for tests that need a kage's list without a kage.
#[cfg(test)]
pub(crate) fn as_installed() -> KageProviders {
    KageProviders::from_run(&RunResult {
        code: 0,
        stdout: INSTALLED_JSON.to_owned(),
        stderr: String::new(),
    })
    .expect("the captured list reads")
}

#[cfg(test)]
mod tests;
