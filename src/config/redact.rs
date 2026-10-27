//! Removes secret values from anything about to be logged or posted.
//!
//! Two layers, because either alone leaks. Structural redaction blanks the
//! known secret fields when the effective configuration is rendered. Value
//! redaction scrubs the same strings out of arbitrary text, since an error
//! thrown by a library may carry a token that never passed through the config
//! renderer.

use serde_json::Value;

use crate::config::schema::{Config, SECRET_PATHS};

/// What a secret is replaced with. Fixed so it is greppable in a log.
pub const REDACTION: &str = "[redacted]";

/// Shortest secret worth scrubbing from free text, to avoid mangling prose.
const MIN_SCRUBBABLE_LENGTH: usize = 8;

/// Renders the effective configuration with every secret field blanked, for
/// logging at startup.
///
/// The fixed paths in [`SECRET_PATHS`] are not the whole of it. An operator
/// may define any number of providers under any names they like, each with a
/// credential of its own, so those are walked rather than named.
pub fn redact_config(config: &Config) -> Value {
    let mut clone =
        serde_json::to_value(config).expect("the resolved configuration always renders as JSON");
    for path in SECRET_PATHS {
        let Some((parent_path, last)) = path.rsplit_once('.') else {
            continue;
        };
        if let Some(Value::Object(fields)) = read_path_mut(&mut clone, parent_path) {
            fields.insert(last.to_owned(), Value::String(REDACTION.to_owned()));
        }
    }

    // A defined provider carries its own credential, and there is no fixed
    // path to name it by: the operator chooses how many providers there are
    // and what they are called. Walked instead, so a second provider's key is
    // blanked as thoroughly as the first one's.
    if let Some(Value::Object(providers)) = read_path_mut(&mut clone, "agent.providers") {
        for definition in providers.values_mut() {
            if let Value::Object(fields) = definition
                && fields.contains_key("credential")
            {
                fields.insert("credential".to_owned(), Value::String(REDACTION.to_owned()));
            }
        }
    }
    clone
}

/// Collects the secret values held by a configuration, for scrubbing text.
pub fn secret_values(config: &Config) -> Vec<String> {
    let mut values = Vec::new();
    let mut hold = |value: Option<&str>| {
        if let Some(value) = value.filter(|value| value.len() >= MIN_SCRUBBABLE_LENGTH) {
            values.push(value.to_owned());
        }
    };
    hold(Some(&config.chat.token));
    if let Some(github) = &config.github {
        hold(Some(&github.token));
    }
    if let Some(login) = config.web.as_ref().and_then(|web| web.login.as_ref()) {
        hold(Some(&login.client_secret));
    }
    // A defined provider carries its own credential, and there is no fixed
    // path to name it by: the operator chooses how many providers there are
    // and what they are called. Walked instead, so a second provider's key is
    // scrubbed as thoroughly as the first one's.
    for definition in config.agent.providers.values() {
        hold(definition.get("credential").and_then(Value::as_str));
    }
    values
}

/// Scrubs known secret values out of arbitrary text.
///
/// A secret shorter than the minimum is left alone: replacing a short string
/// everywhere it occurs mangles ordinary prose without hiding anything worth
/// hiding.
pub fn redact_text(text: &str, secrets: &[String]) -> String {
    let mut scrubbed = text.to_owned();
    for secret in secrets {
        if secret.len() < MIN_SCRUBBABLE_LENGTH {
            continue;
        }
        scrubbed = scrubbed.replace(secret.as_str(), REDACTION);
    }
    scrubbed
}

/// Scrubs known secret values out of bytes that arrive in pieces.
///
/// A secret can be split across two pieces, so each piece is held back by one
/// byte less than the longest secret: anything that could still be the start
/// of one is kept until the next piece shows whether it is.
pub struct ByteScrubber {
    secrets: Vec<Vec<u8>>,
    keep: usize,
    carry: Vec<u8>,
}

impl ByteScrubber {
    /// A scrubber for `secrets`, skipping any too short to be worth it.
    pub fn new(secrets: &[String]) -> Self {
        let secrets: Vec<Vec<u8>> = secrets
            .iter()
            .filter(|secret| secret.len() >= MIN_SCRUBBABLE_LENGTH)
            .map(|secret| secret.as_bytes().to_vec())
            .collect();
        let keep = secrets.iter().map(Vec::len).max().unwrap_or(1) - 1;
        Self {
            secrets,
            keep,
            carry: Vec::new(),
        }
    }

    /// Takes the next piece and returns what is safe to pass on.
    pub fn push(&mut self, piece: &[u8]) -> Vec<u8> {
        self.carry.extend_from_slice(piece);
        let held = std::mem::take(&mut self.carry);
        let scrubbed = self.scrub(held);
        let split = scrubbed.len().saturating_sub(self.keep);
        self.carry = scrubbed[split..].to_vec();
        scrubbed[..split].to_vec()
    }

    /// Returns whatever was held back, once there are no more pieces.
    pub fn finish(&mut self) -> Vec<u8> {
        let rest = std::mem::take(&mut self.carry);
        self.scrub(rest)
    }

    fn scrub(&self, mut bytes: Vec<u8>) -> Vec<u8> {
        for secret in &self.secrets {
            let mut out = Vec::with_capacity(bytes.len());
            let mut at = 0;
            while at < bytes.len() {
                if bytes[at..].starts_with(secret) {
                    out.extend_from_slice(REDACTION.as_bytes());
                    at += secret.len();
                } else {
                    out.push(bytes[at]);
                    at += 1;
                }
            }
            bytes = out;
        }
        bytes
    }
}

/// Walks a dotted path to the value it names, when every step is an object.
fn read_path_mut<'a>(source: &'a mut Value, path: &str) -> Option<&'a mut Value> {
    let mut current = source;
    for segment in path.split('.') {
        current = current.as_object_mut()?.get_mut(segment)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests;
