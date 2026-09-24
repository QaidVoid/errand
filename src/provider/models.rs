//! What the host's agent already knows about a provider's models.
//!
//! Two facts are needed to send an image somewhere useful: whether a model
//! accepts one at all, and the endpoint it is reached at. The agent keeps both
//! in a store beside its own configuration, so they are read from there rather
//! than kept as a table here, which would be wrong within a release.
//!
//! Nothing here is configuration. A host with no agent installation yields
//! nothing, and everything that depends on this treats that as "do not route".

use serde_json::Value;

use crate::config::load::Environment;
use crate::config::schema::AgentConfig;

/// The model store, inside the agent's configuration directory.
pub const STORE_FILENAME: &str = "models-store.json";

/// One model, reduced to what routing an image needs.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    /// The model id, as `--model` names it.
    pub id: String,
    /// Where the provider is reached. Absent when the store does not say.
    pub base_url: Option<String>,
    /// Input kinds the model accepts, such as `text` and `image`.
    pub input: Vec<String>,
    /// Input price, used only to prefer the cheapest model that can see.
    pub cost_in: f64,
}

/// Candidate agent configuration directories, most specific first.
///
/// The documented default is `~/.pi/agent`, but installs vary and the
/// directory is overridable, so each is tried rather than assumed.
pub fn agent_directories(env: &Environment) -> Vec<String> {
    let override_dir = env
        .get("PI_CODING_AGENT_DIR")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    let home = env.get("HOME").map_or("", |value| value.trim());
    let config_root = env
        .get("XDG_CONFIG_HOME")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map_or_else(|| format!("{home}/.config"), str::to_owned);

    let mut directories = Vec::new();
    if let Some(override_dir) = override_dir {
        directories.push(override_dir.to_owned());
    }
    directories.push(format!("{home}/.pi/agent"));
    directories.push(format!("{config_root}/pi"));
    directories
}

/// The first candidate directory that actually holds a model store.
pub fn agent_directory(env: &Environment) -> Option<String> {
    for directory in agent_directories(env) {
        let path = std::path::Path::new(&directory).join(STORE_FILENAME);
        // Not this one, or not readable by whoever the daemon runs as.
        if std::fs::metadata(&path).is_ok() {
            return Some(directory);
        }
    }
    None
}

/// Whether a model can be shown an image.
pub fn sees_images(model: Option<&ModelInfo>) -> bool {
    model.is_some_and(|model| model.input.iter().any(|kind| kind == "image"))
}

/// A model this host can run, and the provider that serves it.
///
/// A model id is only unique within its provider, so the pair travels
/// together: a switch that carried the id alone would be sent to whichever
/// provider happened to be current, which is not where the model lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableModel {
    /// The provider that serves it.
    pub provider: String,
    /// The model id, as the agent should be given it.
    pub id: String,
    /// How hard it thinks when nobody says, with its colon already on.
    ///
    /// Read from the model's own `defaultThinkingLevel`, or the provider's
    /// when the model names none. A level on the name beats both, because
    /// somebody typing one is being more specific than the configuration.
    pub default_level: Option<String>,
}

impl AvailableModel {
    /// How somebody names it unambiguously, as `provider/id`.
    pub fn qualified(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }

    /// The id to give the agent, with a level on it when one applies.
    pub fn with_level(&self, asked: &str) -> String {
        if !asked.is_empty() {
            return format!("{}{asked}", self.id);
        }
        match &self.default_level {
            Some(level) => format!("{}{level}", self.id),
            None => self.id.clone(),
        }
    }
}

/// A level as it is written onto a model name, with its colon.
fn level_suffix(from: Option<&Value>) -> Option<String> {
    let named = from?.get("defaultThinkingLevel")?.as_str()?.trim();
    (!named.is_empty()).then(|| format!(":{}", named.to_lowercase()))
}

/// Every model a session can actually be switched to.
///
/// Not every model the host's store lists. A session runs in a sandbox whose
/// agent is given a credential for the configured provider and for each one
/// the operator defined, and for nothing else: its store holds the configured
/// provider alone, and the defined providers arrive as an override beside it.
/// A model on a provider the agent cannot authenticate to is not a model it
/// can run, and offering one only moves the failure into the turn.
///
/// So the providers the operator configured are the source of truth, and the
/// models are read where each provider's models are written: the host store
/// for the configured one, and the definition itself for the rest.
pub fn available_models(agent: &AgentConfig, directory: Option<&str>) -> Vec<AvailableModel> {
    let configured = level_suffix(agent.providers.get(&agent.provider));
    let mut found: Vec<AvailableModel> = read_models(directory, &agent.provider)
        .into_iter()
        .map(|model| AvailableModel {
            provider: agent.provider.clone(),
            id: model.id,
            default_level: configured.clone(),
        })
        .collect();

    for (provider, definition) in &agent.providers {
        let across = level_suffix(Some(definition));
        let listed = definition
            .get("models")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for model in listed {
            let Some(id) = model.get("id").and_then(Value::as_str) else {
                continue;
            };
            let level = level_suffix(Some(model)).or_else(|| across.clone());
            // The configured provider's models come from the store as well. A
            // definition naming one again is saying something about it, not
            // adding a second of it.
            if let Some(known) = found
                .iter_mut()
                .find(|known| known.provider == *provider && known.id == id)
            {
                known.default_level = level;
                continue;
            }
            found.push(AvailableModel {
                provider: provider.clone(),
                id: id.to_owned(),
                default_level: level,
            });
        }
    }
    found
}

