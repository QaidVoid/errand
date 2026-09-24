//! How a provider definition says to read what is left of its usage window.
//!
//! Written as `usage` on a provider: `"zai"` for z.ai's quota endpoint,
//! `"gateway"` for a gateway that serves `/usage` beside its API, or an object
//! that says where a provider shaped otherwise keeps the numbers.

use serde_json::Value;

/// Where a provider's usage window is read from.
#[derive(Debug, Clone, PartialEq)]
pub enum UsageShape {
    /// z.ai's own quota endpoint, asked with the provider's key.
    Zai,
    /// `/usage` under the provider's `baseUrl`, as a gateway serves it.
    Gateway,
    /// An endpoint under the provider's `baseUrl`, read as the mapping says.
    Mapped(MappedUsage),
}

/// Where one provider keeps its usage numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct MappedUsage {
    /// The path under the provider's `baseUrl` that answers.
    pub path: String,
    /// Whether the key is sent as `Bearer <key>` or as the key alone.
    pub bearer: bool,
    /// Where the percentage sits in the answer, as a dotted path.
    pub percent: String,
    /// Whether that percentage is what is left rather than what is used.
    pub percent_is_left: bool,
    /// Where the reset time sits, as a dotted path, and how it is written.
    pub resets: Option<(String, ResetFormat)>,
}

/// How a reset time is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetFormat {
    /// An RFC 3339 timestamp.
    Iso,
    /// Milliseconds since the epoch.
    Millis,
    /// Seconds since the epoch.
    Seconds,
}

impl UsageShape {
    /// Reads a provider definition's `usage`, or nothing when it has none.
    ///
    /// Refuses with every problem it finds, named for the provider, rather
    /// than reading a window upside down because a word was misspelt.
    pub fn of(provider: &str, definition: &Value) -> Result<Option<Self>, Vec<String>> {
        let place = format!("agent.providers.{provider}.usage");
        let written = match definition.get("usage") {
            None => return Ok(None),
            Some(Value::String(named)) if named == "zai" => return Ok(Some(Self::Zai)),
            Some(Value::String(named)) if named == "gateway" => return Ok(Some(Self::Gateway)),
            Some(Value::Object(written)) => written,
            Some(_) => {
                return Err(vec![format!(
                    "{place} must be \"zai\", \"gateway\", or an object saying where the numbers are"
                )]);
            }
        };

        let mut problems = Vec::new();
        let text = |key: &str, problems: &mut Vec<String>| match written.get(key) {
            None => None,
            Some(Value::String(value)) if !value.trim().is_empty() => Some(value.trim().to_owned()),
            Some(_) => {
                problems.push(format!("{place}.{key} must be text"));
                None
            }
        };
        for key in written.keys() {
            if !["path", "auth", "percent", "percentIs", "resets", "resetsAs"]
                .contains(&key.as_str())
            {
                problems.push(format!(
                    "{place}.{key} is not something usage takes; use path, auth, percent, \
                     percentIs, resets, or resetsAs"
                ));
            }
        }

        let path = text("path", &mut problems).unwrap_or_else(|| "/usage".to_owned());
        if !path.starts_with('/') {
            problems.push(format!("{place}.path must be a path starting with /"));
        }
        let bearer = match text("auth", &mut problems).as_deref() {
            None | Some("bearer") => true,
            Some("raw") => false,
            Some(_) => {
                problems.push(format!("{place}.auth must be \"bearer\" or \"raw\""));
                true
            }
        };
        let percent = text("percent", &mut problems);
        if percent.is_none() {
            problems.push(format!("{place}.percent must say where the percentage is"));
        }
        let percent_is_left = match text("percentIs", &mut problems).as_deref() {
            Some("left") => true,
            Some("used") => false,
            _ => {
                problems.push(format!(
                    "{place}.percentIs must be \"used\" or \"left\", since reading one as the \
                     other turns the window upside down"
                ));
                false
            }
        };
        let resets = match (
            text("resets", &mut problems),
            text("resetsAs", &mut problems).as_deref(),
        ) {
            (None, None) => None,
            (Some(at), Some("iso")) => Some((at, ResetFormat::Iso)),
            (Some(at), Some("ms")) => Some((at, ResetFormat::Millis)),
            (Some(at), Some("s")) => Some((at, ResetFormat::Seconds)),
            (Some(_), _) => {
                problems.push(format!(
                    "{place}.resetsAs must be \"iso\", \"ms\", or \"s\" when resets is set"
                ));
                None
            }
            (None, Some(_)) => {
                problems.push(format!(
                    "{place}.resetsAs needs resets to say where the time is"
                ));
                None
            }
        };

        match percent {
            Some(percent) if problems.is_empty() => Ok(Some(Self::Mapped(MappedUsage {
                path,
                bearer,
                percent,
                percent_is_left,
                resets,
            }))),
            _ => Err(problems),
        }
    }
}

#[cfg(test)]
mod tests;
