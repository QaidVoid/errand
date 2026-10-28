//! What a brokered provider's route lets a session do with its key, as the
//! provider definition declares it.
//!
//! Written on a provider as `placement`, the header the key travels in, and
//! `capabilities`, the calls the key may make, with `strictCapabilities`
//! turning a declared list from a warning into a refusal.

use serde_json::Value;

/// The header a route's key travels in, both ways: where the session's nonce
/// is accepted and where the real credential is put.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    /// Nothing declared and no `kind` to derive it from: the nonce is
    /// accepted in either header, and the credential goes out as a bearer
    /// token and in `x-api-key` too when the session sent one. Every built-in
    /// provider is here unless its definition says otherwise.
    #[default]
    Either,
    /// `Authorization: Bearer`, as an OpenAI-shaped provider reads it.
    Bearer,
    /// `x-api-key`, as an Anthropic-shaped provider reads it.
    ApiKey,
}

/// One call a route's key may make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    /// The method, upper case, or `*` for any.
    pub method: String,
    /// The path after the route's prefix, where a `*` segment matches one
    /// segment and a `**` segment any number, none included.
    pub path: String,
}

/// The calls a route's key may make.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Capabilities {
    /// Nothing declared: every call is forwarded and named in the log with
    /// the entry that would allow it.
    #[default]
    Unset,
    /// `"*"`: every call is forwarded, quietly.
    Any,
    /// The calls listed, in order. The first that matches allows a call.
    Declared(Vec<Capability>),
}

/// What a route allows, read from its provider definition.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RoutePolicy {
    /// The header the key travels in.
    pub placement: Placement,
    /// The calls the key may make.
    pub capabilities: Capabilities,
    /// Whether a call the declared list does not name is refused rather than
    /// forwarded with a warning.
    pub strict: bool,
}

impl RoutePolicy {
    /// Reads a provider definition's `placement`, `capabilities` and
    /// `strictCapabilities`.
    ///
    /// A placement left out is derived from an explicit `kind`, since that
    /// says which shape of client the session is given: `anthropic` sends
    /// `x-api-key` and `openai` a bearer token. A definition with no `kind`
    /// keeps either header, because a built-in provider needs none and errand
    /// has no list of which header each built-in reads.
    ///
    /// Refuses with every problem it finds, named for the provider.
    pub fn of(provider: &str, definition: &Value) -> Result<Self, Vec<String>> {
        let place = format!("agent.providers.{provider}");
        let mut problems = Vec::new();
        let placement = match definition.get("placement") {
            None => match definition.get("kind").and_then(Value::as_str) {
                Some("anthropic") => Placement::ApiKey,
                Some("openai") => Placement::Bearer,
                _ => Placement::Either,
            },
            Some(Value::String(written)) if written == "bearer" => Placement::Bearer,
            Some(Value::String(written)) if written == "x-api-key" => Placement::ApiKey,
            Some(_) => {
                problems.push(format!(
                    "{place}.placement must be \"bearer\" or \"x-api-key\", the header the \
                     provider reads its key from"
                ));
                Placement::Either
            }
        };
        let capabilities = match definition.get("capabilities") {
            None => Capabilities::Unset,
            Some(Value::String(any)) if any == "*" => Capabilities::Any,
            Some(Value::Array(entries)) if entries.is_empty() => {
                problems.push(format!(
                    "{place}.capabilities is empty. Write \"*\" to allow every call, or leave \
                     it out to have each call named in the log"
                ));
                Capabilities::Unset
            }
            Some(Value::Array(entries)) => Capabilities::Declared(
                entries
                    .iter()
                    .enumerate()
                    .filter_map(|(at, entry)| {
                        capability(&format!("{place}.capabilities[{at}]"), entry)
                            .map_err(|problem| problems.push(problem))
                            .ok()
                    })
                    .collect(),
            ),
            Some(_) => {
                problems.push(format!(
                    "{place}.capabilities must be \"*\" or a list of {{ \"method\", \"path\" }}"
                ));
                Capabilities::Unset
            }
        };
        let strict = match definition.get("strictCapabilities") {
            None => false,
            Some(Value::Bool(strict)) => *strict,
            Some(_) => {
                problems.push(format!("{place}.strictCapabilities must be true or false"));
                false
            }
        };
        if strict && !matches!(capabilities, Capabilities::Declared(_)) {
            problems.push(format!(
                "{place}.strictCapabilities needs a list in {place}.capabilities to hold calls \
                 to. With none every call would be refused, and with \"*\" none would be"
            ));
        }
        if problems.is_empty() {
            Ok(Self {
                placement,
                capabilities,
                strict,
            })
        } else {
            Err(problems)
        }
    }
}

