//! Tests for asking providers which models they serve.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Mutex;

use serde_json::{Value, json};

use super::{discover, read_models, with_discovered};
use crate::config::discover::Discovery;
use crate::config::validate::validate_config;
use crate::provider::models::AvailableModel;
use crate::provider::usage::{Fetch, FetchError, HttpRequest, HttpResponse};

/// Answers each URL from a table, and records the key each was asked with.
struct Fake {
    answers: BTreeMap<String, HttpResponse>,
    keys: Mutex<Vec<(String, String)>>,
}

impl Fetch for Fake {
    fn fetch(
        &self,
        url: String,
        request: HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, FetchError>> + Send {
        if let Some((_, key)) = request
            .headers
            .iter()
            .find(|(name, _)| name == "Authorization")
        {
            self.keys.lock().unwrap().push((url.clone(), key.clone()));
        }
        let answer = self
            .answers
            .get(&url)
            .cloned()
            .ok_or_else(|| FetchError("connection refused".to_owned()));
        async move { answer }
    }
}

fn gateway_listing() -> Value {
    json!({
        "object": "list",
        "data": [{
            "id": "glm-5.3-flash",
            "object": "model",
            "context_window": 1_000_000,
            "max_output_tokens": 128_000,
            "provider": "zai",
        }],
    })
}

fn asked(discover: &Value) -> Discovery {
    Discovery::of("p", &json!({ "discover": discover }))
        .expect("valid")
        .expect("asked")
}

#[test]
fn a_listing_is_read_into_the_agents_own_field_names() {
    let found = read_models(
        &gateway_listing(),
        &asked(&json!({ "defaults": { "reasoning": true, "contextWindow": 1 } })),
    )
    .expect("read");

    assert_eq!(
        found,
        [json!({
            "id": "glm-5.3-flash",
            "reasoning": true,
            "contextWindow": 1_000_000,
            "maxTokens": 128_000,
        })]
    );
}

#[test]
fn a_listing_shaped_otherwise_is_read_where_the_definition_says() {
    let body = json!({ "result": { "models": [
        { "slug": "big", "top_provider": { "context_length": 64_000 } },
        { "note": "no id, so nothing to switch to" },
    ] } });
    let found = read_models(
        &body,
        &asked(&json!({
            "list": "result.models",
            "fields": { "id": "slug", "contextWindow": "top_provider.context_length" },
        })),
    )
    .expect("read");

    assert_eq!(found, [json!({ "id": "big", "contextWindow": 64_000 })]);
    assert!(read_models(&json!({ "data": {} }), &asked(&json!(true))).is_err());
}

/// An entry written for a model overrides the fetched fields it names and
/// keeps the rest; one nobody fetched is kept; one only fetched is added.
#[test]
fn a_written_entry_is_laid_over_what_was_found() {
    let definition = json!({
        "baseUrl": "https://gateway.example/v1",
        "models": [
            { "id": "glm-5.3-flash", "contextWindow": 256_000, "reasoning": true },
            { "id": "spark-max" },
        ],
    });
    let found = [
        json!({ "id": "glm-5.3-flash", "contextWindow": 1_000_000, "maxTokens": 128_000 }),
        json!({ "id": "muse-spark" }),
    ];

    assert_eq!(
        with_discovered(&definition, &found)["models"],
        json!([
            { "id": "glm-5.3-flash", "contextWindow": 256_000, "maxTokens": 128_000, "reasoning": true },
            { "id": "spark-max" },
            { "id": "muse-spark" },
        ])
    );
}

/// Only providers that say to are asked, with their own key, and one that
/// cannot be reached keeps the models its definition names.
#[tokio::test]
async fn every_provider_that_says_to_is_asked_and_a_failure_keeps_its_own() {
    let config = validate_config(&json!({
        "chat": { "token": "t", "channelId": "c", "allowedUserIds": ["u"] },
        "agent": {
            "provider": "gateway",
            "providers": {
                "gateway": {
                    "baseUrl": "https://gateway.example/v1",
                    "credential": "g-key",
                    "discover": true,
                },
                "down": {
                    "baseUrl": "https://down.example/v1",
                    "credential": "d-key",
                    "discover": true,
                    "models": [{ "id": "kept" }],
                },
                "quiet": { "baseUrl": "https://quiet.example/v1", "credential": "q-key" },
            },
        },
        "projectRoot": "/tmp/p",
        "stateDir": "/tmp/s",
    }))
    .expect("resolves");
    let fetch = Fake {
        answers: BTreeMap::from([(
            "https://gateway.example/v1/models".to_owned(),
            HttpResponse {
                status: 200,
                body: Some(gateway_listing()),
            },
        )]),
        keys: Mutex::new(Vec::new()),
    };

    let (providers, models, outcomes) = discover(&config.agent, None, &fetch, 1_000).await;

    assert_eq!(
        providers["gateway"]["models"],
        json!([{ "id": "glm-5.3-flash", "contextWindow": 1_000_000, "maxTokens": 128_000 }])
    );
    assert_eq!(providers["down"]["models"], json!([{ "id": "kept" }]));
    let switchable: Vec<String> = models.iter().map(AvailableModel::qualified).collect();
    assert!(switchable.contains(&"gateway/glm-5.3-flash".to_owned()));
    assert!(switchable.contains(&"down/kept".to_owned()));

    assert_eq!(outcomes.len(), 2, "quiet is never asked");
    assert_eq!(outcomes[0], ("gateway".to_owned(), Ok(1)));
    assert!(
        outcomes[1]
            .1
            .as_ref()
            .is_err_and(|why| why.contains("connection refused"))
    );
    assert_eq!(
        *fetch.keys.lock().unwrap(),
        [
            (
                "https://gateway.example/v1/models".to_owned(),
                "Bearer g-key".to_owned()
            ),
            (
                "https://down.example/v1/models".to_owned(),
                "Bearer d-key".to_owned()
            ),
        ]
    );
}
