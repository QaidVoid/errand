//! The usage window of a provider whose definition says where it is.
//!
//! For a provider that is neither z.ai nor a gateway of the known shape: its
//! `usage` names the path, how the key is sent, and for each window it keeps,
//! where the percentage is and which way it reads, and where the reset time is
//! and how it is written.

use serde_json::Value;

use super::usage::{Fetch, HttpRequest, Quota, is_spent};
use crate::config::path::at;
use crate::config::usage::{MappedUsage, MappedWindow, ResetFormat};
use crate::log::now_ms;

/// Reads the window that matters out of an answer, where the mapping says.
///
/// A spent window stops work whatever the others say, since a spent weekly
/// allowance leaves nothing for the five hour one to give. So when any window
/// is spent, the provider reads as spent until the last of them resets.
/// Otherwise the first window is shown, or the next where one is missing or
/// already past its reset, which is a reading left over from before it rolled.
///
/// Returns nothing when no window can be read, rather than a guess: a window
/// read as spent would stop every session here.
pub fn read_mapped(body: &Value, mapping: &MappedUsage, now: i64) -> Option<Quota> {
    let read: Vec<Quota> = mapping
        .windows
        .iter()
        .filter_map(|window| read_window(body, window))
        .filter(|quota| quota.resets_at.is_none_or(|at| at > now))
        .collect();
    let spent_until = read
        .iter()
        .filter(|quota| is_spent(quota))
        .map(|quota| quota.resets_at)
        .max();
    match spent_until {
        Some(resets_at) => Some(Quota {
            percentage: 100.0,
            resets_at,
        }),
        None => read.into_iter().next(),
    }
}

/// One window's reading, where the mapping says it is.
fn read_window(body: &Value, window: &MappedWindow) -> Option<Quota> {
    let percent = at(body, &window.percent)?.as_f64()?;
    let used = if window.percent_is_left {
        100.0 - percent
    } else {
        percent
    };
    let resets_at = window
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
    read_mapped(&response.body?, mapping, now_ms())
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

#[cfg(test)]
mod tests;
