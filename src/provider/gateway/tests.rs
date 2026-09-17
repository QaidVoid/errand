//! Tests for the gateway window, ported from `gateway_test.ts`.

use std::future::Future;

use serde_json::{Value, json};

use super::{USAGE_PATH, fetch_gateway_usage, read_gateway_usage};
use crate::provider::usage::{Fetch, FetchError, HttpRequest, HttpResponse};

/// The shape the gateway actually answers with, trimmed to what is read.
fn usage_body(limiting: &Value) -> Value {
    json!({
        "provider": "muse-code",
        "scope": "key",
        "limiting": limiting,
        "windows": [],
        "key": {},
    })
}

fn open() -> Value {
    json!({
        "status": "ok",
        "id": "window-share:300m",
        "label": "Share of the 5 hours window",
        "peakUsedPercent": 12,
        "lastUsedPercent": 3,
        "remainingPercent": 88,
        "spent": false,
        "resetsAt": "2026-09-16T08:39:43.000Z",
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
            answer: Err("x".to_owned()),
            seen_url: std::sync::Mutex::new(None),
            seen_auth: std::sync::Mutex::new(None),
        }
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

/// A fetch that answers a server error with a body that is not JSON.
struct Unavailable;

impl Fetch for Unavailable {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the trait is async; a stand-in that answers at once still has to match it"
    )]
    async fn fetch(&self, _url: String, _request: HttpRequest) -> Result<HttpResponse, FetchError> {
        Ok(HttpResponse {
            status: 503,
            body: None,
        })
    }
}

#[expect(
    clippy::float_cmp,
    reason = "the fixtures hold values a f64 holds exactly"
)]
#[test]
fn an_open_window_is_read_as_what_has_been_used_of_it() {
    let quota = read_gateway_usage(&usage_body(&open())).expect("an answer");
    assert_eq!(quota.percentage, 12.0);
    assert_eq!(
        quota.resets_at,
        Some(
            "2026-09-16T08:39:43.000Z"
                .parse::<jiff::Timestamp>()
                .expect("a timestamp")
                .as_millisecond()
        )
    );
}

/// The gateway says outright, and that beats the arithmetic.
#[expect(
    clippy::float_cmp,
    reason = "the fixtures hold values a f64 holds exactly"
)]
#[test]
fn a_spent_window_is_spent_whatever_the_percentages_say() {
    let mut limiting = open();
    limiting["spent"] = json!(true);
    limiting["peakUsedPercent"] = json!(97);
    limiting["remainingPercent"] = json!(3);
    let quota = read_gateway_usage(&usage_body(&limiting)).expect("an answer");

    assert_eq!(quota.percentage, 100.0);
}

/// Rounding must not close a window the gateway left open.
#[test]
fn an_unspent_window_never_reads_as_full() {
    let mut limiting = open();
    limiting["spent"] = json!(false);
    limiting["peakUsedPercent"] = json!(100);
    limiting["remainingPercent"] = json!(0);
    let quota = read_gateway_usage(&usage_body(&limiting)).expect("an answer");

    assert!(quota.percentage < 100.0);
}

/// An unsure answer must not read as a spent window: that would stop every
/// session on this host until somebody noticed.
#[test]
fn an_answer_the_gateway_is_unsure_of_is_no_answer() {
    let mut stale = open();
    stale["status"] = json!("stale");
    assert_eq!(read_gateway_usage(&usage_body(&stale)), None);

    let mut unknown = open();
    unknown["status"] = json!("unknown");
    assert_eq!(read_gateway_usage(&usage_body(&unknown)), None);

    let mut no_reset = open();
    no_reset["resetsAt"] = Value::Null;
    assert_eq!(read_gateway_usage(&usage_body(&no_reset)), None);

    let mut bad_reset = open();
    bad_reset["resetsAt"] = json!("not a time");
    assert_eq!(read_gateway_usage(&usage_body(&bad_reset)), None);

    assert_eq!(read_gateway_usage(&json!({})), None);
    assert_eq!(read_gateway_usage(&Value::Null), None);

    // Neither percentage present, so there is nothing to report.
    assert_eq!(
        read_gateway_usage(&usage_body(&json!({
            "status": "ok",
            "spent": false,
            "resetsAt": "2026-09-16T08:39:43.000Z",
        }))),
        None
    );
}

#[expect(
    clippy::float_cmp,
    reason = "the fixtures hold values a f64 holds exactly"
)]
#[tokio::test]
async fn usage_is_asked_for_beside_the_base_url_with_the_key() {
    let fetch = Fake::answer(usage_body(&open()));

    let window = fetch_gateway_usage("https://gateway.example/v1/", "the-key", &fetch, 10_000)
        .await
        .expect("an answer");

    // One slash, whether or not the base url ended in one.
    assert_eq!(
        fetch.seen_url.lock().unwrap().clone(),
        Some(format!("https://gateway.example/v1{USAGE_PATH}"))
    );
    assert_eq!(
        fetch.seen_auth.lock().unwrap().clone(),
        Some("Bearer the-key".to_owned())
    );
    assert_eq!(window.percentage, 12.0);
}

/// A gateway that cannot be reached must leave work running.
#[tokio::test]
async fn a_gateway_that_does_not_answer_is_not_a_spent_window() {
    assert_eq!(
        fetch_gateway_usage("https://gateway.example/v1", "k", &Fake::failed(), 10_000).await,
        None
    );
    assert_eq!(
        fetch_gateway_usage("https://gateway.example/v1", "k", &Unavailable, 10_000).await,
        None
    );
}
