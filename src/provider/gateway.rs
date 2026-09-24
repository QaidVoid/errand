//! The usage window of a proxy gateway that reports one beside its API.
//!
//! A gateway of this kind serves `/usage` next to `/chat/completions`, under
//! the same base URL and the same key, and answers with the one constraint
//! that decides whether work can start. That is the whole of what is read
//! here: the other windows it lists are for a person looking, not for a
//! decision.
//!
//! Told apart from the provider's own metering by where it is asked. This is
//! the base URL the operator configured, so a provider that does not serve it
//! simply does not answer and the daemon carries on.

use serde_json::Value;

use super::usage::{Fetch, HttpRequest, Quota};

/// Where a gateway reports what is left, relative to its base URL.
pub const USAGE_PATH: &str = "/usage";

/// Reads the window out of a gateway's answer.
///
/// Only `limiting` is read, which the gateway defines as the one constraint
/// that decides whether work can start. A status other than `ok` means the
/// gateway is not sure, and an unsure answer must not read as a spent window:
/// that would stop every session on this host until somebody noticed.
///
/// Returns nothing for anything unrecognised rather than a guess.
pub fn read_gateway_usage(body: &Value) -> Option<Quota> {
    let entry = body.get("limiting")?.as_object()?;

    if entry.get("status").and_then(Value::as_str) != Some("ok") {
        return None;
    }

    let resets_text = entry.get("resetsAt").and_then(Value::as_str)?;
    let resets_at: i64 = resets_text
        .parse::<jiff::Timestamp>()
        .ok()?
        .as_millisecond();

    // The gateway says outright whether the window is spent, and that is worth
    // more than a percentage: it is the answer the gateway would give its own
    // rate limiter. A spent window is reported as full so that everything
    // downstream, which asks only about the percentage, agrees with it.
    if entry.get("spent").and_then(Value::as_bool) == Some(true) {
        return Some(Quota {
            percentage: 100.0,
            resets_at: Some(resets_at),
        });
    }

    let used = match entry.get("peakUsedPercent").and_then(Value::as_f64) {
        Some(peak) => Some(peak),
        None => entry
            .get("remainingPercent")
            .and_then(Value::as_f64)
            .map(|remaining| 100.0 - remaining),
    };
    let used = used?;

    // Not spent, whatever the arithmetic says, because the gateway already
    // said so and rounding must not close a window it left open.
    Some(Quota {
        percentage: used.clamp(0.0, 99.9),
        resets_at: Some(resets_at),
    })
}

/// Asks a gateway what is left.
///
/// Returns nothing when the answer cannot be had, which callers must treat as
/// "carry on". A gateway that is unreachable, slow, or has changed its
/// response must not become a reason to refuse work.
pub async fn fetch_gateway_usage(
    base_url: &str,
    key: &str,
    fetch: &impl Fetch,
    timeout_ms: u64,
) -> Option<Quota> {
    // One slash, whether or not the base url ended in one.
    let base = base_url.strip_suffix('/').unwrap_or(base_url);
    let response = fetch
        .fetch(
            format!("{base}{USAGE_PATH}"),
            HttpRequest {
                headers: vec![
                    ("Authorization".to_owned(), format!("Bearer {key}")),
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
    read_gateway_usage(&response.body?)
}

#[cfg(test)]
mod tests;
