//! Turns whatever was on disk into a [`Config`], or refuses with reasons.
//!
//! Every problem is collected before anything is returned, and an unknown key
//! is a problem rather than something quietly ignored: a misspelled setting
//! that takes no effect is worse than one that is rejected, because the daemon
//! then runs with a guarantee somebody believes they configured.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{is_absolute, resolve};
use crate::config::schema::{
    self, AgentConfig, ChatConfig, Config, ConfigError, DelegateConfig, EgressConfig, EgressMode,
    GithubConfig, LimitsConfig, NetworkMode, OutputConfig, PolicyExtraConfig, SandboxBackend,
    SandboxConfig, ShutdownConfig, TimeoutsConfig, WebConfig,
};
use crate::config::size::parse_size;

/// Variables the policy sets itself, and so refuses to take from a file.
const SET_BY_THE_POLICY: [&str; 2] = ["PATH", "HOME"];

const KNOWN_ROOT: [&str; 12] = [
    // Not a setting: it is how an editor finds the schema to check the file
    // against, and refusing it would make the file uncheckable.
    "$schema",
    "chat",
    "agent",
    "github",
    "projectRoot",
    "stateDir",
    "sandbox",
    "output",
    "shutdown",
    "web",
    "limits",
    "timeouts",
];
const KNOWN_CHAT: [&str; 6] = [
    "token",
    "channelId",
    "allowedUserIds",
    "blockedUserIds",
    "operatorUserIds",
    "startOnMention",
];
const KNOWN_AGENT: [&str; 10] = [
    "provider",
    "model",
    "visionModel",
    "delegate",
    "rulesPath",
    "providers",
    "extensions",
    "aliases",
    // Known so that finding one is answered with where it went, rather than
    // with the spelling check an actual typo gets.
    "credential",
    "credentialName",
];
const KNOWN_DELEGATE: [&str; 4] = ["model", "perTurn", "deadlineMs", "baseUrl"];
const KNOWN_GITHUB: [&str; 3] = ["token", "userName", "userEmail"];
const KNOWN_SANDBOX: [&str; 17] = [
    "backend",
    "requireFullEnforcement",
    "network",
    "egressPorts",
    "egress",
    "hideHostAddress",
    "image",
    "memory",
    "cpus",
    "pids",
    "fileMax",
    "disk",
    "diskCheckMs",
    "gracePeriodMs",
    "policyExtra",
    "pathExtra",
    "env",
];
const KNOWN_EGRESS: [&str; 3] = ["mode", "allow", "allowInternal"];
const KNOWN_POLICY_EXTRA: [&str; 3] = ["read", "write", "execute"];
const KNOWN_SHUTDOWN: [&str; 1] = ["allowedUserIds"];
const KNOWN_WEB: [&str; 4] = ["host", "port", "observer", "publicUrl"];
const KNOWN_OUTPUT: [&str; 5] = [
    "forwardToolOutput",
    "maxToolOutputChars",
    "maxAttachmentBytes",
    "maxAttachmentsPerMessage",
    "postDiffs",
];
const KNOWN_LIMITS: [&str; 4] = [
    "maxConcurrentTurns",
    "maxLiveSessions",
    "maxQueueLength",
    "maxQueueWaitMs",
];
const KNOWN_TIMEOUTS: [&str; 4] = ["idleMs", "startupMs", "questionMs", "abortMs"];

/// Collects reasons so that a first run reports all of them at once.
#[derive(Default)]
struct Problems {
    found: Vec<String>,
}

impl Problems {
    fn add(&mut self, problem: impl Into<String>) {
        self.found.push(problem.into());
    }
}

