//! Turns whatever was on disk into a [`Config`], or refuses with reasons.
//!
//! Every problem is collected before anything is returned, and an unknown key
//! is a problem rather than something quietly ignored: a misspelled setting
//! that takes no effect is worse than one that is rejected, because the daemon
//! then runs with a guarantee somebody believes they configured.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{is_absolute, resolve};
use crate::config::discover::Discovery;
use crate::config::duration::parse_duration;
use crate::config::redact::MIN_SCRUBBABLE_LENGTH;
use crate::config::route::{Placement, RoutePolicy};
use crate::config::schema::{
    self, AgentConfig, ChatConfig, Config, ConfigError, DelegateConfig, EgressConfig, EgressMode,
    GithubAppConfig, GithubConfig, GithubTrigger, LimitsConfig, NetworkMode, OutputConfig,
    PluginConfig, PolicyExtraConfig, RetentionConfig, SandboxBackend, SandboxConfig,
    ShutdownConfig, TimeoutsConfig, WebConfig, WebLoginConfig,
};
use crate::config::size::parse_size;
use crate::config::usage::UsageShape;

/// Variables the policy sets itself, and so refuses to take from a file.
const SET_BY_THE_POLICY: [&str; 2] = ["PATH", "HOME"];

const KNOWN_ROOT: [&str; 13] = [
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
    "retention",
];
const KNOWN_RETENTION: [&str; 3] = ["enabled", "olderThan", "every"];
const KNOWN_CHAT: [&str; 6] = [
    "token",
    "channelId",
    "allowedUserIds",
    "blockedUserIds",
    "operatorUserIds",
    "startOnMention",
];
const KNOWN_AGENT: [&str; 16] = [
    "provider",
    "model",
    "visionModel",
    "imageGen",
    "speechGen",
    "delegate",
    "rulesPath",
    "providers",
    "plugins",
    "aliases",
    "fallback",
    "deny",
    "deniedTools",
    // Known so that finding one is answered with where it went, rather than
    // with the spelling check an actual typo gets.
    "credential",
    "credentialName",
    "extensions",
];
const KNOWN_DELEGATE: [&str; 4] = ["model", "perTurn", "deadlineMs", "baseUrl"];
const KNOWN_GITHUB: [&str; 5] = ["token", "userName", "userEmail", "trigger", "app"];
const KNOWN_SANDBOX: [&str; 20] = [
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
    "tmpSize",
    "shmSize",
    "diskTmp",
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
const KNOWN_WEB: [&str; 5] = ["host", "port", "observer", "publicUrl", "login"];
const KNOWN_WEB_LOGIN: [&str; 2] = ["clientId", "clientSecret"];
const KNOWN_OUTPUT: [&str; 8] = [
    "forwardToolOutput",
    "maxToolOutputChars",
    "maxAttachmentBytes",
    "maxAttachmentsPerMessage",
    "maxUploadBytes",
    "postDiffs",
    "postImages",
    "postAudio",
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

/// A required token, which must also be long enough to be scrubbed from what
/// is posted: a shorter one would pass through every redaction unnoticed.
fn required_token(source: &Map<String, Value>, place: &str, problems: &mut Problems) -> String {
    required_secret(source, "token", place, problems)
}

/// A secret that must be present and long enough to be scrubbed: every
/// scrubber skips a value under [`MIN_SCRUBBABLE_LENGTH`] bytes, which would
/// otherwise be posted as it is.
fn required_secret(
    source: &Map<String, Value>,
    key: &str,
    place: &str,
    problems: &mut Problems,
) -> String {
    let secret = required_string(source, key, place, problems);
    if !secret.is_empty() && secret.len() < MIN_SCRUBBABLE_LENGTH {
        problems.add(format!(
            "{place}.{key} is shorter than {MIN_SCRUBBABLE_LENGTH} bytes, too short to be \
             scrubbed from what is posted, so use the real one"
        ));
    }
    secret
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
/// Where a path really is: through every link when it exists, and with
/// `.` and `..` worked out when it does not.
fn real_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    std::fs::canonicalize(path).map_or_else(
        |_| resolve(path),
        |real| real.to_string_lossy().into_owned(),
    )
}

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
        token: required_token(&source, "chat", problems),
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

/// Refuses a provider id kage would not accept: anything but lowercase ASCII
/// letters, digits and dashes. kage refuses the whole configuration over one
/// id it cannot take, which would end every session rather than the one
/// provider. A provider a plugin registers is never written into kage's
/// configuration, so kage never reads its id.
fn validate_provider_id(name: &str, definition: &Value, problems: &mut Problems) {
    let id = name.trim();
    let takes = !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !takes && definition.get("extension").and_then(Value::as_bool) != Some(true) {
        problems.add(format!(
            "agent.providers.{name} must be named with lowercase letters, digits and dashes \
             only, which is all kage accepts in a provider id"
        ));
    }
}

/// The keys of a provider definition that hold its brokered route back.
const ROUTE_KEYS: [&str; 3] = ["placement", "capabilities", "strictCapabilities"];

/// The route key a written one stands for by starting with the word the key
/// turns on: `strict` for `strictCapabilities`, `place` for `placement`, and
/// `capab` for `capabilities`, as `strictness`, `place` or `capability` do.
///
/// The whole word, not merely the same few letters, since an operator's own
/// key may begin like a route key by chance, as `platform` or `capacity` do,
/// and such a key is not a misspelling.
fn shortened(written: &str) -> Option<&'static str> {
    let written = written.to_ascii_lowercase();
    [
        ("strict", "strictCapabilities"),
        ("place", "placement"),
        ("capab", "capabilities"),
    ]
    .into_iter()
    .find(|(stem, _)| written.starts_with(stem))
    .map(|(_, key)| key)
}

/// Whether `written` reads as a misspelling of `key`: the same apart from
/// case, or at most two letters added, dropped or changed.
fn near_miss(written: &str, key: &str) -> bool {
    let (written, key) = (written.to_ascii_lowercase(), key.to_ascii_lowercase());
    let (a, b): (Vec<char>, Vec<char>) = (written.chars().collect(), key.chars().collect());
    if a.len().abs_diff(b.len()) > 2 {
        return false;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, left) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, right) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(left != right));
            diagonal = above;
        }
    }
    row[b.len()] <= 2
}

