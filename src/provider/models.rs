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

/// Everything the store lists for one provider, or nothing at all.
pub fn read_models(directory: Option<&str>, provider: &str) -> Vec<ModelInfo> {
    let Some(directory) = directory else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(std::path::Path::new(directory).join(STORE_FILENAME))
    else {
        return Vec::new();
    };
    let parsed: Value = match serde_json::from_str(&text) {
        Ok(parsed) => parsed,
        Err(_) => return Vec::new(),
    };

    let models = parsed
        .get(provider)
        .and_then(|entry| entry.get("models"))
        .and_then(Value::as_array);
    let Some(models) = models else {
        return Vec::new();
    };

    let mut found = Vec::new();
    for raw in models {
        let Some(model) = raw.as_object() else {
            // A null where a model was expected crashes the TypeScript reader,
            // which is its behaviour against a store written wrong; this
            // reproduces it rather than quietly reading nothing.
            assert!(
                !raw.is_null(),
                "the model store holds a null entry where a model was expected"
            );
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
    found
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
