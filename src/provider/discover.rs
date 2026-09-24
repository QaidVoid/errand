//! Asks providers which models they serve, and holds what sessions read.
//!
//! A provider whose definition carries `discover` is asked at startup and on
//! `!models refresh`. What it lists goes underneath the models its definition
//! names: an entry written for a model overrides what was fetched for it field
//! by field, and keeps the rest. A provider that cannot be asked keeps the
//! models its definition names, and the reason is reported.

use std::sync::{Arc, RwLock};

use serde_json::{Map, Value};

use super::models::{AvailableModel, available_models};
use super::usage::{Fetch, HttpRequest};
use crate::config::discover::Discovery;
use crate::config::path::at;
use crate::config::schema::AgentConfig;

/// Asks one provider for its models.
pub async fn fetch_models(
    base_url: &str,
    credential: Option<&str>,
    discovery: &Discovery,
    fetch: &impl Fetch,
    timeout_ms: u64,
) -> Result<Vec<Value>, String> {
    let url = format!("{}{}", base_url.trim_end_matches('/'), discovery.path);
    let mut headers = vec![("Accept".to_owned(), "application/json".to_owned())];
    if let Some(credential) = credential {
        headers.push(("Authorization".to_owned(), format!("Bearer {credential}")));
    }
    let answer = fetch
        .fetch(
            url.clone(),
            HttpRequest {
                headers,
                timeout_ms,
            },
        )
        .await
        .map_err(|error| format!("{url} could not be reached: {error}"))?;
    if !answer.ok() {
        return Err(format!("{url} answered {}", answer.status));
    }
    let body = answer
        .body
        .ok_or_else(|| format!("{url} did not answer with JSON"))?;
    read_models(&body, discovery).map_err(|problem| format!("{url}: {problem}"))
}

/// Reads the model entries out of a listing, in the agent's own field names.
///
/// An entry with no id is skipped, since there is nothing to switch to. The
/// discovery's defaults go underneath what each entry says.
pub fn read_models(body: &Value, discovery: &Discovery) -> Result<Vec<Value>, String> {
    let Some(Value::Array(entries)) = at(body, &discovery.list) else {
        return Err(format!("no list at `{}`", discovery.list));
    };
    let mut found = Vec::new();
    for entry in entries {
        let mut model = discovery.defaults.clone();
        for (field, from) in &discovery.fields {
            if let Some(value) = at(entry, from).filter(|value| !value.is_null()) {
                model.insert(field.clone(), value.clone());
            }
        }
        if model.get("id").and_then(Value::as_str).is_some() {
            found.push(Value::Object(model));
        }
    }
    Ok(found)
}

/// A definition with the models found for it underneath its own.
///
/// An entry the definition writes for a found model is laid over it, so the
/// definition's fields win and the found ones fill the rest. Entries it writes
/// for models nobody found are kept as they are.
pub fn with_discovered(definition: &Value, found: &[Value]) -> Value {
    let Value::Object(fields) = definition else {
        return definition.clone();
    };
    let written: Vec<Value> = fields
        .get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let id_of = |model: &Value| model.get("id").and_then(Value::as_str).map(str::to_owned);

    let mut models: Vec<Value> = written
        .iter()
        .map(|entry| {
            let under = found.iter().find(|model| id_of(model) == id_of(entry));
            match (under, entry) {
                (Some(Value::Object(under)), Value::Object(over)) => {
                    let mut merged = under.clone();
                    merged.extend(over.clone());
                    Value::Object(merged)
                }
                _ => entry.clone(),
            }
        })
        .collect();
    models.extend(
        found
            .iter()
            .filter(|model| !written.iter().any(|entry| id_of(entry) == id_of(model)))
            .cloned(),
    );

    let mut fields = fields.clone();
    fields.insert("models".to_owned(), Value::Array(models));
    Value::Object(fields)
}

/// What asking one provider came to: how many models it listed, or why it
/// could not be asked.
pub type Outcome = (String, Result<usize, String>);

/// Asks every provider whose definition says to.
///
/// Returns the definitions with what was found folded in, the models a
/// session can switch to among them, and what each provider asked answered.
pub async fn discover(
    agent: &AgentConfig,
    store: Option<&str>,
    fetch: &impl Fetch,
    timeout_ms: u64,
) -> (Map<String, Value>, Vec<AvailableModel>, Vec<Outcome>) {
    let mut providers = agent.providers.clone();
    let mut outcomes = Vec::new();
    for (name, definition) in &agent.providers {
        let Ok(Some(discovery)) = Discovery::of(name, definition) else {
            continue;
        };
        let Some(base_url) = definition.get("baseUrl").and_then(Value::as_str) else {
            continue;
        };
        let fetched = fetch_models(
            base_url,
            agent.credential_of(name),
            &discovery,
            fetch,
            timeout_ms,
        )
        .await;
        if let Ok(found) = &fetched {
            providers.insert(name.clone(), with_discovered(definition, found));
        }
        outcomes.push((name.clone(), fetched.map(|found| found.len())));
    }
    let resolved = AgentConfig {
        providers: providers.clone(),
        ..agent.clone()
    };
    let models = available_models(&resolved, store);
    (providers, models, outcomes)
}

/// The provider definitions sessions launch with and the models they can
/// switch to, replaced whole when providers are asked again.
///
/// Shared rather than copied into each session, so a refresh reaches every
/// session's `!model` at once. A running sandbox keeps the definitions it was
/// launched with until its next launch.
#[derive(Clone, Default)]
pub struct Catalog {
    inner: Arc<RwLock<Contents>>,
}

/// The definitions, then the models, as one value so they change together.
type Contents = (Map<String, Value>, Vec<AvailableModel>);

impl Catalog {
    /// A catalog holding these definitions and models.
    pub fn new(providers: Map<String, Value>, models: Vec<AvailableModel>) -> Self {
        Self {
            inner: Arc::new(RwLock::new((providers, models))),
        }
    }

    /// The provider definitions a session is launched with.
    pub fn providers(&self) -> Map<String, Value> {
        self.inner.read().expect("the catalog lock").0.clone()
    }

    /// The models a session can switch to.
    pub fn models(&self) -> Vec<AvailableModel> {
        self.inner.read().expect("the catalog lock").1.clone()
    }

    /// Replaces what the catalog holds.
    pub fn replace(&self, providers: Map<String, Value>, models: Vec<AvailableModel>) {
        *self.inner.write().expect("the catalog lock") = (providers, models);
    }
}

#[cfg(test)]
mod tests;
