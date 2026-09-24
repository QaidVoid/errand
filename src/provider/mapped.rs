//! The usage window of a provider whose definition says where it is.
//!
//! For a provider that is neither z.ai nor a gateway of the known shape: its
//! `usage` names the path, how the key is sent, where the percentage is and
//! which way it reads, and where the reset time is and how it is written.

use serde_json::Value;

use super::usage::{Fetch, HttpRequest, Quota};
use crate::config::usage::{MappedUsage, ResetFormat};

/// Reads the window out of an answer, where the mapping says it is.
///
/// Returns nothing when the percentage is not where the mapping says, rather
/// than a guess: a window read as spent would stop every session here.
pub fn read_mapped(body: &Value, mapping: &MappedUsage) -> Option<Quota> {
    let percent = at(body, &mapping.percent)?.as_f64()?;
    let used = if mapping.percent_is_left {
        100.0 - percent
    } else {
        percent
    };
    let resets_at = mapping
        .resets
        .as_ref()
        .and_then(|(path, format)| reset_time(at(body, path)?, *format));
    Some(Quota {
        percentage: used.clamp(0.0, 100.0),
        resets_at,
    })
}

/// Asks a provider for its window, under its base URL.
///
/// Returns nothing when the answer cannot be had, which callers treat as
/// "carry on", as they do for every other provider.
pub async fn fetch_mapped(
    base_url: &str,
    key: &str,
    mapping: &MappedUsage,
    fetch: &impl Fetch,
    timeout_ms: u64,
) -> Option<Quota> {
    let authorization = if mapping.bearer {
        format!("Bearer {key}")
    } else {
        key.to_owned()
    };
    let response = fetch
        .fetch(
            format!("{}{}", base_url.trim_end_matches('/'), mapping.path),
            HttpRequest {
                headers: vec![
                    ("Authorization".to_owned(), authorization),
                    ("Accept".to_owned(), "application/json".to_owned()),
                ],
                timeout_ms,
            },
        )
        .await
        .ok()?;
    if !response.ok() {
        return None;
    }
    read_mapped(&response.body?, mapping)
}

/// A reset time in epoch milliseconds, read as the mapping says it is written.
fn reset_time(value: &Value, format: ResetFormat) -> Option<i64> {
    match format {
        ResetFormat::Iso => value
            .as_str()?
            .parse::<jiff::Timestamp>()
            .ok()
            .map(jiff::Timestamp::as_millisecond),
        ResetFormat::Millis => value.as_i64(),
        ResetFormat::Seconds => value.as_i64().map(|seconds| seconds * 1_000),
    }
}

/// Walks a dotted path through nested objects.
fn at<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(value, |inside, key| inside.get(key))
}

#[cfg(test)]
mod tests;