/// The section of that name, or an empty one when it is not an object.
fn section(source: &Map<String, Value>, name: &str) -> Map<String, Value> {
    match source.get(name) {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

fn reject_unknown(
    source: &Map<String, Value>,
    known: &[&str],
    place: &str,
    problems: &mut Problems,
) {
    for key in source.keys() {
        if !known.contains(&key.as_str()) {
            problems.add(format!(
                "{place}.{key} is not a setting; check the spelling"
            ));
        }
    }
}

fn required_string(
    source: &Map<String, Value>,
    key: &str,
    place: &str,
    problems: &mut Problems,
) -> String {
    match source.get(key) {
        Some(Value::String(value)) if !value.trim().is_empty() => value.trim().to_owned(),
        _ => {
            problems.add(format!(
                "{place}.{key} is required and must be a non-empty string"
            ));
            String::new()
        }
    }
}

fn optional_string(
    source: &Map<String, Value>,
    key: &str,
    place: &str,
    problems: &mut Problems,
) -> Option<String> {
    match source.get(key) {
        None => None,
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.trim().to_owned()),
        _ => {
            problems.add(format!(
                "{place}.{key} must be a non-empty string when it is set"
            ));
            None
        }
    }
}

fn id_list(
    source: &Map<String, Value>,
    key: &str,
    place: &str,
    problems: &mut Problems,
) -> Vec<String> {
    let Some(Value::Array(entries)) = source.get(key) else {
        if source.get(key).is_some() {
            problems.add(format!("{place}.{key} must be a list of account ids"));
        }
        return Vec::new();
    };
    let mut ids = Vec::new();
    for entry in entries {
        match entry {
            Value::String(id) if !id.trim().is_empty() => ids.push(id.trim().to_owned()),
            _ => problems.add(format!(
                "{place}.{key} contains an entry that is not an account id"
            )),
        }
    }
    ids
}

fn positive(
    source: &Map<String, Value>,
    key: &str,
    fallback: f64,
    place: &str,
    problems: &mut Problems,
) -> f64 {
    match source.get(key) {
        None => fallback,
        Some(value) => match value.as_f64() {
            Some(amount) if amount > 0.0 => amount,
            _ => {
                problems.add(format!("{place}.{key} must be a number greater than zero"));
                fallback
            }
        },
    }
}

/// The integer widths a counted setting is kept as: widened for the check,
/// narrowed after it.
trait Positive: Copy {
    /// The setting as the number the check runs on.
    fn amount(self) -> f64;

    /// The checked amount as the whole number the configuration carries.
    fn from_amount(amount: f64) -> Self;
}

impl Positive for u16 {
    fn amount(self) -> f64 {
        f64::from(self)
    }
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn from_amount(amount: f64) -> Self {
        amount as u16
    }
}

impl Positive for u32 {
    fn amount(self) -> f64 {
        f64::from(self)
    }
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn from_amount(amount: f64) -> Self {
        amount as u32
    }
}

impl Positive for u64 {
    #[expect(clippy::cast_precision_loss)]
    fn amount(self) -> f64 {
        self as f64
    }
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn from_amount(amount: f64) -> Self {
        amount as u64
    }
}

impl Positive for usize {
    #[expect(clippy::cast_precision_loss)]
    fn amount(self) -> f64 {
        self as f64
    }
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn from_amount(amount: f64) -> Self {
        amount as usize
    }
}

/// A positive setting that counts something, taken as the whole number the
/// resolved configuration carries.
fn positive_as<P: Positive>(
    source: &Map<String, Value>,
    key: &str,
    fallback: P,
    place: &str,
    problems: &mut Problems,
) -> P {
    P::from_amount(positive(source, key, fallback.amount(), place, problems))
}

fn size(
    source: &Map<String, Value>,
    key: &str,
    fallback: &str,
    place: &str,
    problems: &mut Problems,
) -> String {
    match source.get(key) {
        None => fallback.to_owned(),
        Some(Value::String(value)) if parse_size(value).is_some() => value.trim().to_owned(),
        _ => {
            problems.add(format!("{place}.{key} must be a size such as 512m or 4g"));
            fallback.to_owned()
        }
    }
}

/// Reads a list of outbound ports.
///
/// A port is a whole number in the range a socket accepts, so anything outside
/// it is refused rather than clamped. An empty list would leave a session that
/// has a network unable to open anything, which is a mistake worth naming.
fn ports(
    source: &Map<String, Value>,
    key: &str,
    fallback: Vec<u16>,
    place: &str,
    problems: &mut Problems,
) -> Vec<u16> {
    let Some(Value::Array(entries)) = source.get(key) else {
        if source.get(key).is_some() {
            problems.add(format!("{place}.{key} must be a list of port numbers"));
        }
        return fallback;
    };
    let mut found = Vec::new();
    for entry in entries {
        let in_range = entry
            .as_f64()
            .is_some_and(|p| p.fract() == 0.0 && (1.0..=65_535.0).contains(&p));
        if in_range {
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let port = entry.as_f64().unwrap_or_default() as u16;
            found.push(port);
        } else {
            problems.add(format!(
                "{place}.{key} contains {}, which is not a port between 1 and 65535",
                render_value(entry)
            ));
        }
    }
    if found.is_empty() {
        problems.add(format!(
            "{place}.{key} names no port; remove it for the default, or name one"
        ));
        return fallback;
    }
    found
}

fn flag(
    source: &Map<String, Value>,
    key: &str,
    fallback: bool,
    place: &str,
    problems: &mut Problems,
) -> bool {
    match source.get(key) {
        None => fallback,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            problems.add(format!("{place}.{key} must be true or false"));
            fallback
        }
    }
}