/// Checks what a provider's brokered route allows, and that it is set only on
/// a provider the broker fronts.
fn validate_route(name: &str, definition: &Value, problems: &mut Problems) {
    // A definition may carry keys errand does not read, so an unknown one is
    // not refused in general. One that reads as a misspelling of a key that
    // holds a route back is, since leaving that key out quietly lets
    // everything through.
    if let Some(fields) = definition.as_object() {
        for written in fields.keys() {
            if ROUTE_KEYS.contains(&written.as_str()) {
                continue;
            }
            if let Some(meant) = ROUTE_KEYS
                .into_iter()
                .find(|key| near_miss(written, key))
                .or_else(|| shortened(written))
            {
                problems.add(format!(
                    "agent.providers.{name}.{written} is not something errand reads; did you \
                     mean {meant}? Left as it is, the route is not held to it"
                ));
            }
        }
    }
    let brokered = schema::definition_is_brokered(Some(definition));
    match RoutePolicy::of(name, definition) {
        Err(found) => {
            for problem in found {
                problems.add(problem);
            }
        }
        // The media tools borrow the provider's route and always send their
        // key as a bearer token, so a route that takes it in x-api-key alone
        // would refuse every one of their calls. A placement derived from
        // `kind: anthropic` is not checked here, since media on such a
        // provider is refused for its kind already.
        Ok(policy)
            if brokered
                && policy.placement == Placement::ApiKey
                && definition.get("placement").is_some() =>
        {
            if let Some((entry, ..)) = MEDIA_ENTRIES
                .iter()
                .find(|(entry, ..)| definition.get(*entry).is_some())
            {
                problems.add(format!(
                    "agent.providers.{name}.{entry} borrows this provider's brokered route, \
                     which takes its key in x-api-key, and the {entry} tool sends it as \
                     Authorization: Bearer"
                ));
            }
        }
        Ok(_) => {}
    }
    if !brokered
        && let Some(key) = ROUTE_KEYS
            .into_iter()
            .find(|key| definition.get(*key).is_some())
    {
        problems.add(format!(
            "agent.providers.{name}.{key} applies to a brokered route, and this provider is \
             not brokered, so its key crosses whole"
        ));
    }
}

