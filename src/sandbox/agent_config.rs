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
use crate::log::{Logger, fields};
use crate::sandbox::backend::{Denials, KAGE_DIR, SandboxLaunch, SandboxLaunchError};
use crate::sandbox::kage_providers::KageProviders;
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

/// The kinds of media kage knows a model can take or produce, as the store
/// and the configuration name them.
const KAGE_MODALITIES: [&str; 5] = ["text", "image", "pdf", "audio", "video"];

/// The thinking levels kage offers a model, as the configuration names them.
const KAGE_EFFORTS: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// One `[[models]]` entry from a store model, naming what kage needs to
/// address it. Only the id is required; the name falls back to it and the
/// context passes through when the store said it. The output limit, the
/// inputs, what it produces, and the thinking levels pass through too, in
/// kage's own words: a model whose window was set without its output limit
/// compacts at the wrong size, one without its inputs never receives an
/// image, and one that draws without `produces` never hands its pictures
/// over.
///
/// Anything kage does not know is left out rather than guessed at. A level it
/// never heard of would fail the whole configuration file, which would cost
/// every other provider their session too.
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
    let max_output = entry
        .get("max_output")
        .or_else(|| entry.get("maxTokens"))
        .and_then(Value::as_u64)
        .filter(|max| u32::try_from(*max).is_ok() && *max > 0);
    if let Some(max_output) = max_output {
        let _ = write!(table, "\nmax_output = {max_output}");
    }
    if let Some(inputs) = model_modalities(entry, "input") {
        let _ = write!(table, "\ninput = [{inputs}]");
    }
    if let Some(produces) = model_modalities(entry, "produces") {
        let _ = write!(table, "\nproduces = [{produces}]");
    }
    if let Some(reasoning) = model_reasoning(entry) {
        let _ = write!(table, "\nreasoning = {reasoning}");
    }
    if let Some(efforts) = model_efforts(entry) {
        let _ = write!(table, "\nefforts = [{efforts}]");
    }
    Some(table)
}

/// The kinds of media a model entry claims under `field`, `input` or
/// `produces`, as a TOML list, or nothing when it names none kage knows.
/// Unknown kinds are dropped one by one: a gateway inventing a new one must
/// not cost the model its images.
fn model_modalities(entry: &Value, field: &str) -> Option<String> {
    let kinds = entry.get(field)?.as_array()?;
    let mut known: Vec<&str> = Vec::new();
    for kind in kinds.iter().filter_map(Value::as_str) {
        if KAGE_MODALITIES.contains(&kind) && !known.contains(&kind) {
            known.push(kind);
        }
    }
    (!known.is_empty()).then(|| {
        known
            .iter()
            .map(|kind| toml_string(kind))
            .collect::<Vec<_>>()
            .join(", ")
    })
}

/// Whether a model entry says the model thinks, when it says anything at
/// all. Read from `reasoning`, kage's own word for it.
fn model_reasoning(entry: &Value) -> Option<bool> {
    entry.get("reasoning")?.as_bool()
}

/// The thinking levels a model entry claims, as a TOML list, or nothing when
/// it names none kage offers. Unknown levels are dropped one by one, for the
/// same reason unknown inputs are: a model that lists levels stays addressable
/// at the ones kage knows.
fn model_efforts(entry: &Value) -> Option<String> {
    let levels = entry.get("efforts")?.as_array()?;
    let mut known: Vec<&str> = Vec::new();
    for level in levels.iter().filter_map(Value::as_str) {
        if KAGE_EFFORTS.contains(&level) && !known.contains(&level) {
            known.push(level);
        }
    }
    (!known.is_empty()).then(|| {
        known
            .iter()
            .map(|level| toml_string(level))
            .collect::<Vec<_>>()
            .join(", ")
    })
}

