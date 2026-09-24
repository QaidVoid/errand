//! The z.ai usage window, so a session is not started against a spent quota.
//!
//! z.ai meters tokens in a rolling five hour window. Past it every request is
//! refused, and without this the refusal arrives as a failed turn: the thread
//! has already opened, the sandbox has already started, and the person is told
//! something went wrong rather than when to come back.
//!
//! Specific to this provider: the endpoint, the field names, and the five hour
//! window are z.ai's. What is shared with any other metered provider lives in
//! the module beside this one.

use serde_json::Value;

use super::usage::{Fetch, HttpRequest, Quota};

/// Where z.ai reports what is left of a quota.
pub const QUOTA_URL: &str = "https://bigmodel.cn/api/monitor/usage/quota/limit";

/// The rolling token window, as opposed to the monthly tool-call allowance.
const TOKENS_LIMIT: &str = "TOKENS_LIMIT";

/// Reads the token window out of a quota response.
///
/// Returns nothing for anything unrecognised rather than a guess. A shape that
/// changed must not read as a spent quota, because that would stop every
/// session on this host until somebody noticed.
pub fn read_quota(body: &Value) -> Option<Quota> {
    let limits = body.get("data")?.get("limits")?.as_array()?;

    for limit in limits {
        let Some(entry) = limit.as_object() else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some(TOKENS_LIMIT) {
            continue;
        }
        let percentage = entry.get("percentage").and_then(Value::as_f64)?;
        // A window nothing has been charged to has nothing scheduled to reset,
        // and z.ai sends null for it. That is an answer: the window is empty,
        // which is the most useful thing it could say.
        return Some(match entry.get("nextResetTime").and_then(Value::as_i64) {
            Some(resets_at) => Quota {
                percentage,
                resets_at: Some(resets_at),
            },
            None => Quota {
                percentage,
                resets_at: None,
            },
        });
    }
    None
}

/// Asks z.ai what is left of the window.
///
/// Returns nothing when the answer cannot be had, which callers must treat as
/// "carry on". A provider that is unreachable, slow, or has changed its
/// response must not become a reason to refuse work: the cost of guessing
/// wrong that way is every session refused, against one failed turn for
/// guessing wrong the other way.
pub async fn fetch_quota(key: &str, fetch: &impl Fetch, timeout_ms: u64) -> Option<Quota> {
    let response = fetch
        .fetch(
            QUOTA_URL.to_owned(),
            HttpRequest {
                // Raw, not a bearer token. This is what the endpoint accepts.
                headers: vec![
                    ("Authorization".to_owned(), key.to_owned()),
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
    read_quota(&response.body?)
}

#[cfg(test)]
mod tests;