/// Reads the operator's provider definitions, which are passed through as they
/// are written.
///
/// Each value is handed to the agent unread, so only the shape this depends on
/// is checked: a name mapping to an object, `kind` and `images`, which errand
/// translates into the agent's own configuration, and `discover` and `usage`,
/// which errand reads itself. Naming the fields here would mean
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
        validate_provider_id(name, definition, problems);
        if let Some(flag) = definition.get("broker")
            && !flag.is_boolean()
        {
            problems.add(format!(
                "agent.providers.{name}.broker must be true or false"
            ));
        }
        if let Some(kind) = definition.get("kind") {
            let known = kind
                .as_str()
                .is_some_and(|kind| schema::defaults::CUSTOM_PROVIDER_KINDS.contains(&kind));
            if !known {
                problems.add(format!(
                    "agent.providers.{name}.kind must be one of: {}",
                    schema::defaults::CUSTOM_PROVIDER_KINDS.join(", ")
                ));
            }
        }
        validate_route(name, definition, problems);
        // A named variable is filled from the credential, so one without it
        // has nothing to carry, and a session started with it cannot launch.
        let filled = |key: &str| {
            definition
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        };
        if filled("credentialName") && !filled("credential") {
            problems.add(format!(
                "agent.providers.{name}.credentialName names a variable, but there is no \
                 credential to put in it: add agent.providers.{name}.credential or remove \
                 credentialName"
            ));
        }
        for (entry, optional, words) in MEDIA_ENTRIES {
            validate_media(name, definition, entry, optional, words, problems);
        }
        match Discovery::of(name, definition) {
            Err(found) => {
                for problem in found {
                    problems.add(problem);
                }
            }
            Ok(Some(_)) if definition.get("baseUrl").and_then(Value::as_str).is_none() => {
                problems.add(format!(
                    "agent.providers.{name}.discover needs a baseUrl to ask under"
                ));
            }
            Ok(_) => {}
        }
        match UsageShape::of(name, definition) {
            Err(found) => {
                for problem in found {
                    problems.add(problem);
                }
            }
            Ok(Some(shape)) => {
                let has = |key: &str| definition.get(key).and_then(Value::as_str).is_some();
                if !has("credential") {
                    problems.add(format!(
                        "agent.providers.{name}.usage needs a credential to ask with"
                    ));
                }
                if shape != UsageShape::Zai && !has("baseUrl") {
                    problems.add(format!(
                        "agent.providers.{name}.usage needs a baseUrl to ask under"
                    ));
                }
            }
            Ok(None) => {}
        }
        defined.insert(name.trim().to_owned(), definition.clone());
    }
    for (entry, ..) in MEDIA_ENTRIES {
        validate_single_carrier(&defined, entry, problems);
    }
    defined
}

/// The media entries a provider may carry: the entry, its optional keys,
/// and the tool and endpoint a message names. `editModel` is the model
/// kage's `edit_image` asks for, since an edit server often names its
/// models apart from the images it draws.
const MEDIA_ENTRIES: [(&str, &[&str], (&str, &str)); 3] = [
    ("images", &["editModel"], ("image", "images")),
    ("speech", &["voice"], ("speech", "speech")),
    ("music", &[], ("music", "music")),
];

/// Refuses more than one provider carrying `entry`, naming them all, since
/// the agent can be pointed at only one endpoint per tool.
fn validate_single_carrier(defined: &Map<String, Value>, entry: &str, problems: &mut Problems) {
    let carriers: Vec<&str> = defined
        .iter()
        .filter(|(_, definition)| definition.get(entry).is_some())
        .map(|(name, _)| name.as_str())
        .collect();
    if carriers.len() > 1 {
        problems.add(format!(
            "agent.providers: only one provider may carry {entry}, and {} do",
            carriers.join(", ")
        ));
    }
}