/// The agent configuration and provider declarations it could not include.
#[derive(Debug)]
pub struct KageConfig {
    /// Configuration text containing endpoints and environment variable names.
    pub toml: String,
    /// Providers omitted because their declarations are incomplete.
    pub dropped: Vec<DroppedProvider>,
}

/// One provider omitted from the agent's provider table.
#[derive(Debug)]
pub struct DroppedProvider {
    /// The operator's provider name, never its credential or endpoint.
    pub name: String,
    /// Which part of the declaration was missing.
    pub reason: Missing,
    /// Whether it carried `images`, so the session lacks the image tool too.
    pub images: bool,
    /// Whether it carried `speech`, so the session lacks the speech tool too.
    pub speech: bool,
    /// Whether it carried `music`, so the session lacks the music tool too.
    pub music: bool,
}

impl DroppedProvider {
    /// The report for a provider the writer leaves out, `known` when kage
    /// overrides it, or nothing for a media-only shape: a provider carrying
    /// `images`, `speech` or `music` with an endpoint and no models is reached
    /// by that tool through its endpoint, so nothing is missing.
    fn of(
        name: &str,
        definition: Option<&Map<String, Value>>,
        known: bool,
        has_endpoint: bool,
    ) -> Option<Self> {
        let carries = |entry: &str| definition.is_some_and(|fields| fields.contains_key(entry));
        let (images, speech, music) = (carries("images"), carries("speech"), carries("music"));
        let reason = if known {
            Missing::Credential
        } else if has_endpoint {
            Missing::Models
        } else {
            Missing::Endpoint
        };
        (!((images || speech || music) && reason == Missing::Models)).then(|| Self {
            name: name.to_owned(),
            reason,
            images,
            speech,
            music,
        })
    }
}

/// What a provider declaration lacks for the agent to be told about it.
#[derive(Debug, PartialEq, Eq)]
pub enum Missing {
    /// A provider kage knows was given neither an endpoint nor a credential.
    Credential,
    /// A provider kage does not know was given no endpoint.
    Endpoint,
    /// A provider kage does not know was given no models.
    Models,
}

impl Missing {
    fn message(&self) -> &'static str {
        match self {
            Self::Credential => "it has neither a baseUrl nor a credential",
            Self::Endpoint => "kage does not know this provider, so it needs a baseUrl",
            Self::Models => "kage does not know this provider, so it needs at least one model",
        }
    }
}

