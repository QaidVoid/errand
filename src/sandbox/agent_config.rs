//! What a sandboxed agent is told about providers.
//!
//! Written as kage's own configuration into the session's state, where the
//! sandbox mounts it. Only endpoints travel in the file: credentials stay in
//! the environment, under their usual names with a broker's nonces swapped
//! in, which kage reads itself.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Map, Value};

use crate::sandbox::backend::{KAGE_DIR, SandboxLaunch, SandboxLaunchError};
use crate::sandbox::paths;

/// Where the broker is reached for one provider.
///
/// The nonce standing in for the key travels in the environment, not here:
///
/// [`super::bailey::brokered_env`] swaps it into the provider's variable.
#[derive(Debug, Clone)]
pub struct BrokeredProvider {
    /// The base URL the session is pointed at.
    pub base_url: String,
}

/// Provider ids kage already knows, from its built-ins (`anthropic`,
/// `openai`, `openai-responses`, `gemini`) and its `COMPAT_PROVIDERS` table
/// at the pinned kage. One of these is overridden in place, keeping its
/// catalog models; anything else is registered as a custom provider with the
/// store's models. A kage that learns a new id takes the custom path for it,
/// which still reaches it but without catalog pricing, and says so in its
/// startup warning.
const KAGE_KNOWN: [&str; 20] = [
    "anthropic",
    "openai",
    "openai-responses",
    "gemini",
    "zai",
    "zai-coding-plan",
    "zhipuai-coding-plan",
    "deepseek",
    "groq",
    "mistral",
    "cerebras",
    "xai",
    "openrouter",
    "fireworks-ai",
    "moonshotai",
    "kimi-for-coding",
    "xiaomi",
    "xiaomi-token-plan-ams",
    "xiaomi-token-plan-cn",
    "xiaomi-token-plan-sgp",
];

/// A TOML basic string: backslashes, quotes, and line breaks escaped.
fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// A TOML table header segment: bare when the id allows, quoted otherwise.
fn toml_key(id: &str) -> String {
    if !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        id.to_owned()
    } else {
        toml_string(id)
    }
}

/// One `[[models]]` entry from a store model, naming what kage needs to
/// address it. Only the id is required; the name falls back to it and the
/// context passes through when the store said it.
fn kage_model(entry: &Value) -> Option<String> {
    let id = entry.get("id")?.as_str()?;
    let mut table = format!("id = {}", toml_string(id));
    let _ = write!(
        table,
        "\nname = {}",
        toml_string(entry.get("name").and_then(Value::as_str).unwrap_or(id))
    );
    let context = entry
        .get("context")
        .or_else(|| entry.get("contextWindow"))
        .and_then(Value::as_u64);
    if let Some(context) = context {
        let _ = write!(table, "\ncontext = {context}");
    }
    Some(table)
}

/// The kage provider configuration for one session, as TOML.
///
/// Brokered providers point at the broker with the nonce's variable named,
/// so what the file holds is worth nothing anywhere but this broker.
/// Operator definitions contribute their base URL; their credentials never
/// enter the file, traveling in the environment instead. Providers kage does
/// not know are registered custom with the store's models.
pub fn kage_config(
    defined: &Map<String, Value>,
    brokered: &BTreeMap<String, BrokeredProvider>,
    built_in: &BTreeMap<String, Vec<Value>>,
    credential_names: &BTreeMap<String, String>,
) -> String {
    let mut out = String::from(
        "# Written by the daemon for one session. Endpoints only: credentials\n\
         # travel in the environment, with a broker's nonces swapped in.\n",
    );
    let names: Vec<&String> = defined
        .keys()
        .chain(brokered.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    for name in names {
        let through = brokered.get(name);
        let definition = defined
            .get(name)
            .and_then(Value::as_object)
            .filter(|fields| fields.get("extension").and_then(Value::as_bool) != Some(true));
        let base_url = through.map(|through| through.base_url.clone()).or_else(|| {
            definition
                .and_then(|fields| fields.get("baseUrl"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        if through.is_none() && definition.is_none() {
            continue;
        }
        // Nothing to tell kage: no endpoint override and no models to declare.
        // The key still reaches it through the environment on its own.
        let models: Vec<String> = if KAGE_KNOWN.contains(&name.as_str()) {
            Vec::new()
        } else {
            defined
                .get(name)
                .and_then(|definition| definition.get("models"))
                .and_then(Value::as_array)
                .map(|written| written.iter().filter_map(kage_model).collect::<Vec<_>>())
                .unwrap_or_default()
                .into_iter()
                .chain(
                    built_in
                        .get(name)
                        .map(|known| known.iter().filter_map(kage_model).collect::<Vec<_>>())
                        .unwrap_or_default(),
                )
                .collect()
        };
        if base_url.is_none() && models.is_empty() {
            continue;
        }
        if KAGE_KNOWN.contains(&name.as_str()) {
            let _ = writeln!(out, "\n[providers.{}]", toml_key(name));
            if let Some(url) = base_url {
                let _ = writeln!(out, "base_url = {}", toml_string(&url));
            }
            continue;
        }
        let _ = writeln!(out, "\n[providers.custom.{}]", toml_key(name));
        out.push_str("kind = \"openai\"\n");
        if let Some(url) = base_url {
            let _ = writeln!(out, "base_url = {}", toml_string(&url));
        }
        let _ = writeln!(out, "display_name = {}", toml_string(name));
        if let Some(env) = credential_names.get(name) {
            let _ = writeln!(out, "api_key_env = {}", toml_string(env));
        }
        for model in models {
            let _ = writeln!(
                out,
                "\n[[providers.custom.{}.models]]\n{model}",
                toml_key(name)
            );
        }
    }
    out
}

/// Writes the agent's provider configuration.
///
/// Only endpoints and model declarations go in the file; credentials stay in
/// the environment. Pi extensions have no kage equivalent, so naming one
/// fails the launch rather than starting a session without the tools its
/// configuration promises.
pub async fn write_agent_config(
    launch: &SandboxLaunch,
    brokered: &BTreeMap<String, BrokeredProvider>,
    built_in: &BTreeMap<String, Vec<Value>>,
    credential_names: &BTreeMap<String, String>,
) -> Result<(), SandboxLaunchError> {
    if let Some(first) = launch.extensions.first() {
        return Err(SandboxLaunchError(format!(
            "extension {first} has no kage equivalent; remove it to start sessions on kage"
        )));
    }
    let failed = |error: std::io::Error| SandboxLaunchError(error.to_string());
    // The host side of `{KAGE_HOME}`, with kage's own `kage` segment under
    // it where the configuration is read from.
    let directory = Path::new(&launch.state_dir).join(KAGE_DIR).join("kage");
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(failed)?;
    let body = kage_config(&launch.providers, brokered, built_in, credential_names);
    // Written beneath the directory rather than by name, for the same
    // reason the policy is: the session writes here too, and a link
    // planted at the file name would redirect the write.
    paths::write_beneath(&directory.to_string_lossy(), "config.toml", body.as_bytes())
        .map_err(failed)?;
    Ok(())
}

#[cfg(test)]
mod tests;