/// Reads what one of kage's media tools needs from a provider, which errand
/// translates into the agent's own configuration: an object named `entry`
/// naming the model every call asks for, plus the `optional` keys the entry
/// may carry. `tool` names the tool and `api` the endpoint in a message.
///
/// The endpoint must speak an `OpenAI`-shaped API, which an `anthropic` kind
/// does not, and errand must be able to name it, which it cannot for a
/// provider a plugin registers. Which of kage's own providers speak it is
/// kage's to say, so that is checked against its list once the backend is
/// probed. One provider may carry several entries.
fn validate_media(
    name: &str,
    definition: &Value,
    entry: &str,
    optional: &[&str],
    (tool, api): (&str, &str),
    problems: &mut Problems,
) {
    let Some(media) = definition.get(entry) else {
        return;
    };
    let place = format!("agent.providers.{name}.{entry}");
    let Some(fields) = media.as_object() else {
        problems.add(format!("{place} must be an object naming a model"));
        return;
    };
    let known: Vec<&str> = std::iter::once("model")
        .chain(optional.iter().copied())
        .collect();
    reject_unknown(fields, &known, &place, problems);
    let _ = required_string(fields, "model", &place, problems);
    for key in optional {
        let _ = optional_string(fields, key, &place, problems);
    }
    if definition.get("extension").and_then(Value::as_bool) == Some(true) {
        problems.add(format!(
            "{place} needs an endpoint errand writes for the agent, and a provider a plugin \
             registers has none"
        ));
    }
    if definition.get("extension").and_then(Value::as_bool) != Some(true)
        && !["baseUrl", "credential"].iter().any(|field| {
            definition
                .get(*field)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        })
    {
        problems.add(format!(
            "{place} needs a baseUrl, or a credential when kage already knows the provider; \
             otherwise sessions run without the {tool} tool"
        ));
    }
    if definition.get("kind").and_then(Value::as_str) == Some("anthropic") {
        problems.add(format!(
            "{place} needs a provider that speaks the OpenAI API: the Anthropic API has no \
             {api} endpoint"
        ));
    }
}

/// Reads the models a session falls back to, each a name for one.
fn validate_fallback(source: &Map<String, Value>, problems: &mut Problems) -> Vec<String> {
    let Some(raw) = source.get("fallback") else {
        return Vec::new();
    };
    let Some(entries) = raw.as_array() else {
        problems.add("agent.fallback must be a list of models, in the order to try them");
        return Vec::new();
    };
    let mut models = Vec::new();
    for entry in entries {
        match entry.as_str().map(str::trim) {
            Some(model) if !model.is_empty() => models.push(model.to_owned()),
            _ => problems.add("agent.fallback contains an entry that does not name a model"),
        }
    }
    models
}

/// Reads a list of command patterns the agent may not run.
///
/// Each is checked as a glob here rather than left to the agent, which would
/// only refuse a pattern that fails to compile when a tool call happens to
/// reach it. An unusable pattern is a guard the operator believes is up.
fn validate_deny(source: &Map<String, Value>, problems: &mut Problems) -> Vec<String> {
    let Some(raw) = source.get("deny") else {
        return Vec::new();
    };
    let Some(entries) = raw.as_array() else {
        problems.add("agent.deny must be a list of command patterns");
        return Vec::new();
    };
    let mut patterns = Vec::new();
    for entry in entries {
        let Some(pattern) = entry.as_str().map(str::trim) else {
            problems.add("agent.deny contains an entry that does not name a command pattern");
            continue;
        };
        if pattern.is_empty() {
            problems.add("agent.deny contains an empty pattern, which would refuse everything");
            continue;
        }
        if !compiles_as_glob(pattern) {
            problems.add(format!(
                "agent.deny entry {pattern:?} is not a usable pattern: a bracket, brace or \
                 parenthesis is left open"
            ));
            continue;
        }
        patterns.push(pattern.to_owned());
    }
    patterns
}

/// Whether a glob's grouping is balanced.
///
/// A full glob compiler is not worth a dependency for one check, and this is
/// the mistake worth catching: a pattern with an unclosed group is refused
/// by the agent at launch, which ends the session rather than the command.
/// Anything subtler is left to the agent, which refuses a pattern it cannot
/// use the same way.
fn compiles_as_glob(pattern: &str) -> bool {
    let (mut square, mut curly, mut round) = (0_i32, 0_i32, 0_i32);
    let mut characters = pattern.chars();
    while let Some(character) = characters.next() {
        match character {
            // An escaped bracket is a literal one, not a group.
            '\\' => {
                characters.next();
            }
            '[' => square += 1,
            ']' => square -= 1,
            '{' => curly += 1,
            '}' => curly -= 1,
            '(' => round += 1,
            ')' => round -= 1,
            _ => {}
        }
    }
    square == 0 && curly == 0 && round == 0
}

