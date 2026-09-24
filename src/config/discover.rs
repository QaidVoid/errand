//! How a provider definition says to ask the provider which models it serves.
//!
//! Written as `discover` on a provider. `true` asks the OpenAI-shaped
//! `/models` endpoint under the provider's `baseUrl`. An object says where
//! the list is and where each field sits for a provider shaped otherwise.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// Where a provider lists its models, and how to read one entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Discovery {
    /// The path under the provider's `baseUrl` that answers with the list.
    pub path: String,
    /// Where the list sits in the answer, as a dotted path.
    pub list: String,
    /// Where each field sits in an entry, as a dotted path, keyed by the name
    /// the agent gives the field.
    pub fields: BTreeMap<String, String>,
    /// Fields every model found carries unless its entry says otherwise.
    pub defaults: Map<String, Value>,
}

/// The fields an entry can fill, and where they sit when nothing says.
///
/// `id` and `name` are where most providers put them. The two sizes are named
/// as gateways that report them commonly do.
const FIELDS: [(&str, &str); 4] = [
    ("id", "id"),
    ("name", "name"),
    ("contextWindow", "context_window"),
    ("maxTokens", "max_output_tokens"),
];

impl Discovery {
    /// Reads a provider definition's `discover`, or nothing when it has none
    /// or says `false`.
    ///
    /// Refuses with every problem it finds, named for the provider, rather
    /// than guessing at what a misspelt field meant.
    pub fn of(provider: &str, definition: &Value) -> Result<Option<Self>, Vec<String>> {
        let place = format!("agent.providers.{provider}.discover");
        let mut discovery = Self {
            path: "/models".to_owned(),
            list: "data".to_owned(),
            fields: FIELDS
                .iter()
                .map(|(field, at)| ((*field).to_owned(), (*at).to_owned()))
                .collect(),
            defaults: Map::new(),
        };
        let written = match definition.get("discover") {
            None | Some(Value::Bool(false)) => return Ok(None),
            Some(Value::Bool(true)) => return Ok(Some(discovery)),
            Some(Value::Object(written)) => written,
            Some(_) => return Err(vec![format!("{place} must be true, false, or an object")]),
        };

        let mut problems = Vec::new();
        for (key, value) in written {
            match (key.as_str(), value) {
                ("path", Value::String(path)) if path.starts_with('/') => {
                    discovery.path.clone_from(path);
                }
                ("path", _) => problems.push(format!("{place}.path must be a path starting with /")),
                ("list", Value::String(list)) if !list.trim().is_empty() => {
                    list.trim().clone_into(&mut discovery.list);
                }
                ("list", _) => problems.push(format!("{place}.list must name where the list is")),
                ("fields", Value::Object(fields)) => {
                    for (field, at) in fields {
                        if !FIELDS.iter().any(|(known, _)| known == field) {
                            problems.push(format!(
                                "{place}.fields.{field} is not a field a model can be given; \
                                 use id, name, contextWindow, or maxTokens"
                            ));
                            continue;
                        }
                        match at.as_str().map(str::trim) {
                            Some(at) if !at.is_empty() => {
                                discovery.fields.insert(field.clone(), at.to_owned());
                            }
                            _ => problems.push(format!(
                                "{place}.fields.{field} must name where the field is"
                            )),
                        }
                    }
                }
                ("fields", _) => problems.push(format!("{place}.fields must be an object")),
                ("defaults", Value::Object(defaults)) => discovery.defaults.clone_from(defaults),
                ("defaults", _) => problems.push(format!("{place}.defaults must be an object")),
                _ => problems.push(format!(
                    "{place}.{key} is not something discover takes; use path, list, fields, or defaults"
                )),
            }
        }
        if problems.is_empty() {
            Ok(Some(discovery))
        } else {
            Err(problems)
        }
    }
}

#[cfg(test)]
mod tests;