/// A choice among written names: absent reads as the default, and a written
/// value that is not one of the names is a problem the default cannot fix.
fn one_of<T>(
    source: &Map<String, Value>,
    key: &str,
    parse: impl Fn(&str) -> Option<T>,
    names: &str,
    default: T,
    place: &str,
    problems: &mut Problems,
) -> T {
    let written = source.get(key);
    let chosen = written.and_then(Value::as_str).and_then(parse);
    match (written, chosen) {
        (_, Some(chosen)) => chosen,
        (None, _) => default,
        (Some(_), None) => {
            problems.add(format!("{place}.{key} must be one of {names}"));
            default
        }
    }
}

/// Whether one path is the other, or sits inside it.
///
/// Compared by component, so `/srv/work` does not read as containing
/// `/srv/workspace`. Both are already resolved and absolute here.
fn nests(one: &str, other: &str) -> bool {
    let one = std::path::Path::new(one);
    let other = std::path::Path::new(other);
    one.starts_with(other) || other.starts_with(one)
}

fn directory(source: &Map<String, Value>, key: &str, problems: &mut Problems) -> String {
    match source.get(key) {
        Some(Value::String(value)) if !value.trim().is_empty() => {
            let path = value.trim();
            if is_absolute(path) {
                resolve(path)
            } else {
                problems.add(format!("{key} must be an absolute path, got {path}"));
                String::new()
            }
        }
        _ => {
            problems.add(format!("{key} is required and must be an absolute path"));
            String::new()
        }
    }
}

/// An optional path that must be absolute when it is given at all.
///
/// Absolute because the daemon's working directory is not the operator's, so a
/// relative path names a different file depending on where the daemon was
/// started. Whether the file is actually there is checked at startup, where a
/// refusal can say so; this stays free of the filesystem so it remains a pure
/// reading of the configuration.
fn optional_absolute_path(
    source: &Map<String, Value>,
    key: &str,
    place: &str,
    problems: &mut Problems,
) -> Option<String> {
    match source.get(key) {
        None => None,
        Some(Value::String(value)) if !value.trim().is_empty() => {
            let path = value.trim();
            if is_absolute(path) {
                Some(resolve(path))
            } else {
                problems.add(format!(
                    "{place}.{key} must be an absolute path, got {path}"
                ));
                None
            }
        }
        _ => {
            problems.add(format!("{place}.{key} must be an absolute path"));
            None
        }
    }
}

/// Reads a list of absolute paths.
///
/// A relative path in a policy is meaningless: there is no working directory to
/// resolve it against once the session has pivoted, so it is refused rather
/// than resolved against whatever the daemon happened to be started from.
fn path_list(
    source: &Map<String, Value>,
    key: &str,
    place: &str,
    problems: &mut Problems,
) -> Vec<String> {
    let Some(Value::Array(entries)) = source.get(key) else {
        if source.get(key).is_some() {
            problems.add(format!("{place}.{key} must be a list of absolute paths"));
        }
        return Vec::new();
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Value::String(value) if !value.trim().is_empty() => {
                let path = value.trim();
                if is_absolute(path) {
                    paths.push(resolve(path));
                } else {
                    problems.add(format!(
                        "{place}.{key} entry {path} must be an absolute path"
                    ));
                }
            }
            _ => problems.add(format!(
                "{place}.{key} contains an entry that is not a path"
            )),
        }
    }
    paths
}

fn validate_chat(raw: &Map<String, Value>, problems: &mut Problems) -> ChatConfig {
    let source = section(raw, "chat");
    reject_unknown(&source, &KNOWN_CHAT, "chat", problems);

    let allowed = id_list(&source, "allowedUserIds", "chat", problems);
    if allowed.is_empty() {
        problems.add(
            "chat.allowedUserIds is required and must list at least one account; there is no \
             allow-everyone default",
        );
    }

    ChatConfig {
        token: required_string(&source, "token", "chat", problems),
        channel_id: required_string(&source, "channelId", "chat", problems),
        allowed_user_ids: allowed,
        blocked_user_ids: id_list(&source, "blockedUserIds", "chat", problems),
        operator_user_ids: id_list(&source, "operatorUserIds", "chat", problems),
        start_on_mention: flag(
            &source,
            "startOnMention",
            schema::defaults::START_ON_MENTION,
            "chat",
            problems,
        ),
    }
}

