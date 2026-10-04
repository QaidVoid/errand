//! What a sandboxed agent is told about providers.
//!
//! Written as kage's own configuration into the session's state, where the
//! sandbox mounts it. Only endpoints travel in the file: credentials stay in
//! the environment, under their usual names with a broker's nonces swapped
//! in, which kage reads itself. Configured plugins are copied beside it, so
//! the session loads what the operator named and nothing else.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Map, Value};

use crate::config::schema::PluginConfig;
use crate::config::schema::defaults::{CUSTOM_PROVIDER_KIND, CUSTOM_PROVIDER_KINDS};
use crate::sandbox::backend::{Denials, KAGE_DIR, SandboxLaunch, SandboxLaunchError};
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

/// The tools the agent ships with, allowed so it never asks about one.
///
/// Named one by one because the agent's rules are keyed by literal tool
/// name and an unlisted tool falls back to asking. A tool it gains later
/// asks again, which costs a stalled turn rather than a wrong permission, so
/// the list is read from what this build registers rather than guessed at.
///
/// Plugin and MCP tools are not here: they are covered by the wildcard
/// under `permissions.mcp`, since their names carry a server prefix this
/// list cannot know.
const TOOLS_ALLOWED: [&str; 10] = [
    "edit",
    "find",
    "grep",
    "ls",
    "read",
    "shell",
    "todo_list",
    "web_fetch",
    "web_search",
    "write",
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

/// The wire protocol one custom provider speaks, as kage names it.
///
/// Read from the definition's `kind`, which kage's own configuration also
/// calls it, so an operator copies the word they already know. Anything else,
/// including nothing, means the default: validation owns the refusal of a
/// word kage would not accept, and this is also read on paths validation
/// never saw.
fn provider_kind(definition: Option<&Map<String, Value>>) -> &str {
    definition
        .and_then(|fields| fields.get("kind"))
        .and_then(Value::as_str)
        .filter(|kind| CUSTOM_PROVIDER_KINDS.contains(kind))
        .unwrap_or(CUSTOM_PROVIDER_KIND)
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
/// not know are registered custom with the store's models, and their `kind`
/// says which wire protocol they speak, defaulting to `openai`. A provider an
/// extension registers (`extension: true`) is left out entirely: the plugin
/// brings its own endpoint, and writing errand's would only disagree with
/// it.
pub fn kage_config(
    defined: &Map<String, Value>,
    brokered: &BTreeMap<String, BrokeredProvider>,
    built_in: &BTreeMap<String, Vec<Value>>,
    credential_names: &BTreeMap<String, String>,
    plugins: &[PluginConfig],
    denied: &Denials,
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
        let known = KAGE_KNOWN.contains(&name.as_str());
        let models: Vec<String> = if known {
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
        // A custom provider kage does not know must declare both an endpoint and
        // at least one model: it refuses the whole file when either is
        // missing, which would cost every other provider their session too.
        // One errand cannot describe completely is left out instead. A known
        // provider needs neither, since kage already knows its endpoint and
        // its models, so a variable alone is enough to have it written.
        let known = KAGE_KNOWN.contains(&name.as_str());
        let worth_writing = if known {
            base_url.is_some() || credential_names.contains_key(name)
        } else {
            base_url.is_some() && !models.is_empty()
        };
        if !worth_writing {
            continue;
        }
        if known {
            let _ = writeln!(out, "\n[providers.{}]", toml_key(name));
            if let Some(url) = base_url {
                let _ = writeln!(out, "base_url = {}", toml_string(&url));
            }
            if let Some(env) = credential_names.get(name) {
                let _ = writeln!(out, "api_key_env = {}", toml_string(env));
            }
            continue;
        }
        let _ = writeln!(out, "\n[providers.custom.{}]", toml_key(name));
        let _ = writeln!(out, "kind = {}", toml_string(provider_kind(definition)));
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
    out.push_str(&plugin_capabilities(plugins));
    out.push_str(&permissions_section(denied));
    out
}

/// The `[plugins.capabilities]` table for one session.
///
/// Keyed by the plugin file's stem, which is how kage names a grant. A
/// plugin granted nothing writes nothing: kage then exposes it only the
/// sandboxed base surface, and one left without what it needs disables
/// itself at load, saying so.
fn plugin_capabilities(plugins: &[PluginConfig]) -> String {
    if plugins.iter().all(|plugin| plugin.capabilities.is_empty()) {
        return String::new();
    }
    let mut out = String::from("\n[plugins.capabilities]\n");
    for plugin in plugins {
        if plugin.capabilities.is_empty() {
            continue;
        }
        let stem = Path::new(&plugin.path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(plugin.path.as_str());
        let grants = plugin
            .capabilities
            .iter()
            .map(|name| toml_string(name))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "{} = [{grants}]", toml_key(stem));
    }
    out
}

/// Every tool allowed, and what may not be used anyway.
///
/// The agent asks before running a tool, and sets its fallback to asking for
/// every one of them. A thread cannot answer: the ask arrives as a request
/// the agent blocks on, and nobody is watching a chat channel for it, so the
/// session waits out its dialog timer and reports a question that was never
/// really put to anybody. Allowing each tool by name is what stops that, and
/// it stops it in the agent rather than by answering for it here.
///
/// MCP tools are covered by the wildcard, since errand starts no servers of
/// its own but a plugin may bring some. The agent's own refusals are left
/// intact: this only decides what may run without asking.
fn permissions_section(denied: &Denials) -> String {
    let mut out = String::from(
        "\n# Every tool runs without asking: the thread cannot serve a permission\n\
         # prompt, and an ask nobody sees is a session that waits. What the\n\
         # operator refuses below is refused by the agent itself.\n",
    );
    let _ = writeln!(out, "\n[permissions.mcp]");
    let _ = writeln!(out, "\"*\" = \"allow\"");
    // One table per tool. A tool named twice is a TOML parse error, which
    // ends the launch, so a tool is written once and decided there. A tool
    // the operator refuses outright takes no denied commands: it runs
    // nothing either way, so the patterns would say nothing.
    let denied_tool = |tool: &str| denied.tools.iter().any(|denied| denied == tool);
    for tool in TOOLS_ALLOWED.iter().filter(|tool| !denied_tool(tool)) {
        let _ = writeln!(out, "\n[permissions.tools.{}]", toml_key(tool));
        let _ = writeln!(out, "default = \"allow\"");
        if tool == &"shell" {
            // A list, which the schema requires: `deny` and `allow` take
            // one entry each. A bare string here is a parse error, which
            // ends the launch rather than the command.
            for pattern in &denied.commands {
                let _ = writeln!(out, "deny = [{}]", toml_string(pattern));
            }
        }
    }
    for tool in &denied.tools {
        let _ = writeln!(out, "\n[permissions.tools.{}]", toml_key(tool));
        let _ = writeln!(out, "default = \"deny\"");
    }
    out
}

/// Writes the agent's provider configuration and places its plugins.
///
/// Only endpoints and model declarations go in the file; credentials stay in
/// the environment. Each configured plugin is copied into the directory kage
/// reads plugins from, so what the session loads is exactly what the
/// configuration named, and its capability grants ride the same file.
pub async fn write_agent_config(
    launch: &SandboxLaunch,
    brokered: &BTreeMap<String, BrokeredProvider>,
    built_in: &BTreeMap<String, Vec<Value>>,
    credential_names: &BTreeMap<String, String>,
) -> Result<(), SandboxLaunchError> {
    let failed = |error: std::io::Error| SandboxLaunchError(error.to_string());
    // The host side of `{KAGE_HOME}`, with kage's own `kage` segment under
    // it where the configuration is read from.
    let directory = Path::new(&launch.state_dir).join(KAGE_DIR).join("kage");
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(failed)?;
    let body = kage_config(
        &launch.providers,
        brokered,
        built_in,
        credential_names,
        &launch.plugins,
        &launch.denied,
    );
    // Written beneath the directory rather than by name, for the same
    // reason the policy is: the session writes here too, and a link
    // planted at the file name would redirect the write.
    paths::write_beneath(&directory.to_string_lossy(), "config.toml", body.as_bytes())
        .map_err(failed)?;
    copy_plugins(&directory.join("plugins"), &launch.plugins).await
}

/// Copies the configured plugins into the directory kage reads plugins from.
///
/// The directory is emptied first, so the set the session loads is exactly
/// what the configuration names now rather than whatever an earlier launch
/// left. Each file is written beneath the directory rather than by name, as
/// the configuration is. Two plugins copying to one name would have the
/// second silently replace the first, so that is refused instead.
async fn copy_plugins(
    directory: &Path,
    plugins: &[PluginConfig],
) -> Result<(), SandboxLaunchError> {
    let failed = |error: std::io::Error| SandboxLaunchError(error.to_string());
    match tokio::fs::remove_dir_all(directory).await {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(failed(error)),
        _ => {}
    }
    tokio::fs::create_dir_all(directory).await.map_err(failed)?;
    let mut taken: BTreeMap<String, String> = BTreeMap::new();
    for plugin in plugins {
        let bytes = tokio::fs::read(&plugin.path).await.map_err(|error| {
            SandboxLaunchError(format!("reading plugin {}: {error}", plugin.path))
        })?;
        let name = Path::new(&plugin.path)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| Path::new(name).extension() == Some(std::ffi::OsStr::new("lua")))
            .ok_or_else(|| {
                SandboxLaunchError(format!(
                    "plugin path {} does not name a .lua file",
                    plugin.path
                ))
            })?
            .to_owned();
        if let Some(previous) = taken.get(&name) {
            return Err(SandboxLaunchError(format!(
                "two plugins copy to {name}: {previous} and {}; rename one",
                plugin.path
            )));
        }
        taken.insert(name.clone(), plugin.path.clone());
        paths::write_beneath(&directory.to_string_lossy(), &name, &bytes).map_err(failed)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