/// The model declarations a custom provider is written with: the ones its
/// definition lists, then the host store's for that provider.
fn custom_models(
    defined: &Map<String, Value>,
    built_in: &BTreeMap<String, Vec<Value>>,
    name: &str,
) -> Vec<String> {
    defined
        .get(name)
        .and_then(|definition| definition.get("models"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(built_in.get(name).into_iter().flatten())
        .filter_map(kage_model)
        .collect()
}

/// The kage provider configuration for one session, as TOML.
///
/// Brokered providers point at the broker with the nonce's variable named,
/// so what the file holds is worth nothing anywhere but this broker.
/// Operator definitions contribute their base URL; their credentials never
/// enter the file, traveling in the environment instead. A provider `kage`
/// says it overrides keeps kage's own models, and any other is registered
/// custom with the store's models: the id, the name, and, when the store says
/// them, the context window, the output limit, the inputs, and the thinking
/// levels. A `kind` on the definition says which
/// wire protocol they speak, defaulting to `openai`. A provider an
/// extension registers (`extension: true`) is left out entirely: the plugin
/// brings its own endpoint, and writing errand's would only disagree with
/// it. The providers carrying `images`, `speech` and `music` also give the
/// agent those tools, and an `editModel` on `images` gives it `edit_image`.
pub fn kage_config(
    defined: &Map<String, Value>,
    brokered: &BTreeMap<String, BrokeredProvider>,
    built_in: &BTreeMap<String, Vec<Value>>,
    credential_names: &BTreeMap<String, String>,
    plugins: &[PluginConfig],
    denied: &Denials,
    kage: &KageProviders,
) -> KageConfig {
    let mut dropped = Vec::new();
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
    // Where each provider is reached, and which ones the agent is told about,
    // for the image tool to reuse below.
    let mut endpoints: BTreeMap<&str, String> = BTreeMap::new();
    let mut written: Vec<&str> = Vec::new();
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
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .map(str::to_owned)
        });
        // Validation refuses an id kage reserves, at startup and on reload.
        // One reaching here anyway is left out rather than written into a
        // file kage would refuse whole.
        if (through.is_none() && definition.is_none()) || kage.reserves(name) {
            continue;
        }
        if let Some(url) = &base_url {
            endpoints.insert(name.as_str(), url.clone());
        }
        let known = kage.overrides(name);
        let models = if known {
            Vec::new()
        } else {
            custom_models(defined, built_in, name)
        };
        // A custom provider kage does not know must declare both an endpoint and
        // at least one model: it refuses the whole file when either is
        // missing, which would cost every other provider their session too.
        // One errand cannot describe completely is left out instead. A known
        // provider needs neither, since kage already knows its endpoint and
        // its models, so a variable alone is enough to have it written.
        let worth_writing = if known {
            base_url.is_some() || credential_names.contains_key(name)
        } else {
            base_url.is_some() && !models.is_empty()
        };
        if !worth_writing {
            dropped.extend(DroppedProvider::of(
                name,
                definition,
                known,
                base_url.is_some(),
            ));
            continue;
        }
        written.push(name.as_str());
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
    out.push_str(&media_tables(
        defined,
        &written,
        &endpoints,
        credential_names,
    ));
    out.push_str(&plugin_capabilities(plugins));
    out.push_str(&permissions_section(denied));
    KageConfig { toml: out, dropped }
}

/// Every media tool's table, in a fixed order: images and their edits,
/// speech, then music.
fn media_tables(
    defined: &Map<String, Value>,
    written: &[&str],
    endpoints: &BTreeMap<&str, String>,
    credential_names: &BTreeMap<String, String>,
) -> String {
    generate_image(defined, written, endpoints, credential_names)
        + &generate_speech(defined, written, endpoints, credential_names)
        + &media_table(
            ("music", "model"),
            "tools.generate_music",
            defined,
            written,
            endpoints,
            credential_names,
        )
}

/// The `[tools.generate_image]` table, for the provider whose definition
/// carries `images`, and the `[tools.edit_image]` table on the same
/// endpoint when the entry names an `editModel`, or nothing when none
/// does. See [`media_table`].
fn generate_image(
    defined: &Map<String, Value>,
    written: &[&str],
    endpoints: &BTreeMap<&str, String>,
    credential_names: &BTreeMap<String, String>,
) -> String {
    let table = |model, table| {
        media_table(
            ("images", model),
            table,
            defined,
            written,
            endpoints,
            credential_names,
        )
    };
    table("model", "tools.generate_image") + &table("editModel", "tools.edit_image")
}

/// The `[tools.generate_speech]` table, for the provider whose definition
/// carries `speech`, or nothing when none does. A `voice` on the entry is
/// the one asked for when the agent names none. See [`media_table`].
fn generate_speech(
    defined: &Map<String, Value>,
    written: &[&str],
    endpoints: &BTreeMap<&str, String>,
    credential_names: &BTreeMap<String, String>,
) -> String {
    let mut out = media_table(
        ("speech", "model"),
        "tools.generate_speech",
        defined,
        written,
        endpoints,
        credential_names,
    );
    if out.is_empty() {
        return out;
    }
    let voice = defined
        .iter()
        .filter(|(_, fields)| fields.get("extension").and_then(Value::as_bool) != Some(true))
        .find_map(|(_, fields)| fields.get("speech")?.get("voice")?.as_str())
        .map(str::trim)
        .filter(|voice| !voice.is_empty());
    if let Some(voice) = voice {
        let _ = writeln!(out, "voice = {}", toml_string(voice));
    }
    out
}