/// Reads the operator's provider definitions, which are passed through as they
/// are written.
///
/// Each value is handed to the agent unread, so only the shape this depends on
/// is checked: a name mapping to an object. Naming the fields here would mean
/// refusing one the agent had just learned, and this is not the schema's owner.
fn validate_providers(source: &Map<String, Value>, problems: &mut Problems) -> Map<String, Value> {
    let Some(raw) = source.get("providers") else {
        return Map::new();
    };
    let Some(definitions) = raw.as_object() else {
        problems.add("agent.providers must be an object of provider definitions, keyed by name");
        return Map::new();
    };

    let mut defined = Map::new();
    for (name, definition) in definitions {
        if name.trim().is_empty() {
            problems.add("agent.providers has a definition with no name");
            continue;
        }
        if !definition.is_object() {
            problems.add(format!(
                "agent.providers.{name} must be an object describing the provider"
            ));
            continue;
        }
        defined.insert(name.trim().to_owned(), definition.clone());
    }
    defined
}

/// Reads the short names for models, which must be names standing for text.
fn validate_aliases(
    source: &Map<String, Value>,
    problems: &mut Problems,
) -> BTreeMap<String, String> {
    let Some(raw) = source.get("aliases") else {
        return BTreeMap::new();
    };
    let Some(entries) = raw.as_object() else {
        problems.add("agent.aliases must be an object of short names to models");
        return BTreeMap::new();
    };

    let mut aliases = BTreeMap::new();
    for (name, target) in entries {
        let short = name.trim();
        if short.is_empty() {
            problems.add("agent.aliases has a name that is empty");
            continue;
        }
        // A name with a colon in it could never be typed: the colon is where
        // the thinking level starts, so the name would be read as ending
        // before it.
        if short.contains(':') {
            problems.add(format!(
                "agent.aliases.{name} cannot hold a colon, which starts the thinking level"
            ));
            continue;
        }
        match target {
            Value::String(text) if !text.trim().is_empty() => {
                aliases.insert(short.to_owned(), text.trim().to_owned());
            }
            _ => problems.add(format!("agent.aliases.{name} must name a model")),
        }
    }
    aliases
}

fn validate_agent(raw: &Map<String, Value>, problems: &mut Problems) -> AgentConfig {
    let source = section(raw, "agent");
    reject_unknown(&source, &KNOWN_AGENT, "agent", problems);

    // Said before the unknown-key refusal would call them typos: they were
    // real keys until a provider became one thing described in one place.
    for moved in ["credential", "credentialName"] {
        if source.contains_key(moved) {
            let provider = source
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("<provider>");
            problems.add(format!(
                "agent.{moved} has moved into the provider it belongs to. Write it as \
                 agent.providers.{provider}.{moved} instead"
            ));
        }
    }

    let agent = AgentConfig {
        provider: required_string(&source, "provider", "agent", problems),
        model: optional_string(&source, "model", "agent", problems),
        vision_model: optional_string(&source, "visionModel", "agent", problems),
        delegate: validate_delegate(&source, problems),
        rules_path: optional_absolute_path(&source, "rulesPath", "agent", problems),
        providers: validate_providers(&source, problems),
        extensions: validate_extensions(&source, problems),
        aliases: validate_aliases(&source, problems),
    };

    // A provider a session starts on that nothing describes is a session that
    // cannot reach a model, which is worth saying here rather than at launch.
    if !agent.provider.is_empty() && !agent.providers.contains_key(&agent.provider) {
        let named = agent
            .providers
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        problems.add(if named.is_empty() {
            format!(
                "agent.provider is {} but agent.providers describes none",
                agent.provider
            )
        } else {
            format!(
                "agent.provider is {} but agent.providers describes only {named}",
                agent.provider
            )
        });
    }
    // Only the one a session starts on has to be reachable. Another may be
    // described without a credential: the agent is given the description and
    // no route, which is the operator's business rather than a refusal.
    if !agent.provider.is_empty()
        && agent.providers.contains_key(&agent.provider)
        && !agent.is_extension_provider(&agent.provider)
        && agent.credential_of(&agent.provider).is_none()
    {
        problems.add(format!(
            "agent.providers.{}.credential is required: it is the provider a session starts on",
            agent.provider
        ));
    }
    agent
}