/// Reads the tools the agent may not call at all.
///
/// Named as the agent calls them, so the set is checked against the tools
/// this build of the agent registers. A typo would otherwise write a rule
/// for a tool that does not exist and read as one that does.
fn validate_denied_tools(source: &Map<String, Value>, problems: &mut Problems) -> Vec<String> {
    let Some(raw) = source.get("deniedTools") else {
        return Vec::new();
    };
    let Some(entries) = raw.as_array() else {
        problems.add("agent.deniedTools must be a list of tool names");
        return Vec::new();
    };
    let mut tools = Vec::new();
    for entry in entries {
        match entry.as_str().map(str::trim) {
            Some(tool) if !tool.is_empty() => tools.push(tool.to_owned()),
            _ => problems.add("agent.deniedTools contains an entry that does not name a tool"),
        }
    }
    tools
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
    // Same courtesy for the pi-era key: answered with where the idea went.
    if source.contains_key("extensions") {
        problems.add(
            "agent.extensions is gone with pi. Name kage plugin files with agent.plugins instead",
        );
    }

    let agent = AgentConfig {
        provider: required_string(&source, "provider", "agent", problems),
        model: optional_string(&source, "model", "agent", problems),
        vision_model: optional_string(&source, "visionModel", "agent", problems),
        image_gen: optional_string(&source, "imageGen", "agent", problems),
        speech_gen: optional_string(&source, "speechGen", "agent", problems),
        delegate: validate_delegate(&source, problems),
        rules_path: optional_absolute_path(&source, "rulesPath", "agent", problems),
        providers: validate_providers(&source, problems),
        plugins: validate_plugins(&source, problems),
        aliases: validate_aliases(&source, problems),
        fallback: validate_fallback(&source, problems),
        deny: validate_deny(&source, problems),
        denied_tools: validate_denied_tools(&source, problems),
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
        token: required_token(&source, "github", problems),
        user_name: required_string(&source, "userName", "github", problems),
        user_email: required_string(&source, "userEmail", "github", problems),
        trigger: validate_github_trigger(&source, problems),
        app: validate_github_app(&source, problems),
    })
}

/// Reads the GitHub App a session's credential is minted from, if one is
/// named.
fn validate_github_app(
    source: &Map<String, Value>,
    problems: &mut Problems,
) -> Option<GithubAppConfig> {
    source.get("app")?;
    let app = section(source, "app");
    reject_unknown(
        &app,
        &[
            "appId",
            "installationId",
            "privateKeyPath",
            "permissions",
            "repositories",
        ],
        "github.app",
        problems,
    );
    let id = |key: &str, problems: &mut Problems| {
        let read = match app.get(key) {
            Some(Value::Number(number)) => number.as_u64(),
            Some(Value::String(written)) => written.trim().parse::<u64>().ok(),
            _ => None,
        };
        match read {
            Some(id) if id > 0 => id,
            _ => {
                problems.add(format!(
                    "github.app.{key} must be the number GitHub shows for it"
                ));
                0
            }
        }
    };
    let app_id = id("appId", problems);
    let installation_id = id("installationId", problems);
    let private_key_path = required_string(&app, "privateKeyPath", "github.app", problems);
    if !private_key_path.is_empty() && !is_absolute(&private_key_path) {
        problems.add("github.app.privateKeyPath must be an absolute path");
    }
    let mut permissions = BTreeMap::new();
    match app.get("permissions") {
        None => {}
        Some(Value::Object(written)) => {
            for (name, level) in written {
                match level.as_str() {
                    Some(level @ ("read" | "write")) if !name.trim().is_empty() => {
                        permissions.insert(name.clone(), level.to_owned());
                    }
                    _ => problems.add(format!(
                        "github.app.permissions.{name} must be \"read\" or \"write\""
                    )),
                }
            }
        }
        Some(_) => problems.add(
            "github.app.permissions must map a permission to \"read\" or \"write\", as \
             { \"contents\": \"write\" }",
        ),
    }
    let repositories = id_list(&app, "repositories", "github.app", problems);
    for repository in &repositories {
        if repository.contains('/') {
            problems.add(format!(
                "github.app.repositories names `{repository}`, but a repository is named here \
                 without its owner, which is the installation's"
            ));
        }
    }
    Some(GithubAppConfig {
        app_id,
        installation_id,
        private_key_path,
        permissions,
        repositories,
    })
}