/// The provider that serves a bare model id, when one clearly does.
///
/// A model named without a provider, `-m big-pickle`, belongs to whichever
/// provider lists it, not to the one a session would otherwise start on:
/// sending it to the default provider's endpoint is what this exists to stop.
/// The starting provider wins a tie, because a model two providers both serve
/// is most naturally the one already in hand; anything more ambiguous is left
/// for the caller to fall back on the default.
pub fn provider_for(available: &[AvailableModel], model_id: &str, prefer: &str) -> Option<String> {
    let serving: Vec<&str> = available
        .iter()
        .filter(|model| model.id == model_id)
        .map(|model| model.provider.as_str())
        .collect();
    if serving.contains(&prefer) {
        return Some(prefer.to_owned());
    }
    match serving.as_slice() {
        [only] => Some((*only).to_owned()),
        _ => None,
    }
}

/// What reading the store found, and what it could not read.
pub struct StoreContents {
    /// Every model the store lists for the provider.
    pub models: Vec<ModelInfo>,
    /// Entries that were not models, which an operator may want to know about.
    pub skipped: usize,
}

/// Everything the store lists for one provider, or nothing at all.
pub fn read_models(directory: Option<&str>, provider: &str) -> Vec<ModelInfo> {
    read_store(directory, provider).models
}

/// Everything the store lists, with a count of what it could not read.
///
/// An entry that is not an object is skipped rather than fatal. A store is
/// written by something other than errand, and a daemon that will not start
/// because one line of it is wrong is a worse answer than a daemon that
/// starts and says which line.
pub fn read_store(directory: Option<&str>, provider: &str) -> StoreContents {
    let nothing = || StoreContents {
        models: Vec::new(),
        skipped: 0,
    };
    let Some(directory) = directory else {
        return nothing();
    };
    let Ok(text) = std::fs::read_to_string(std::path::Path::new(directory).join(STORE_FILENAME))
    else {
        return nothing();
    };
    let parsed: Value = match serde_json::from_str(&text) {
        Ok(parsed) => parsed,
        Err(_) => return nothing(),
    };

    let models = parsed
        .get(provider)
        .and_then(|entry| entry.get("models"))
        .and_then(Value::as_array);
    let Some(models) = models else {
        return nothing();
    };

    let mut found = Vec::new();
    let mut skipped = 0;
    for raw in models {
        let Some(model) = raw.as_object() else {
            skipped += 1;
            continue;
        };
        let Some(Value::String(id)) = model.get("id") else {
            continue;
        };
        let cost = model.get("cost");
        found.push(ModelInfo {
            id: id.clone(),
            base_url: match model.get("baseUrl") {
                Some(Value::String(base_url)) => Some(base_url.clone()),
                _ => None,
            },
            input: match model.get("input") {
                Some(Value::Array(kinds)) => kinds
                    .iter()
                    .filter_map(|kind| kind.as_str())
                    .map(str::to_owned)
                    .collect(),
                _ => Vec::new(),
            },
            cost_in: cost
                .and_then(|cost| cost.get("input"))
                .and_then(Value::as_f64)
                .unwrap_or(f64::INFINITY),
        });
    }
    StoreContents {
        models: found,
        skipped,
    }
}

/// The base URL most of a provider's models are served from.
///
/// One provider may serve models from more than one URL, one per API it
/// speaks, while a brokered provider is reached through a single one.
pub fn common_base_url(models: &[ModelInfo]) -> Option<String> {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for url in models.iter().filter_map(|model| model.base_url.as_deref()) {
        *counts.entry(url).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(url, _)| url.to_owned())
}

/// One model by id, when the store lists it.
pub fn model_by_id<'a>(models: &'a [ModelInfo], id: Option<&str>) -> Option<&'a ModelInfo> {
    let id = id?;
    models.iter().find(|model| model.id == id)
}

/// The model an image is described by.
///
/// A named one when the configuration names one, so the choice can be made
/// deliberately. Otherwise the cheapest that can see: describing an image is a
/// paragraph of output, and the model doing the work is a different one.
///
/// Returns nothing when the provider has no such model, or when the store does
/// not say where to reach the one it has.
pub fn vision_model(models: &[ModelInfo], preferred: Option<&str>) -> Option<ModelInfo> {
    if let Some(preferred) = preferred {
        let named = model_by_id(models, Some(preferred));
        return match named {
            Some(named) if sees_images(Some(named)) && named.base_url.is_some() => {
                Some(named.clone())
            }
            _ => None,
        };
    }
    let mut candidates: Vec<&ModelInfo> = models
        .iter()
        .filter(|model| sees_images(Some(model)) && model.base_url.is_some())
        .collect();
    candidates.sort_by(|left, right| {
        left.cost_in
            .partial_cmp(&right.cost_in)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates.first().map(|model| (*model).clone())
}