/// Reads the delegation settings, which the whole section may omit.
///
/// Naming no model is the same as having no section: there is nothing to ask,
/// so nothing is offered to the agent.
fn validate_delegate(
    source: &Map<String, Value>,
    problems: &mut Problems,
) -> Option<DelegateConfig> {
    source.get("delegate")?;
    let section = section(source, "delegate");
    reject_unknown(&section, &KNOWN_DELEGATE, "agent.delegate", problems);

    Some(DelegateConfig {
        model: required_string(&section, "model", "agent.delegate", problems),
        per_turn: positive_as(
            &section,
            "perTurn",
            schema::defaults::DELEGATE_PER_TURN,
            "agent.delegate",
            problems,
        ),
        deadline_ms: positive_as(
            &section,
            "deadlineMs",
            schema::defaults::DELEGATE_DEADLINE_MS,
            "agent.delegate",
            problems,
        ),
        base_url: optional_string(&section, "baseUrl", "agent.delegate", problems),
    })
}

/// Reads the GitHub identity, which the whole section may omit.
///
/// Present but incomplete is a problem rather than a partial identity: a
/// session that pushes as half of somebody is worse than one that cannot push.
fn validate_github(raw: &Map<String, Value>, problems: &mut Problems) -> Option<GithubConfig> {
    raw.get("github")?;
    let source = section(raw, "github");
    reject_unknown(&source, &KNOWN_GITHUB, "github", problems);

    Some(GithubConfig {
        token: required_string(&source, "token", "github", problems),
        user_name: required_string(&source, "userName", "github", problems),
        user_email: required_string(&source, "userEmail", "github", problems),
    })
}

/// Reads the paths granted on top of the generated policy, if any.
fn validate_policy_extra(
    source: &Map<String, Value>,
    problems: &mut Problems,
) -> Option<PolicyExtraConfig> {
    source.get("policyExtra")?;
    let extra = section(source, "policyExtra");
    reject_unknown(&extra, &KNOWN_POLICY_EXTRA, "sandbox.policyExtra", problems);

    let read = path_list(&extra, "read", "sandbox.policyExtra", problems);
    let write = path_list(&extra, "write", "sandbox.policyExtra", problems);
    let execute = path_list(&extra, "execute", "sandbox.policyExtra", problems);

    if read.is_empty() && write.is_empty() && execute.is_empty() {
        problems.add("sandbox.policyExtra is set but grants nothing; remove it, or name a path");
    }
    Some(PolicyExtraConfig {
        read,
        write,
        execute,
    })
}

/// Reads the directories added to a session's PATH, if any.
/// Reads the pi extension directories to place in every session, if any.
///
/// Each is an absolute host directory. A session cannot see the host's own pi
/// configuration, so an extension is only loaded when its directory is copied
/// into the session; naming it here is what asks for that.
fn validate_extensions(source: &Map<String, Value>, problems: &mut Problems) -> Vec<String> {
    if source.get("extensions").is_none() {
        return Vec::new();
    }
    path_list(source, "extensions", "agent", problems)
}

fn validate_path_extra(
    source: &Map<String, Value>,
    problems: &mut Problems,
) -> Option<Vec<String>> {
    source.get("pathExtra")?;
    let paths = path_list(source, "pathExtra", "sandbox", problems);
    if paths.is_empty() && problems.found.is_empty() {
        problems.add("sandbox.pathExtra is set but names nothing; remove it, or name a directory");
    }
    Some(paths)
}

/// Reads the variables set in every session's environment, if any.
///
/// `PATH` and `HOME` are refused rather than merged: the policy sets both to
/// paths it places, and a session pointed at anything else would be naming
/// paths no grant covers.
fn validate_sandbox_env(
    source: &Map<String, Value>,
    problems: &mut Problems,
) -> Option<BTreeMap<String, String>> {
    source.get("env")?;
    let entries = section(source, "env");

    let mut env = BTreeMap::new();
    for (name, value) in &entries {
        if !is_env_name(name) {
            problems.add(format!("sandbox.env.{name} is not a variable name"));
            continue;
        }
        if SET_BY_THE_POLICY.contains(&name.as_str()) {
            problems.add(format!(
                "sandbox.env must not set {name}, which the policy sets itself"
            ));
            continue;
        }
        match value {
            Value::String(text) => {
                env.insert(name.clone(), text.clone());
            }
            _ => problems.add(format!("sandbox.env.{name} must be a string")),
        }
    }

    if env.is_empty() && problems.found.is_empty() {
        problems.add("sandbox.env is set but names nothing; remove it, or name a variable");
    }
    Some(env)
}