/// Reads who may ask for work from GitHub, if anybody.
///
/// A wildcard is refused rather than read: on a public repository anyone can
/// comment, so "anyone" would mean running whatever a stranger wrote.
fn validate_github_trigger(
    source: &Map<String, Value>,
    problems: &mut Problems,
) -> Option<GithubTrigger> {
    source.get("trigger")?;
    let trigger = section(source, "trigger");
    reject_unknown(
        &trigger,
        &["allowedUsers", "repositories", "on"],
        "github.trigger",
        problems,
    );
    let allowed_users = id_list(&trigger, "allowedUsers", "github.trigger", problems);
    let repositories = id_list(&trigger, "repositories", "github.trigger", problems);
    for repository in &repositories {
        let named = repository.split_once('/').is_some_and(|(owner, name)| {
            !owner.is_empty() && !name.is_empty() && !name.contains('/')
        });
        if !named {
            problems.add(format!(
                "github.trigger.repositories names `{repository}`, which is not `owner/repo` or \
                 `owner/*`"
            ));
        }
    }
    let on = match trigger.get("on") {
        None => vec!["mention".to_owned(), "assign".to_owned()],
        Some(_) => id_list(&trigger, "on", "github.trigger", problems),
    };
    for starts in &on {
        if starts != "mention" && starts != "assign" {
            problems.add(format!(
                "github.trigger.on names `{starts}`; what starts a session is `mention` or `assign`"
            ));
        }
    }
    if on.is_empty() {
        problems.add("github.trigger.on names nothing, so nothing would ever start a session");
    }
    if allowed_users.is_empty() {
        problems
            .add("github.trigger.allowedUsers must name the GitHub logins that may ask for work");
    }
    if allowed_users.iter().any(|login| login == "*") {
        problems.add(
            "github.trigger.allowedUsers cannot be \"*\": anyone can comment on a public \
             repository, and every session runs code",
        );
    }
    Some(GithubTrigger {
        allowed_users,
        repositories,
        on_mention: on.iter().any(|starts| starts == "mention"),
        on_assign: on.iter().any(|starts| starts == "assign"),
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

/// Reads the kage plugins named for every session, if any.
///
/// Each entry names one `.lua` file on the host and the capabilities the
/// operator grants it. A missing grant is not refused: a plugin left without
/// what it needs disables itself at load, saying so.
fn validate_plugins(source: &Map<String, Value>, problems: &mut Problems) -> Vec<PluginConfig> {
    let Some(Value::Array(entries)) = source.get("plugins") else {
        if source.get("plugins").is_some() {
            problems.add("agent.plugins must be a list of plugin entries");
        }
        return Vec::new();
    };
    let mut plugins = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let Some(fields) = entry.as_object() else {
            problems.add(format!(
                "agent.plugins[{index}] must be an object with a path"
            ));
            continue;
        };
        reject_unknown(
            fields,
            &["path", "capabilities"],
            &format!("agent.plugins[{index}]"),
            problems,
        );
        let Some(Value::String(path)) = fields.get("path") else {
            problems.add(format!("agent.plugins[{index}].path is required"));
            continue;
        };
        let path = path.trim();
        if !is_absolute(path) {
            problems.add(format!(
                "agent.plugins[{index}].path must be an absolute path, got {path}"
            ));
            continue;
        }
        if std::path::Path::new(path).extension() != Some(std::ffi::OsStr::new("lua")) {
            problems.add(format!(
                "agent.plugins[{index}].path must name a .lua file, got {path}"
            ));
            continue;
        }
        let mut capabilities = Vec::new();
        match fields.get("capabilities") {
            None => {}
            Some(Value::Array(grants)) => {
                for grant in grants {
                    match grant
                        .as_str()
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                    {
                        Some(name) => capabilities.push(name.to_owned()),
                        None => problems.add(format!(
                            "agent.plugins[{index}].capabilities contains an entry that is not a \
                             capability name"
                        )),
                    }
                }
            }
            Some(_) => problems.add(format!(
                "agent.plugins[{index}].capabilities must be a list of capability names"
            )),
        }
        plugins.push(PluginConfig {
            path: resolve(path),
            capabilities,
        });
    }
    plugins
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

#[expect(clippy::too_many_lines, reason = "one field per sandbox setting")]
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
        tmp_size: size(
            &source,
            "tmpSize",
            schema::defaults::TMP_SIZE,
            "sandbox",
            problems,
        ),
        shm_size: size(
            &source,
            "shmSize",
            schema::defaults::SHM_SIZE,
            "sandbox",
            problems,
        ),
        disk_tmp: flag(
            &source,
            "diskTmp",
            schema::defaults::DISK_TMP,
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
        max_upload_bytes: positive_as(
            &source,
            "maxUploadBytes",
            schema::defaults::MAX_UPLOAD_BYTES,
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
        post_images: flag(
            &source,
            "postImages",
            schema::defaults::POST_IMAGES,
            "output",
            problems,
        ),
        post_audio: flag(
            &source,
            "postAudio",
            schema::defaults::POST_AUDIO,
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

    let web = WebConfig {
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
        login: validate_web_login(&source, problems),
    };

    // The bind check cannot see a tunnel: what arrives through one comes from
    // loopback like everything else. A public address is the operator saying
    // there is one, and without a login everyone who reaches it would act as
    // the operator.
    if web.public_url.is_some() && !web.observer && web.login.is_none() {
        problems.add(
            "web.publicUrl says the interface is reachable from outside, and without web.login \
             anyone who reaches it acts with operator authority; set web.login, or set \
             web.observer to true",
        );
    }
    Some(web)
}

/// Reads the Discord application visitors log in through, if any.
fn validate_web_login(
    source: &Map<String, Value>,
    problems: &mut Problems,
) -> Option<WebLoginConfig> {
    source.get("login")?;
    let section = section(source, "login");
    reject_unknown(&section, &KNOWN_WEB_LOGIN, "web.login", problems);

    Some(WebLoginConfig {
        client_id: required_string(&section, "clientId", "web.login", problems),
        client_secret: required_secret(&section, "clientSecret", "web.login", problems),
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

fn validate_retention(raw: &Map<String, Value>, problems: &mut Problems) -> RetentionConfig {
    let source = section(raw, "retention");
    reject_unknown(&source, &KNOWN_RETENTION, "retention", problems);
    let length = |key: &str, default: &str, problems: &mut Problems| {
        let written = optional_string(&source, key, "retention", problems)
            .unwrap_or_else(|| default.to_owned());
        if parse_duration(&written).is_none() {
            problems.add(format!(
                "retention.{key} must be a length of time such as 30d, 2w or 7d 12h"
            ));
            return default.to_owned();
        }
        written
    };
    RetentionConfig {
        enabled: flag(
            &source,
            "enabled",
            schema::defaults::RETENTION_ENABLED,
            "retention",
            problems,
        ),
        older_than: length(
            "olderThan",
            schema::defaults::RETENTION_OLDER_THAN,
            problems,
        ),
        every: length("every", schema::defaults::RETENTION_EVERY, problems),
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
    let retention = validate_retention(raw, &mut problems);

    // Asked once both sections are read, since the name is the operator's own.
    // Shadowing it would authenticate the agent with whatever was set here.
    if let Some(env) = sandbox.env.as_ref() {
        for name in agent.credential_vars().values() {
            if env.contains_key(name) {
                problems.add(format!(
                    "sandbox.env must not set {name}, which carries a provider credential"
                ));
            }
        }
    }

    // A session can read its project and its state, and an App key it could
    // read is the App itself, able to mint any permission it was granted.
    // Compared where each really is, so neither a `..` nor a link moves the
    // key into a root without it showing.
    if let Some(key) = github
        .as_ref()
        .and_then(|github| github.app.as_ref())
        .map(|app| real_path(&app.private_key_path))
        .filter(|key| !key.is_empty())
        && [&project_root, &state_dir]
            .iter()
            .any(|root| !root.is_empty() && nests(&real_path(root), &key))
    {
        problems.add(
            "github.app.privateKeyPath must lie outside projectRoot and stateDir, which a session \
             can read",
        );
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
            retention,
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