/// The table one media tool is written as, shared by every media table:
/// the header for `table`, the provider whose definition carries `entry`,
/// and the model the entry names under `model`. An entry without that key
/// writes nothing.
///
/// A provider the agent is already told about is named, and lends the tool
/// its endpoint and key the way the agent reaches it, broker and nonce
/// included. One that lists no models is not told about above, since the
/// agent refuses a provider without models and a media endpoint's models
/// must not be offered for chat: it is named by its endpoint and the
/// variable its key is read from instead, which under the broker are the
/// broker's address and the nonce. One with neither is left out, and the
/// session runs without the tool.
fn media_table(
    (entry, model): (&str, &str),
    table: &str,
    defined: &Map<String, Value>,
    written: &[&str],
    endpoints: &BTreeMap<&str, String>,
    credential_names: &BTreeMap<String, String>,
) -> String {
    let Some((name, fields)) = defined
        .iter()
        .filter(|(_, fields)| fields.get("extension").and_then(Value::as_bool) != Some(true))
        .find_map(|(name, fields)| Some((name, fields.get(entry)?.as_object()?)))
    else {
        return String::new();
    };
    let Some(model) = fields
        .get(model)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
    else {
        return String::new();
    };
    let mut out = format!("\n[{table}]\n");
    if written.contains(&name.as_str()) {
        let _ = writeln!(out, "provider = {}", toml_string(name));
    } else if let Some(url) = endpoints.get(name.as_str()) {
        let _ = writeln!(out, "base_url = {}", toml_string(url));
        if let Some(env) = credential_names.get(name) {
            let _ = writeln!(out, "api_key_env = {}", toml_string(env));
        }
    } else {
        return String::new();
    }
    let _ = writeln!(out, "model = {}", toml_string(model));
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

/// What the operator refuses, refused by the agent itself.
///
/// The agent runs with `--yolo`, so every tool runs without asking: a thread
/// cannot serve a permission prompt, and an ask nobody sees is a session that
/// waits out its dialog timer. A configured deny still refuses under it, so
/// only the operator's refusals are written.
fn permissions_section(denied: &Denials) -> String {
    let mut out = String::new();
    let denied_tool = |tool: &str| denied.tools.iter().any(|denied| denied == tool);
    // A tool refused outright takes no denied commands: it runs nothing
    // either way, and a second table for it is a TOML parse error.
    if !denied.commands.is_empty() && !denied_tool("shell") {
        let patterns: Vec<String> = denied.commands.iter().map(|c| toml_string(c)).collect();
        let _ = writeln!(out, "\n[permissions.tools.shell]");
        let _ = writeln!(out, "deny = [{}]", patterns.join(", "));
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
    log: &Logger,
    kage: &KageProviders,
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
        kage,
    );
    for provider in &body.dropped {
        let tools: Vec<&str> = [
            (provider.images, "image"),
            (provider.speech, "speech"),
            (provider.music, "music"),
        ]
        .into_iter()
        .filter_map(|(carried, tool)| carried.then_some(tool))
        .collect();
        let missing = match tools.as_slice() {
            [] => "a provider declaration".to_owned(),
            [tool] => format!("the {tool} tool or a provider declaration"),
            [rest @ .., last] => format!(
                "the {} and {last} tools or a provider declaration",
                rest.join(", ")
            ),
        };
        log.warn(
            &format!("the session runs without {missing}"),
            &fields([
                ("session", launch.session_id.as_str().into()),
                ("provider", provider.name.as_str().into()),
                ("detail", provider.reason.message().into()),
            ]),
        );
    }
    // Written beneath the directory rather than by name, for the same
    // reason the policy is: the session writes here too, and a link
    // planted at the file name would redirect the write.
    paths::write_beneath(
        &directory.to_string_lossy(),
        "config.toml",
        body.toml.as_bytes(),
    )
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