fn validate_sandbox(raw: &Map<String, Value>, problems: &mut Problems) -> SandboxConfig {
    let source = section(raw, "sandbox");
    reject_unknown(&source, &KNOWN_SANDBOX, "sandbox", problems);

    let backend = one_of(
        &source,
        "backend",
        parse_backend,
        "podman, bailey",
        schema::defaults::BACKEND,
        "sandbox",
        problems,
    );
    let network = one_of(
        &source,
        "network",
        parse_network,
        "restricted, none",
        schema::defaults::NETWORK,
        "sandbox",
        problems,
    );

    SandboxConfig {
        backend,
        require_full_enforcement: flag(
            &source,
            "requireFullEnforcement",
            schema::defaults::REQUIRE_FULL_ENFORCEMENT,
            "sandbox",
            problems,
        ),
        network,
        egress_ports: ports(
            &source,
            "egressPorts",
            Vec::from(schema::defaults::EGRESS_PORTS),
            "sandbox",
            problems,
        ),
        egress: validate_egress(&source, problems),
        hide_host_address: flag(
            &source,
            "hideHostAddress",
            schema::defaults::HIDE_HOST_ADDRESS,
            "sandbox",
            problems,
        ),
        image: optional_string(&source, "image", "sandbox", problems)
            .unwrap_or_else(|| schema::defaults::IMAGE.to_owned()),
        memory: size(
            &source,
            "memory",
            schema::defaults::MEMORY,
            "sandbox",
            problems,
        ),
        cpus: positive(&source, "cpus", schema::defaults::CPUS, "sandbox", problems),
        pids: positive_as(&source, "pids", schema::defaults::PIDS, "sandbox", problems),
        file_max: size(
            &source,
            "fileMax",
            schema::defaults::FILE_MAX,
            "sandbox",
            problems,
        ),
        disk: size(&source, "disk", schema::defaults::DISK, "sandbox", problems),
        disk_check_ms: positive_as(
            &source,
            "diskCheckMs",
            schema::defaults::DISK_CHECK_MS,
            "sandbox",
            problems,
        ),
        grace_period_ms: positive_as(
            &source,
            "gracePeriodMs",
            schema::defaults::GRACE_PERIOD_MS,
            "sandbox",
            problems,
        ),
        policy_extra: validate_policy_extra(&source, problems),
        path_extra: validate_path_extra(&source, problems),
        env: validate_sandbox_env(&source, problems),
    }
}

/// The egress bounding, defaulting to the brokered pass-through.
///
/// An allowlist entry that is not a hostname is refused rather than passed to
/// the broker, since a broker told to permit a malformed host either permits
/// nothing or, worse, permits more than was meant.
fn validate_egress(source: &Map<String, Value>, problems: &mut Problems) -> EgressConfig {
    let Some(raw) = source.get("egress") else {
        return default_egress();
    };
    let Some(egress) = raw.as_object() else {
        problems.add("sandbox.egress must be an object with mode and allow");
        return default_egress();
    };
    reject_unknown(egress, &KNOWN_EGRESS, "sandbox.egress", problems);

    let mode = one_of(
        egress,
        "mode",
        parse_egress_mode,
        "open, proxy",
        schema::defaults::EGRESS_MODE,
        "sandbox.egress",
        problems,
    );

    let mut allow = Vec::new();
    match egress.get("allow") {
        None => {}
        Some(Value::Array(entries)) => {
            for entry in entries {
                match entry {
                    Value::String(text) => {
                        let value = text.trim();
                        if value == "*" {
                            allow.push(value.to_owned());
                        } else if is_egress_host(value) {
                            allow.push(value.to_lowercase());
                        } else {
                            problems.add(format!(
                                "sandbox.egress.allow entry {} is not a hostname",
                                json_of(entry)
                            ));
                        }
                    }
                    other => problems.add(format!(
                        "sandbox.egress.allow entry {} is not a hostname",
                        json_of(other)
                    )),
                }
            }
        }
        Some(_) => problems.add("sandbox.egress.allow must be a list of hostnames"),
    }

    let allow_internal = match egress.get("allowInternal") {
        None => schema::defaults::EGRESS_ALLOW_INTERNAL,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            problems.add("sandbox.egress.allowInternal must be true or false");
            schema::defaults::EGRESS_ALLOW_INTERNAL
        }
    };

    EgressConfig {
        mode,
        allow,
        allow_internal,
    }
}

