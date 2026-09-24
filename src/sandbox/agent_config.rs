//! What a sandboxed agent is told about providers and extensions.
//!
//! Written into the agent's own configuration directory inside the session's
//! state, which every backend gives the agent as its home, so a bailey session
//! and a podman session read the same files.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value};

use crate::sandbox::backend::{SandboxLaunch, SandboxLaunchError};

/// Where the broker is reached, and the nonce standing in for the key.
#[derive(Debug, Clone)]
pub struct BrokeredProvider {
    /// The base URL the session is pointed at.
    pub base_url: String,
    /// What stands in for the credential.
    pub nonce: String,
}

/// The agent's provider configuration for one session.
///
/// The operator's definitions first, then the broker's base URL over the one
/// provider it stands in for. Merged rather than written over the top: a
/// definition is how a provider with no built-in entry is reached at all, and
/// replacing it wholesale would leave the agent with a provider it has never
/// heard of. Only the base URL is taken from the broker, so everything else
/// the operator said about that provider still stands.
///
/// Without a broker there is nothing to put a key on in transit, so with
/// `hand_over_keys` each credential is written as the provider's `apiKey`.
/// Otherwise only the provider the session starts on would have one, and the
/// agent refuses to switch to any other.
pub fn provider_config(
    defined: &Map<String, Value>,
    brokered: &BTreeMap<String, BrokeredProvider>,
    built_in: &BTreeMap<String, Vec<Value>>,
    hand_over_keys: bool,
) -> Map<String, Value> {
    let mut providers = Map::new();
    for (name, definition) in defined {
        // An extension registers this provider itself, so writing a second
        // definition here would collide with the one the extension makes.
        if definition.get("extension").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let mut fields = match definition {
            Value::Object(fields) => fields.clone(),
            _ => Map::new(),
        };
        // The credential is the daemon's record of how to reach the provider,
        // not the agent's. Under a broker it is put on there instead.
        let credential = fields.remove("credential");
        if hand_over_keys
            && !fields.contains_key("apiKey")
            && let Some(credential) = credential
        {
            fields.insert("apiKey".to_owned(), credential);
        }
        // How the daemon asks about the window is the daemon's business too,
        // and the agent's configuration would only report it as a field it
        // does not know.
        fields.remove("usage");
        fields.remove("discover");
        if let (Some(Value::Array(models)), Some(known)) =
            (fields.get_mut("models"), built_in.get(name))
        {
            for model in models {
                *model = over_built_in(model, known);
            }
        }
        providers.insert(name.clone(), Value::Object(fields));
    }

    for (name, through) in brokered {
        let mut fields = match providers.get(name) {
            Some(Value::Object(fields)) => fields.clone(),
            _ => Map::new(),
        };
        // The nonce stands in for the key, so what the agent holds is worth
        // nothing anywhere but this broker.
        fields.insert(
            "baseUrl".to_owned(),
            Value::String(through.base_url.clone()),
        );
        fields.insert("apiKey".to_owned(), Value::String(through.nonce.clone()));
        providers.insert(name.clone(), Value::Object(fields));
    }
    // The agent reads its models from a file whose shape names the map, so
    // the providers ride under that key rather than at the top level.
    let mut wrapped = Map::new();
    wrapped.insert("providers".to_owned(), Value::Object(providers));
    wrapped
}

/// A model entry laid over the agent's own definition of that model.
///
/// The agent replaces a built-in model with an entry of the same id rather
/// than merging the two, so an entry that only sets `contextWindow` would
/// lose the rest, reasoning and thinking levels included. The store's
/// definition goes underneath instead. Its `baseUrl` and `provider` are left
/// out, so the model is still reached wherever its provider is, broker
/// included. An entry the store does not know is left as written.
fn over_built_in(entry: &Value, known: &[Value]) -> Value {
    let id = entry.get("id").and_then(Value::as_str);
    let Some(Value::Object(base)) = known
        .iter()
        .find(|model| id.is_some() && model.get("id").and_then(Value::as_str) == id)
    else {
        return entry.clone();
    };
    let mut merged = base.clone();
    merged.remove("baseUrl");
    merged.remove("provider");
    if let Value::Object(fields) = entry {
        merged.extend(fields.clone());
    }
    Value::Object(merged)
}

/// Copies a file or a directory tree from the host into the session.
///
/// Recursive and shallow-simple: pi extensions are a file or a small folder,
/// so this walks directories and copies files, which is all one needs. A
/// symlink is followed by the copy, which is what reading the named directory
/// means.
pub(crate) async fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    let meta = tokio::fs::metadata(from).await?;
    if meta.is_dir() {
        tokio::fs::create_dir_all(to).await?;
        let mut entries = tokio::fs::read_dir(from).await?;
        while let Some(entry) = entries.next_entry().await? {
            Box::pin(copy_tree(&entry.path(), &to.join(entry.file_name()))).await?;
        }
    } else {
        if let Some(parent) = to.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::copy(from, to).await?;
    }
    Ok(())
}

/// Writes the agent's provider configuration and places its extensions.
///
/// `brokered` is the broker's route for each provider it stands in front of,
/// and `hand_over_keys` says that no broker puts keys on, so each provider's
/// is written for the agent. The host's own pi configuration is invisible to
/// a sandbox, so an extension installed there is copied into the `extensions`
/// directory the agent scans. One that cannot be read fails the launch rather
/// than being skipped: an extension the operator named that never loaded is
/// a misconfiguration worth surfacing.
pub async fn write_agent_config(
    launch: &SandboxLaunch,
    brokered: &BTreeMap<String, BrokeredProvider>,
    built_in: &BTreeMap<String, Vec<Value>>,
    hand_over_keys: bool,
) -> Result<(), SandboxLaunchError> {
    let failed = |error: std::io::Error| SandboxLaunchError(error.to_string());
    let directory = Path::new(&launch.state_dir)
        .join("home")
        .join(".pi")
        .join("agent");

    if !brokered.is_empty() || !launch.providers.is_empty() {
        let providers = provider_config(&launch.providers, brokered, built_in, hand_over_keys);
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(failed)?;
        let body = format!(
            "{}\n",
            serde_json::to_string_pretty(&providers).unwrap_or_default()
        );
        tokio::fs::write(directory.join("models.json"), body)
            .await
            .map_err(failed)?;
    }

    for source in &launch.extensions {
        let source = Path::new(source);
        let Some(name) = source.file_name() else {
            continue;
        };
        copy_tree(source, &directory.join("extensions").join(name))
            .await
            .map_err(failed)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