impl Capabilities {
    /// Whether a declared list names this call. Nothing declared, or `"*"`,
    /// names every call.
    pub fn allow(&self, method: &str, rest: &str) -> bool {
        match self {
            Self::Unset | Self::Any => true,
            Self::Declared(list) => list
                .iter()
                .any(|capability| capability.matches(method, rest)),
        }
    }
}

impl Capability {
    /// Whether this names a call: the method, and the path after the prefix
    /// matched segment by segment, never decoded.
    pub fn matches(&self, method: &str, rest: &str) -> bool {
        (self.method == "*" || self.method == method)
            && glob(&segments(&self.path), &segments(rest))
    }
}

/// Reads one `{ "method", "path" }`.
fn capability(place: &str, entry: &Value) -> Result<Capability, String> {
    let (Some(method), Some(path)) = (
        entry.get("method").and_then(Value::as_str),
        entry.get("path").and_then(Value::as_str),
    ) else {
        return Err(format!(
            "{place} must be {{ \"method\": \"POST\", \"path\": \"/v1/messages\" }}"
        ));
    };
    if let Some(object) = entry.as_object()
        && let Some(other) = object.keys().find(|key| *key != "method" && *key != "path")
    {
        return Err(format!(
            "{place}.{other} is not something a capability takes. Use method and path"
        ));
    }
    if method != "*"
        && (method.is_empty()
            || !method
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte == b'-'))
    {
        return Err(format!(
            "{place}.method must be an upper-case method such as POST, or * for any"
        ));
    }
    if let Some(problem) = glob_problem(path) {
        return Err(format!("{place}.path {problem}"));
    }
    Ok(Capability {
        method: method.to_owned(),
        path: path.to_owned(),
    })
}

/// Why a capability's path is not one a call could be matched against.
fn glob_problem(path: &str) -> Option<&'static str> {
    let Some(rest) = path.strip_prefix('/') else {
        return Some("must start with /");
    };
    if rest.contains("//") {
        return Some("has an empty segment, which no call is let through with");
    }
    for segment in rest.split('/') {
        if segment == "." || segment == ".." {
            return Some("has a . or .. segment, which no call is let through with");
        }
        if segment.contains('*') && segment != "*" && segment != "**" {
            return Some(
                "uses * inside a segment, where a whole segment is * for one or ** for many",
            );
        }
        if segment.contains('%') {
            return Some("is matched as sent, before decoding, so it cannot hold an escape");
        }
        if segment.contains('{') || segment.contains('}') {
            return Some(
                "names a placeholder, which matches only itself, so write * for one segment",
            );
        }
        if segment.contains('?') || segment.contains(';') {
            return Some("is matched against the path alone, never a query or parameters");
        }
    }
    // Each `**` tries every split of what is left, so more than one would let
    // a long path chosen by the session cost far more than its length.
    if rest.split('/').filter(|segment| *segment == "**").count() > 1 {
        return Some("has more than one **, and one is enough to match any depth");
    }
    None
}

/// A path as its segments, after the leading slash.
fn segments(path: &str) -> Vec<&str> {
    path.strip_prefix('/')
        .map_or_else(Vec::new, |rest| rest.split('/').collect())
}

/// Whether `pattern` matches `path`, segment by segment.
fn glob(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| glob(rest, &path[skip..])),
        Some((first, rest)) => path.split_first().is_some_and(|(segment, remaining)| {
            (if *first == "*" {
                !segment.is_empty()
            } else {
                first == segment
            }) && glob(rest, remaining)
        }),
    }
}

#[cfg(test)]
mod tests;