fn default_egress() -> EgressConfig {
    EgressConfig {
        mode: schema::defaults::EGRESS_MODE,
        allow: schema::defaults::EGRESS_ALLOW
            .iter()
            .map(|host| (*host).to_owned())
            .collect(),
        allow_internal: schema::defaults::EGRESS_ALLOW_INTERNAL,
    }
}

fn validate_output(raw: &Map<String, Value>, problems: &mut Problems) -> OutputConfig {
    let source = section(raw, "output");
    reject_unknown(&source, &KNOWN_OUTPUT, "output", problems);

    OutputConfig {
        forward_tool_output: flag(
            &source,
            "forwardToolOutput",
            schema::defaults::FORWARD_TOOL_OUTPUT,
            "output",
            problems,
        ),
        max_tool_output_chars: positive_as(
            &source,
            "maxToolOutputChars",
            schema::defaults::MAX_TOOL_OUTPUT_CHARS,
            "output",
            problems,
        ),
        max_attachment_bytes: positive_as(
            &source,
            "maxAttachmentBytes",
            schema::defaults::MAX_ATTACHMENT_BYTES,
            "output",
            problems,
        ),
        max_attachments_per_message: positive_as(
            &source,
            "maxAttachmentsPerMessage",
            schema::defaults::MAX_ATTACHMENTS_PER_MESSAGE,
            "output",
            problems,
        ),
        post_diffs: flag(
            &source,
            "postDiffs",
            schema::defaults::POST_DIFFS,
            "output",
            problems,
        ),
    }
}

/// Reads who may power off the host.
///
/// An absent section means nobody, which is the safe reading of silence for a
/// command that acts on the machine.
fn validate_shutdown(raw: &Map<String, Value>, problems: &mut Problems) -> ShutdownConfig {
    let source = section(raw, "shutdown");
    reject_unknown(&source, &KNOWN_SHUTDOWN, "shutdown", problems);
    ShutdownConfig {
        allowed_user_ids: id_list(&source, "allowedUserIds", "shutdown", problems),
    }
}

/// Reads the interface's settings, when one is configured at all.
///
/// The address is not checked here. Whether it is one worth serving over is
/// the interface's own rule, and it is applied where the listener is opened so
/// that a refusal names the listener.
fn validate_web(raw: &Map<String, Value>, problems: &mut Problems) -> Option<WebConfig> {
    raw.get("web")?;
    let source = section(raw, "web");
    reject_unknown(&source, &KNOWN_WEB, "web", problems);

    Some(WebConfig {
        host: optional_string(&source, "host", "web", problems)
            .unwrap_or_else(|| schema::defaults::WEB_HOST.to_owned()),
        port: positive_as(&source, "port", schema::defaults::WEB_PORT, "web", problems),
        observer: flag(
            &source,
            "observer",
            schema::defaults::WEB_OBSERVER,
            "web",
            problems,
        ),
        public_url: optional_string(&source, "publicUrl", "web", problems),
    })
}

fn validate_limits(raw: &Map<String, Value>, problems: &mut Problems) -> LimitsConfig {
    let source = section(raw, "limits");
    reject_unknown(&source, &KNOWN_LIMITS, "limits", problems);

    LimitsConfig {
        max_concurrent_turns: positive_as(
            &source,
            "maxConcurrentTurns",
            schema::defaults::MAX_CONCURRENT_TURNS,
            "limits",
            problems,
        ),
        max_live_sessions: positive_as(
            &source,
            "maxLiveSessions",
            schema::defaults::MAX_LIVE_SESSIONS,
            "limits",
            problems,
        ),
        max_queue_length: positive_as(
            &source,
            "maxQueueLength",
            schema::defaults::MAX_QUEUE_LENGTH,
            "limits",
            problems,
        ),
        max_queue_wait_ms: positive_as(
            &source,
            "maxQueueWaitMs",
            schema::defaults::MAX_QUEUE_WAIT_MS,
            "limits",
            problems,
        ),
    }
}

fn validate_timeouts(raw: &Map<String, Value>, problems: &mut Problems) -> TimeoutsConfig {
    let source = section(raw, "timeouts");
    reject_unknown(&source, &KNOWN_TIMEOUTS, "timeouts", problems);

    TimeoutsConfig {
        idle_ms: positive_as(
            &source,
            "idleMs",
            schema::defaults::IDLE_MS,
            "timeouts",
            problems,
        ),
        startup_ms: positive_as(
            &source,
            "startupMs",
            schema::defaults::STARTUP_MS,
            "timeouts",
            problems,
        ),
        question_ms: positive_as(
            &source,
            "questionMs",
            schema::defaults::QUESTION_MS,
            "timeouts",
            problems,
        ),
        abort_ms: positive_as(
            &source,
            "abortMs",
            schema::defaults::ABORT_MS,
            "timeouts",
            problems,
        ),
    }
}

