//! Tests for drawing a picture over the Images API.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::{Value, json};

use super::{
    Drawn, Endpoint, ImageError, Post, Request, Response, decode_base64, draw, endpoint_with_effort,
};
use crate::provider::models::AvailableModel;

/// Builds an agent configuration from the settings a test writes.
fn agent(agent: &Value) -> crate::config::schema::AgentConfig {
    let settings = json!({
        "chat": {
            "token": "a.token.value",
            "channelId": "chan",
            "allowedUserIds": ["100000000000000001"],
        },
        "agent": agent,
        "projectRoot": "/tmp/errand-projects",
        "stateDir": "/tmp/errand-state",
    });
    crate::config::validate::validate_config(&settings)
        .expect("the test configuration is accepted")
        .agent
}

/// A PNG's signature, which is what a real answer carries.
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];

/// Encodes bytes the way a server answering `b64_json` does.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let mut combined = 0u32;
        for (at, &byte) in chunk.iter().enumerate() {
            combined |= u32::from(byte) << (16 - 8 * at);
        }
        let digits = chunk.len() + 1;
        for at in 0..4 {
            let index = ((combined >> (18 - 6 * at)) & 0x3F) as usize;
            if at < digits {
                out.push(char::from(ALPHABET[index]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A server that answers in order, recording what it was asked.
struct FakePost {
    answers: Mutex<Vec<Result<Response, String>>>,
    sent: Mutex<Vec<(String, Request)>>,
}

impl FakePost {
    fn answering(answer: Result<Response, String>) -> Self {
        Self {
            answers: Mutex::new(vec![answer]),
            sent: Mutex::new(Vec::new()),
        }
    }

    fn answering_in_order(answers: Vec<Result<Response, String>>) -> Self {
        Self {
            answers: Mutex::new(answers),
            sent: Mutex::new(Vec::new()),
        }
    }

    fn sent(&self) -> Vec<(String, Request)> {
        self.sent.lock().unwrap().clone()
    }
}

impl Post for FakePost {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the trait's method is async, and a fake answers at once"
    )]
    async fn post(&self, url: String, request: Request) -> Result<Response, String> {
        self.sent.lock().unwrap().push((url, request));
        self.answers.lock().unwrap().remove(0)
    }
}

fn endpoint() -> Endpoint {
    Endpoint {
        base_url: "https://images.example/v1".to_owned(),
        model: "image:max".to_owned(),
        credential: "a-key".to_owned(),
    }
}

fn answered(body: Value) -> Response {
    Response {
        status: 200,
        body: Some(body),
    }
}

#[tokio::test]
async fn a_picture_is_drawn_at_the_images_endpoint_with_the_model_and_prompt() {
    let post = FakePost::answering(Ok(answered(json!({
        "created": 1_792_643_618,
        "data": [{ "b64_json": base64(PNG) }],
    }))));

    let drawn = draw(&endpoint(), "a lighthouse", &post)
        .await
        .expect("a picture");

    assert_eq!(drawn.bytes, PNG);
    let sent = post.sent();
    assert_eq!(sent.len(), 1);
    let (url, request) = &sent[0];
    // The endpoint the report named, not a chat one.
    assert_eq!(url, "https://images.example/v1/images/generations");
    let body: Value = serde_json::from_str(&request.body).expect("JSON");
    assert_eq!(body["model"], "image:max");
    assert_eq!(body["prompt"], "a lighthouse");
    assert_eq!(body["response_format"], "b64_json");
    assert!(
        request
            .headers
            .contains(&("Authorization".to_owned(), "Bearer a-key".to_owned()))
    );
}

/// A trailing slash on the base URL must not double up in the path.
#[tokio::test]
async fn a_trailing_slash_on_the_base_url_is_not_doubled() {
    let post = FakePost::answering(Ok(answered(
        json!({ "data": [{ "b64_json": base64(PNG) }] }),
    )));
    let mut base = endpoint();
    base.base_url = "https://images.example/v1/".to_owned();

    draw(&base, "a lighthouse", &post).await.expect("a picture");

    assert_eq!(
        post.sent()[0].0,
        "https://images.example/v1/images/generations"
    );
}

/// `OpenAI`'s own image models refuse `response_format`, so the call is asked
/// again without it rather than failing over a parameter that is optional.
#[tokio::test]
async fn a_refusal_naming_response_format_is_asked_again_without_it() {
    let post = FakePost::answering_in_order(vec![
        Ok(Response {
            status: 400,
            body: Some(json!({ "error": { "message": "unknown parameter: response_format" } })),
        }),
        Ok(answered(json!({ "data": [{ "b64_json": base64(PNG) }] }))),
    ]);

    let drawn = draw(&endpoint(), "a lighthouse", &post)
        .await
        .expect("a picture");

    assert_eq!(drawn.bytes, PNG);
    let sent = post.sent();
    assert_eq!(sent.len(), 2, "asked once, then again without it");
    let second: Value = serde_json::from_str(&sent[1].1.body).expect("JSON");
    assert!(second.get("response_format").is_none(), "{second}");
    // Everything else about the call is unchanged.
    assert_eq!(second["model"], "image:max");
    assert_eq!(second["prompt"], "a lighthouse");
}

/// A link is not bytes: following it would be a second request to a host
/// nobody configured.
#[tokio::test]
async fn a_link_instead_of_bytes_is_refused() {
    let post = FakePost::answering(Ok(answered(json!({
        "data": [{ "url": "https://images.example/a.png" }],
    }))));

    let refused = draw(&endpoint(), "a lighthouse", &post)
        .await
        .expect_err("no bytes");

    assert!(refused.0.contains("no image"), "{}", refused.0);
}

#[tokio::test]
async fn a_refusal_carries_the_servers_own_message() {
    let post = FakePost::answering(Ok(Response {
        status: 402,
        body: Some(json!({ "error": { "message": "no credit left" } })),
    }));

    let refused = draw(&endpoint(), "a lighthouse", &post)
        .await
        .expect_err("refused");

    assert!(refused.0.contains("no credit left"), "{}", refused.0);
    assert!(refused.0.contains("image:max"), "{}", refused.0);
}

#[tokio::test]
async fn an_unreachable_endpoint_says_so() {
    let post = FakePost::answering(Err("connection refused".to_owned()));

    let refused = draw(&endpoint(), "a lighthouse", &post)
        .await
        .expect_err("unreachable");

    assert!(refused.0.contains("could not be reached"), "{}", refused.0);
}

/// The server's own rewrite of the prompt is kept, so a reader can see what
/// was actually drawn.
#[tokio::test]
async fn a_revised_prompt_is_kept() {
    let post = FakePost::answering(Ok(answered(json!({
        "data": [{ "b64_json": base64(PNG), "revised_prompt": "  a tall lighthouse  " }],
    }))));

    let drawn = draw(&endpoint(), "a lighthouse", &post)
        .await
        .expect("a picture");

    assert_eq!(drawn.revised_prompt.as_deref(), Some("a tall lighthouse"));
}

#[test]
fn base64_round_trips_padded_and_unpadded() {
    assert_eq!(decode_base64(&base64(b"hello")), Some(b"hello".to_vec()));
    assert_eq!(decode_base64("aGVsbG8"), Some(b"hello".to_vec()));
    assert_eq!(decode_base64("aGVsbG8="), Some(b"hello".to_vec()));
    // A character no base64 alphabet holds is not guessed at.
    assert_eq!(decode_base64("not base64!"), None);
}

/// Nothing worth posting is reported as a failure rather than as an empty
/// picture.
#[test]
fn an_empty_payload_is_no_picture() {
    let body = json!({ "data": [{ "b64_json": "" }] });
    assert!(super::read(&body).is_none());
}

/// A drawn picture is the bytes the server sent, so a caption can name its
/// type.
#[test]
fn a_drawn_picture_keeps_the_bytes_the_server_sent() {
    let body = json!({ "data": [{ "b64_json": base64(PNG) }] });
    let drawn: Drawn = super::read(&body).expect("a picture");
    assert_eq!(drawn.bytes, PNG);
    assert!(drawn.revised_prompt.is_none());
}

/// The real HTTP poster reaches the images path on the configured host, with
/// the model and prompt in the body. Proved against a listener rather than a
/// fake, since the path and the body are what the endpoint is asked for.
#[tokio::test]
async fn the_real_poster_reaches_the_images_path_with_the_model_and_prompt() {
    use std::sync::Arc;

    let seen: Arc<Mutex<Vec<(String, String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let app = axum::Router::new().route(
        "/v1/images/generations",
        axum::routing::post(move |headers: axum::http::HeaderMap, body: String| {
            let recorded = Arc::clone(&recorded);
            async move {
                let authorization = headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned();
                recorded.lock().unwrap().push((
                    "/v1/images/generations".to_owned(),
                    authorization,
                    body,
                ));
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    format!(
                        r#"{{"created":1,"data":[{{"b64_json":"{}"}}]}}"#,
                        base64(PNG)
                    ),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bound");
    let port = listener.local_addr().expect("an address").port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serves");
    });

    let mut base = endpoint();
    base.base_url = format!("http://127.0.0.1:{port}/v1");
    let drawn = draw(&base, "a lighthouse", &super::HttpPost)
        .await
        .expect("a picture");

    assert_eq!(drawn.bytes, PNG);
    let seen = seen.lock().unwrap();
    let (path, authorization, body) = seen.first().expect("the endpoint was reached");
    assert_eq!(path, "/v1/images/generations");
    assert_eq!(authorization, "Bearer a-key");
    let body: Value = serde_json::from_str(body).expect("JSON");
    assert_eq!(body["model"], "image:max");
    assert_eq!(body["prompt"], "a lighthouse");
}

/// A picture too large for the surface is refused, and the refusal names the
/// size and the limit so whoever reads it knows which to change.
#[test]
fn a_picture_over_the_upload_limit_is_refused() {
    let drawn = Drawn {
        bytes: PNG.to_vec(),
        revised_prompt: None,
    };

    let refused = drawn
        .into_picture("image:max", 4)
        .expect_err("over the limit");

    assert!(refused.0.contains("over the 4 byte"), "{}", refused.0);
}

/// Bytes that are not an image a channel shows are refused rather than
/// posted as a file nobody can see.
#[test]
fn bytes_that_are_not_an_image_are_refused() {
    let drawn = Drawn {
        bytes: b"not a picture".to_vec(),
        revised_prompt: None,
    };

    let refused = drawn
        .into_picture("image:max", u64::MAX)
        .expect_err("not an image");

    assert!(refused.0.contains("not a PNG"), "{}", refused.0);
}

/// The file is named for the type its own bytes declare, and captioned with
/// the model that drew it.
#[test]
fn a_picture_is_named_for_its_type_and_captioned_with_its_model() {
    let drawn = Drawn {
        bytes: PNG.to_vec(),
        revised_prompt: Some("a tall lighthouse".to_owned()),
    };

    let picture = drawn
        .into_picture("image:max", u64::MAX)
        .expect("a picture");

    assert_eq!(picture.name, "image.png");
    assert_eq!(picture.caption, "drawn on `image:max`: a tall lighthouse");
}

/// The error is what a thread is told, so it has to read as a sentence.
#[test]
fn an_error_is_the_message_a_thread_reads() {
    let error = ImageError("it could not be reached".to_owned());
    assert_eq!(error.to_string(), "it could not be reached");
}

/// The endpoint is the provider that serves the named model, which is not
/// always the one a session runs on.
#[test]
fn the_endpoint_is_the_provider_that_serves_the_named_model() {
    let configured = agent(&json!({
        "provider": "anthropic",
        "imageGen": "pictures/pic:max",
        "providers": {
            "anthropic": { "credential": "secret" },
            "pictures": {
                "credential": "picture-key",
                "baseUrl": "https://images.example/v1",
            },
        },
    }));

    let endpoint =
        endpoint_with_effort(&configured, &[], "pictures/pic:max", &BTreeMap::new(), None)
            .expect("an endpoint");

    assert_eq!(endpoint.base_url, "https://images.example/v1");
    assert_eq!(endpoint.model, "pic:max");
    assert_eq!(endpoint.credential, "picture-key");
}

/// A short name stands for the same model here as it does at a start.
#[test]
fn a_short_name_is_expanded_before_it_is_looked_up() {
    let configured = agent(&json!({
        "provider": "anthropic",
        "providers": {
            "anthropic": { "credential": "secret" },
            "pictures": {
                "credential": "k",
                "baseUrl": "https://images.example/v1",
            },
        },
        "aliases": { "pic": "pictures/image-large" },
    }));

    let endpoint = endpoint_with_effort(&configured, &[], "pic", &configured.aliases, None)
        .expect("an endpoint");

    assert_eq!(endpoint.model, "image-large");
    assert_eq!(endpoint.base_url, "https://images.example/v1");
}

/// A bare name is served by the configured provider.
#[test]
fn a_bare_name_is_served_by_the_configured_provider() {
    let configured = agent(&json!({
        "provider": "pictures",
        "providers": {
            "pictures": {
                "credential": "k",
                "baseUrl": "https://images.example/v1",
            },
        },
    }));

    let endpoint = endpoint_with_effort(&configured, &[], "image-small", &BTreeMap::new(), None)
        .expect("an endpoint");

    assert_eq!(endpoint.base_url, "https://images.example/v1");
    assert_eq!(endpoint.model, "image-small");
}

/// A bare image model is served by whichever provider the catalog lists it
/// on, not by the provider a session runs on.
///
/// This is the bug that made `!gen-image` reach a chat host: `imageGen` named
/// `image:max` bare, the session ran on `pg-oc`, and the request went to that
/// host's `/images/generations`, which it does not serve.
#[test]
fn a_bare_image_model_is_served_by_the_provider_that_lists_it() {
    let configured = agent(&json!({
        "provider": "pg-oc",
        "providers": {
            "pg-oc": {
                "credential": "chat-key",
                "baseUrl": "https://oc-free.pg.ai.ajam.dev",
            },
            "pg-image": {
                "credential": "image-key",
                "baseUrl": "https://image.pg.ai.ajam.dev/v1",
                "images": { "model": "image" },
            },
        },
    }));
    let available = vec![
        AvailableModel {
            provider: "pg-oc".to_owned(),
            id: "free".to_owned(),
            default_level: None,
        },
        AvailableModel {
            provider: "pg-image".to_owned(),
            id: "image:max".to_owned(),
            default_level: None,
        },
    ];

    let endpoint =
        endpoint_with_effort(&configured, &available, "image:max", &BTreeMap::new(), None)
            .expect("an endpoint");

    // The images host, with its own key, and the level kept on the model.
    assert_eq!(endpoint.base_url, "https://image.pg.ai.ajam.dev/v1");
    assert_eq!(endpoint.model, "image:max");
    assert_eq!(endpoint.credential, "image-key");
}

/// A name the catalog serves from more than one provider is refused rather
/// than sent to whichever one was looked at first, when the provider a
/// session runs on is not one of them.
#[test]
fn a_name_two_providers_serve_is_refused_with_both_named() {
    let configured = agent(&json!({
        "provider": "chat",
        "providers": {
            "chat": { "credential": "k", "baseUrl": "https://chat.example/v1" },
            "one": { "credential": "k", "baseUrl": "https://one.example/v1" },
            "two": { "credential": "k", "baseUrl": "https://two.example/v1" },
        },
    }));
    let available = vec![
        AvailableModel {
            provider: "one".to_owned(),
            id: "image".to_owned(),
            default_level: None,
        },
        AvailableModel {
            provider: "two".to_owned(),
            id: "image".to_owned(),
            default_level: None,
        },
    ];

    let refused = endpoint_with_effort(&configured, &available, "image", &BTreeMap::new(), None)
        .expect_err("two providers serve it");

    assert!(
        refused.0.contains("more than one provider"),
        "{}",
        refused.0
    );
    assert!(refused.0.contains("one/image"), "{}", refused.0);
    assert!(refused.0.contains("two/image"), "{}", refused.0);
}

/// When the provider a session runs on serves the name too, that one is used:
/// staying where you are is the ordinary case and needs no qualification.
#[test]
fn the_provider_the_session_runs_on_is_preferred_when_it_serves_the_name() {
    let configured = agent(&json!({
        "provider": "one",
        "providers": {
            "one": { "credential": "k", "baseUrl": "https://one.example/v1" },
            "two": { "credential": "k", "baseUrl": "https://two.example/v1" },
        },
    }));
    let available = vec![
        AvailableModel {
            provider: "one".to_owned(),
            id: "image".to_owned(),
            default_level: None,
        },
        AvailableModel {
            provider: "two".to_owned(),
            id: "image".to_owned(),
            default_level: None,
        },
    ];

    let endpoint = endpoint_with_effort(&configured, &available, "image", &BTreeMap::new(), None)
        .expect("an endpoint");

    assert_eq!(endpoint.base_url, "https://one.example/v1");
    assert_eq!(endpoint.model, "image");
}

/// The catalog is what makes a bare name resolvable, and it is built from the
/// providers' own `models` lists. Proved through `available_models` rather
/// than a hand-built list, since that is what the daemon actually passes in.
#[test]
fn a_bare_image_name_resolves_through_the_catalog_the_daemon_builds() {
    let configured = agent(&json!({
        "provider": "pg-oc",
        "providers": {
            "pg-oc": {
                "credential": "chat-key",
                "baseUrl": "https://oc-free.pg.ai.ajam.dev",
            },
            "pg-image": {
                "credential": "image-key",
                "baseUrl": "https://image.pg.ai.ajam.dev/v1",
                "images": { "model": "image" },
                "models": [
                    { "id": "image", "input": ["text"], "produces": ["text", "image"] },
                    { "id": "image:max", "input": ["text"], "produces": ["text", "image"] },
                ],
            },
        },
    }));
    let available = crate::provider::models::available_models(&configured, None);

    let endpoint =
        endpoint_with_effort(&configured, &available, "image:max", &BTreeMap::new(), None)
            .expect("an endpoint");

    assert_eq!(endpoint.base_url, "https://image.pg.ai.ajam.dev/v1");
    assert_eq!(endpoint.model, "image:max");
    assert_eq!(endpoint.credential, "image-key");
}

/// An effort asked for on the command goes on the model id, which is where
/// the endpoint reads it.
#[test]
fn an_effort_asked_for_goes_on_the_model_id() {
    let configured = agent(&json!({
        "provider": "pg-oc",
        "providers": {
            "pg-oc": { "credential": "k", "baseUrl": "https://oc.example" },
            "pg-image": {
                "credential": "k",
                "baseUrl": "https://image.pg.ai.ajam.dev/v1",
                "models": [{ "id": "image", "produces": ["text", "image"] }],
            },
        },
    }));
    let available = vec![AvailableModel {
        provider: "pg-image".to_owned(),
        id: "image".to_owned(),
        default_level: None,
    }];

    let endpoint = endpoint_with_effort(
        &configured,
        &available,
        "image",
        &BTreeMap::new(),
        Some("max"),
    )
    .expect("an endpoint");

    assert_eq!(endpoint.model, "image:max");
}

/// An effort replaces the level the name already carries rather than stacking
/// a second colon beside it.
#[test]
fn an_effort_replaces_a_level_the_name_already_carries() {
    let configured = agent(&json!({
        "provider": "pg-oc",
        "providers": {
            "pg-oc": { "credential": "k", "baseUrl": "https://oc.example" },
            "pg-image": {
                "credential": "k",
                "baseUrl": "https://image.pg.ai.ajam.dev/v1",
                "models": [{ "id": "image:high", "produces": ["text", "image"] }],
            },
        },
    }));
    let available = vec![AvailableModel {
        provider: "pg-image".to_owned(),
        id: "image:high".to_owned(),
        default_level: None,
    }];

    let endpoint = endpoint_with_effort(
        &configured,
        &available,
        "image:high",
        &BTreeMap::new(),
        Some("low"),
    )
    .expect("an endpoint");

    assert_eq!(endpoint.model, "image:low");
}

/// With no effort asked for, the level the name carries is kept, which is how
/// the configuration's own level survives.
#[test]
fn no_effort_keeps_the_level_the_name_carries() {
    let configured = agent(&json!({
        "provider": "pg-oc",
        "providers": {
            "pg-oc": { "credential": "k", "baseUrl": "https://oc.example" },
            "pg-image": {
                "credential": "k",
                "baseUrl": "https://image.pg.ai.ajam.dev/v1",
                "models": [{ "id": "image:max", "produces": ["text", "image"] }],
            },
        },
    }));
    let available = vec![AvailableModel {
        provider: "pg-image".to_owned(),
        id: "image:max".to_owned(),
        default_level: None,
    }];

    let endpoint =
        endpoint_with_effort(&configured, &available, "image:max", &BTreeMap::new(), None)
            .expect("an endpoint");

    assert_eq!(endpoint.model, "image:max");
}

/// A provider with no endpoint has nowhere to send the request, and saying
/// that is better than attempting it against an empty URL.
#[test]
fn a_provider_without_an_endpoint_is_refused() {
    let configured = agent(&json!({
        "provider": "pictures",
        "providers": { "pictures": { "credential": "k" } },
    }));

    let refused = endpoint_with_effort(&configured, &[], "image-small", &BTreeMap::new(), None)
        .expect_err("no endpoint");

    assert!(
        refused.0.contains("no endpoint is configured"),
        "{}",
        refused.0
    );
    assert!(refused.0.contains("baseUrl"), "{}", refused.0);
}

/// A provider with no credential is still reachable, since a self-hosted
/// server may need none.
#[test]
fn a_provider_without_a_credential_sends_none() {
    let configured = agent(&json!({
        "provider": "anthropic",
        "providers": {
            "anthropic": { "credential": "secret" },
            "local": { "baseUrl": "http://localhost:8080/v1" },
        },
    }));

    let endpoint = endpoint_with_effort(
        &configured,
        &[],
        "local/image-small",
        &BTreeMap::new(),
        None,
    )
    .expect("an endpoint");

    assert_eq!(endpoint.base_url, "http://localhost:8080/v1");
    assert_eq!(endpoint.credential, "");
}
