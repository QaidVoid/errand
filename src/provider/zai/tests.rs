//! Tests for the z.ai window, ported from `zai_test.ts`.

use std::future::Future;

use serde_json::{Value, json};

use super::{QUOTA_URL, fetch_quota, meters_usage, read_quota};
use crate::provider::usage::{Fetch, FetchError, HttpRequest, HttpResponse, Quota};

fn quota_body(percentage: f64, resets_at: i64) -> Value {
    json!({
        "data": {
            "limits": [
                { "type": "TOOL_CALL_LIMIT", "percentage": 3, "nextResetTime": 1 },
                { "type": "TOKENS_LIMIT", "percentage": percentage, "nextResetTime": resets_at },
            ],
        },
    })
}

/// A fetch over a canned answer, which records what it was asked.
struct Fake {
    answer: Result<HttpResponse, String>,
    seen_url: std::sync::Mutex<Option<String>>,
    seen_auth: std::sync::Mutex<Option<String>>,
}

impl Fake {
    fn answer(body: Value) -> Self {
        Self {
            answer: Ok(HttpResponse {
                status: 200,
                body: Some(body),
            }),
            seen_url: std::sync::Mutex::new(None),
            seen_auth: std::sync::Mutex::new(None),
        }
    }

    fn failed() -> Self {
        Self {
            answer: Err("offline".to_owned()),
            seen_url: std::sync::Mutex::new(None),
            seen_auth: std::sync::Mutex::new(None),
        }
    }

    fn url(&self) -> Option<String> {
        self.seen_url.lock().unwrap().clone()
    }

    fn auth(&self) -> Option<String> {
        self.seen_auth.lock().unwrap().clone()
    }
}

impl Fetch for Fake {
    fn fetch(
        &self,
        url: String,
        request: HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, FetchError>> + Send {
        *self.seen_url.lock().unwrap() = Some(url);
        *self.seen_auth.lock().unwrap() = request
            .headers
            .iter()
            .find(|(name, _)| name == "Authorization")
            .map(|(_, value)| value.clone());
        let answer = self.answer.clone();
        async move { answer.map_err(FetchError) }
    }
}

/// A fetch whose body is not JSON, the way a broken endpoint answers.
struct NotJson;

impl Fetch for NotJson {
    fn fetch(
        &self,
        _url: String,
        _request: HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, FetchError>> + Send {
        async {
            Ok(HttpResponse {
                status: 200,
                body: None,
            })
        }
    }
}

/// A fetch that answers a server error.
struct ServerError;

impl Fetch for ServerError {
    fn fetch(
        &self,
        _url: String,
        _request: HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, FetchError>> + Send {
        async {
            Ok(HttpResponse {
                status: 500,
                body: Some(json!({})),
            })
        }
    }
}

#[test]
fn the_token_window_is_read_out_of_the_answer_not_the_tool_one() {
    assert_eq!(
        read_quota(&quota_body(42.0, 1_700_000)),
        Some(Quota {
            percentage: 42.0,
            resets_at: Some(1_700_000),
        })
    );
}

/// A shape that changed must not read as a spent quota: that would stop every
/// session on the host until somebody noticed.
#[test]
fn anything_unrecognised_is_read_as_nothing_rather_than_as_spent() {
    for body in [
        Value::Null,
        json!({}),
        json!({ "data": {} }),
        json!({ "data": { "limits": "no" } }),
        json!("nope"),
    ] {
        assert_eq!(read_quota(&body), None, "{body}");
    }
    assert_eq!(
        read_quota(&json!({
            "data": { "limits": [{ "type": "TOKENS_LIMIT", "percentage": "42" }] },
        })),
        None
    );
    assert_eq!(
        read_quota(&json!({
            "data": { "limits": [{ "type": "OTHER", "percentage": 1, "nextResetTime": 2 }] },
        })),
        None
    );
}

#[test]
fn only_the_provider_that_meters_this_way_is_asked() {
    assert!(meters_usage("zai"));
    assert!(meters_usage("zai-coding-cn"));
    assert!(!meters_usage("anthropic"));
}

/// The key goes raw, not as a bearer token: that is what the endpoint takes.
#[tokio::test]
async fn the_quota_is_asked_for_with_the_key_as_it_is() {
    let fetch = Fake::answer(quota_body(10.0, 5));

    let quota = fetch_quota("secret-key", &fetch, 10_000).await;

    assert_eq!(fetch.url(), Some(QUOTA_URL.to_owned()));
    assert_eq!(fetch.auth(), Some("secret-key".to_owned()));
    assert!(quota.is_some());
}

/// An unreachable provider must leave work running: refusing everything is a
/// far worse way to be wrong than one failed turn.
#[tokio::test]
async fn a_provider_that_cannot_be_reached_says_nothing_not_no() {
    assert_eq!(fetch_quota("k", &Fake::failed(), 10_000).await, None);
    assert_eq!(fetch_quota("k", &ServerError, 10_000).await, None);
    assert_eq!(fetch_quota("k", &NotJson, 10_000).await, None);
}

/// A window nothing has been charged to has nothing scheduled to reset, and
/// z.ai sends null for it. Reading that as no answer cleared the status
/// exactly when the window was emptiest.
#[test]
fn an_untouched_window_is_an_answer_reset_time_or_not() {
    let body = json!({
        "data": {
            "limits": [
                { "type": "TIME_LIMIT", "percentage": 3, "nextResetTime": 1_790_080_917_984_i64 },
                { "type": "TOKENS_LIMIT", "percentage": 0, "nextResetTime": null },
            ],
        },
    });
    let quota = read_quota(&body).expect("an answer");
    assert_eq!(quota.percentage, 0.0);
    assert_eq!(quota.resets_at, None);
}

#[test]
fn a_reset_time_that_is_there_is_still_used() {
    let quota = read_quota(&json!({
        "data": { "limits": [{ "type": "TOKENS_LIMIT", "percentage": 40,
                               "nextResetTime": 1_700_000_000_000_i64 }] },
    }))
    .expect("an answer");

    assert_eq!(quota.resets_at, Some(1_700_000_000_000));
}