/// Validates a parsed configuration file.
///
/// Returns a [`ConfigError`] carrying every problem found, so that a first run
/// is fixed in one pass rather than one message at a time.
pub fn validate_config(parsed: &Value) -> Result<Config, ConfigError> {
    let mut problems = Problems::default();
    let Some(raw) = parsed.as_object() else {
        return Err(ConfigError {
            problems: vec!["the configuration file must contain a JSON object".to_owned()],
        });
    };
    reject_unknown(raw, &KNOWN_ROOT, "config", &mut problems);

    let chat = validate_chat(raw, &mut problems);
    let agent = validate_agent(raw, &mut problems);
    let github = validate_github(raw, &mut problems);
    let project_root = directory(raw, "projectRoot", &mut problems);
    let state_dir = directory(raw, "stateDir", &mut problems);
    let sandbox = validate_sandbox(raw, &mut problems);
    let output = validate_output(raw, &mut problems);
    let shutdown = validate_shutdown(raw, &mut problems);
    let web = validate_web(raw, &mut problems);
    let limits = validate_limits(raw, &mut problems);
    let timeouts = validate_timeouts(raw, &mut problems);

    // Asked once both sections are read, since the name is the operator's own.
    // Shadowing it would authenticate the agent with whatever was set here.
    if let Some(env) = sandbox.env.as_ref() {
        for name in agent.credential_names() {
            if env.contains_key(name) {
                problems.add(format!(
                    "sandbox.env must not set {name}, which carries a provider credential"
                ));
            }
        }
    }

    if !project_root.is_empty() && !state_dir.is_empty() && nests(&project_root, &state_dir) {
        problems.add(
            "projectRoot and stateDir must be separate directories, neither inside the other: a \
             session may write both its project and its state, and the transcript is kept beside \
             the state directory precisely so it cannot",
        );
    }

    if problems.found.is_empty() {
        Ok(Config {
            chat,
            agent,
            github,
            project_root,
            state_dir,
            sandbox,
            output,
            shutdown,
            web,
            limits,
            timeouts,
        })
    } else {
        Err(ConfigError {
            problems: problems.found,
        })
    }
}

/// A hostname the broker may be told to permit: a name, optionally
/// `*.`-prefixed, with at least two labels.
fn is_egress_host(value: &str) -> bool {
    fn label_ok(label: &str) -> bool {
        let bytes = label.as_bytes();
        match bytes.first() {
            None => false,
            Some(first) if !first.is_ascii_alphanumeric() => false,
            // A single character is both the first and the last.
            Some(_) if bytes.len() == 1 => true,
            Some(_) => {
                bytes.len() <= 63
                    && bytes[bytes.len() - 1].is_ascii_alphanumeric()
                    && bytes[1..bytes.len() - 1]
                        .iter()
                        .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
            }
        }
    }

    let rest = value.strip_prefix("*.").unwrap_or(value);
    let mut labels = rest.split('.');
    if !labels.next().is_some_and(label_ok) {
        return false;
    }
    let mut count = 1;
    for label in labels {
        if !label_ok(label) {
            return false;
        }
        count += 1;
    }
    count >= 2
}

fn parse_backend(value: &str) -> Option<SandboxBackend> {
    match value {
        "podman" => Some(SandboxBackend::Podman),
        "bailey" => Some(SandboxBackend::Bailey),
        _ => None,
    }
}

fn parse_network(value: &str) -> Option<NetworkMode> {
    match value {
        "restricted" => Some(NetworkMode::Restricted),
        "none" => Some(NetworkMode::None),
        _ => None,
    }
}

fn parse_egress_mode(value: &str) -> Option<EgressMode> {
    match value {
        "open" => Some(EgressMode::Open),
        "proxy" => Some(EgressMode::Proxy),
        _ => None,
    }
}

/// What a variable may be called, which is what a shell would accept.
fn is_env_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    match bytes.first() {
        None => false,
        Some(first) => {
            (first.is_ascii_alphabetic() || *first == b'_')
                && bytes[1..]
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
        }
    }
}

/// The value as an operator message would say it, which is what a string
/// interpolation did in the text this replaces.
fn render_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(render_value).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

fn json_of(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| render_value(value))
}

#[cfg(test)]
mod tests;
