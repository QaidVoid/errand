//! How a provider definition says to read what is left of its usage window.
//!
//! Written as `usage` on a provider: `"zai"` for z.ai's quota endpoint,
//! `"gateway"` for a gateway that serves `/usage` beside its API, or an object
//! that says where a provider shaped otherwise keeps the numbers.

use serde_json::{Map, Value};

use super::path;

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
    /// The windows to read, the one to show first. The next is shown when
    /// one is missing or already past its reset, and a spent one is shown
    /// over them all, since it is what stops work.
    pub windows: Vec<MappedWindow>,
}

/// Where one window's numbers sit in the answer.
#[derive(Debug, Clone, PartialEq)]
pub struct MappedWindow {
    /// Where the percentage sits, as a path.
    pub percent: String,
    /// Whether that percentage is what is left rather than what is used.
    pub percent_is_left: bool,
    /// Where the reset time sits, as a path, and how it is written.
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
        for key in written.keys() {
            if ![
                "path",
                "auth",
                "windows",
                "percent",
                "percentIs",
                "resets",
                "resetsAs",
            ]
            .contains(&key.as_str())
            {
                problems.push(format!(
                    "{place}.{key} is not something usage takes; use path, auth, windows, \
                     percent, percentIs, resets, or resetsAs"
                ));
            }
        }

        let path =
            text(written, "path", &place, &mut problems).unwrap_or_else(|| "/usage".to_owned());
        if !path.starts_with('/') {
            problems.push(format!("{place}.path must be a path starting with /"));
        }
        let bearer = match text(written, "auth", &place, &mut problems).as_deref() {
            None | Some("bearer") => true,
            Some("raw") => false,
            Some(_) => {
                problems.push(format!("{place}.auth must be \"bearer\" or \"raw\""));
                true
            }
        };

        let windows = match written.get("windows") {
            None => window(written, &place, &mut problems).into_iter().collect(),
            Some(Value::Array(listed)) if !listed.is_empty() => {
                if ["percent", "percentIs", "resets", "resetsAs"]
                    .iter()
                    .any(|key| written.contains_key(*key))
                {
                    problems.push(format!(
                        "{place} names its windows under windows, so percent, percentIs, \
                         resets, and resetsAs belong in each of them"
                    ));
                }
                let mut windows = Vec::new();
                for (index, entry) in listed.iter().enumerate() {
                    let at = format!("{place}.windows[{index}]");
                    match entry {
                        Value::Object(entry) => {
                            for key in entry.keys() {
                                if !["percent", "percentIs", "resets", "resetsAs"]
                                    .contains(&key.as_str())
                                {
                                    problems.push(format!(
                                        "{at}.{key} is not something a window takes; use \
                                         percent, percentIs, resets, or resetsAs"
                                    ));
                                }
                            }
                            windows.extend(window(entry, &at, &mut problems));
                        }
                        _ => problems.push(format!("{at} must be an object")),
                    }
                }
                windows
            }
            Some(_) => {
                problems.push(format!("{place}.windows must be a list of windows"));
                Vec::new()
            }
        };

        if problems.is_empty() {
            Ok(Some(Self::Mapped(MappedUsage {
                path,
                bearer,
                windows,
            })))
        } else {
            Err(problems)
        }
    }
}

/// Reads one window's mapping out of `written`, reporting under `place`.
fn window(
    written: &Map<String, Value>,
    place: &str,
    problems: &mut Vec<String>,
) -> Option<MappedWindow> {
    let percent = text(written, "percent", place, problems);
    if let Some(percent) = &percent
        && let Err(problem) = path::check(percent)
    {
        problems.push(format!("{place}.percent: {problem}"));
    }
    if percent.is_none() {
        problems.push(format!("{place}.percent must say where the percentage is"));
    }
    let percent_is_left = match text(written, "percentIs", place, problems).as_deref() {
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
        text(written, "resets", place, problems),
        text(written, "resetsAs", place, problems).as_deref(),
    ) {
        (None, None) => None,
        (Some(at), format) => {
            if let Err(problem) = path::check(&at) {
                problems.push(format!("{place}.resets: {problem}"));
            }
            match format {
                Some("iso") => Some((at, ResetFormat::Iso)),
                Some("ms") => Some((at, ResetFormat::Millis)),
                Some("s") => Some((at, ResetFormat::Seconds)),
                _ => {
                    problems.push(format!(
                        "{place}.resetsAs must be \"iso\", \"ms\", or \"s\" when resets is set"
                    ));
                    None
                }
            }
        }
        (None, Some(_)) => {
            problems.push(format!(
                "{place}.resetsAs needs resets to say where the time is"
            ));
            None
        }
    };
    Some(MappedWindow {
        percent: percent?,
        percent_is_left,
        resets,
    })
}

/// A setting that must be text, when it is there at all.
fn text(
    written: &Map<String, Value>,
    key: &str,
    place: &str,
    problems: &mut Vec<String>,
) -> Option<String> {
    match written.get(key) {
        None => None,
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.trim().to_owned()),
        Some(_) => {
            problems.push(format!("{place}.{key} must be text"));
            None
        }
    }
}

#[cfg(test)]
mod tests;
