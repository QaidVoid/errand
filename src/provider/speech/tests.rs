//! Tests for reading a text aloud over the speech shape.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::{Value, json};

use super::{Answer, Endpoint, Post, Request, SpeechError, Spoken, endpoint, speak};
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

/// An MP3: an ID3 tag followed by an MPEG-1 Layer III frame header.
const MP3: &[u8] = &[
    b'I', b'D', 0x33, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFB, 0x90, 0x64,
];

/// A server that answers in order, recording what it was asked.
struct FakePost {
    answers: Mutex<Vec<Result<Answer, String>>>,
    sent: Mutex<Vec<(String, Request)>>,
}

impl FakePost {
    fn answering(answer: Result<Answer, String>) -> Self {
        Self {
            answers: Mutex::new(vec![answer]),
            sent: Mutex::new(Vec::new()),
        }
    }

    fn answering_in_order(answers: Vec<Result<Answer, String>>) -> Self {
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
    async fn post(&self, url: String, request: Request) -> Result<Answer, String> {
        self.sent.lock().unwrap().push((url, request));
        self.answers.lock().unwrap().remove(0)
    }
}

fn endpoint_of() -> Endpoint {
    Endpoint {
        base_url: "https://speech.example/v1".to_owned(),
        model: "tts".to_owned(),
        credential: "a-key".to_owned(),
    }
}

fn answered(bytes: &[u8]) -> Answer {
    Answer {
        status: 200,
        bytes: bytes.to_vec(),
    }
}

fn refused(status: u16, body: &Value) -> Answer {
    Answer {
        status,
        bytes: body.to_string().into_bytes(),
    }
}

#[tokio::test]
async fn a_text_is_read_at_the_speech_endpoint_with_the_model_and_text() {
    let post = FakePost::answering(Ok(answered(MP3)));

    let spoken = speak(
        &endpoint_of(),
        "a red kite",
        Some("alloy"),
        Some("en"),
        &post,
    )
    .await
    .expect("a clip");

    assert_eq!(spoken.bytes, MP3);
    assert_eq!(spoken.extension, "mp3");
    let sent = post.sent();
    assert_eq!(sent.len(), 1);
    let (url, request) = &sent[0];
    assert_eq!(url, "https://speech.example/v1/audio/speech");
    let body: Value = serde_json::from_str(&request.body).expect("JSON");
    assert_eq!(body["model"], "tts");
    assert_eq!(body["input"], "a red kite");
    assert_eq!(body["voice"], "alloy");
    assert_eq!(body["language"], "en");
    assert_eq!(body["response_format"], "mp3");
    assert!(
        request
            .headers
            .contains(&("Authorization".to_owned(), "Bearer a-key".to_owned()))
    );
}

/// A trailing slash on the base URL must not double up in the path.
#[tokio::test]
async fn a_trailing_slash_on_the_base_url_is_not_doubled() {
    let post = FakePost::answering(Ok(answered(MP3)));
    let mut base = endpoint_of();
    base.base_url = "https://speech.example/v1/".to_owned();

    speak(&base, "a red kite", None, None, &post)
        .await
        .expect("a clip");

    assert_eq!(post.sent()[0].0, "https://speech.example/v1/audio/speech");
}

/// No voice asked for sends none, and no language asked for sends none.
#[tokio::test]
async fn no_voice_and_no_language_send_none() {
    let post = FakePost::answering(Ok(answered(MP3)));

    speak(&endpoint_of(), "a red kite", None, None, &post)
        .await
        .expect("a clip");

    let body: Value = serde_json::from_str(&post.sent()[0].1.body).expect("JSON");
    assert!(body.get("voice").is_none(), "{body}");
    assert!(body.get("language").is_none(), "{body}");
    assert_eq!(body["response_format"], "mp3");
}

/// A server that refuses `language` is asked again without it, which is how
/// a server that does not take the hint field is worked with.
#[tokio::test]
async fn a_refusal_naming_language_is_asked_again_without_it() {
    let post = FakePost::answering_in_order(vec![
        Ok(refused(
            400,
            &json!({ "error": { "message": "unknown parameter: 'language'.", "param": "language" } }),
        )),
        Ok(answered(MP3)),
    ]);

    let spoken = speak(&endpoint_of(), "a red kite", None, Some("en"), &post)
        .await
        .expect("a clip");

    assert_eq!(spoken.bytes, MP3);
    let sent = post.sent();
    assert_eq!(sent.len(), 2, "asked once, then again without it");
    let second: Value = serde_json::from_str(&sent[1].1.body).expect("JSON");
    assert!(second.get("language").is_none(), "{second}");
    // Everything else about the call is unchanged.
    assert_eq!(second["model"], "tts");
    assert_eq!(second["response_format"], "mp3");
}

/// The message naming the field is enough, when the body carries no
/// `error.param` to read.
#[tokio::test]
async fn a_refusal_whose_message_names_language_is_asked_again_without_it() {
    let post = FakePost::answering_in_order(vec![
        Ok(refused(
            400,
            &json!({ "error": { "message": "unknown parameter: 'language'." } }),
        )),
        Ok(answered(MP3)),
    ]);

    speak(&endpoint_of(), "a red kite", None, Some("en"), &post)
        .await
        .expect("a clip");

    assert_eq!(post.sent().len(), 2);
}

/// `response_format` is asked for as `mp3`, and a server that refuses it is
/// asked again without it rather than failing over a parameter that is
/// optional.
#[tokio::test]
async fn a_refusal_naming_response_format_is_asked_again_without_it() {
    let post = FakePost::answering_in_order(vec![
        Ok(refused(
            400,
            &json!({ "error": { "message": "unknown parameter", "param": "response_format" } }),
        )),
        Ok(answered(MP3)),
    ]);

    let spoken = speak(&endpoint_of(), "a red kite", None, None, &post)
        .await
        .expect("a clip");

    assert_eq!(spoken.bytes, MP3);
    let sent = post.sent();
    assert_eq!(sent.len(), 2);
    let second: Value = serde_json::from_str(&sent[1].1.body).expect("JSON");
    assert!(second.get("response_format").is_none(), "{second}");
    assert_eq!(second["input"], "a red kite");
}

/// Fields refused one at a time are each dropped, which is the two retries
/// the rule allows.
#[tokio::test]
async fn fields_refused_one_at_a_time_are_each_dropped() {
    let refusing = |field: &str| {
        refused(
            400,
            &json!({ "error": { "message": format!("unknown parameter: '{field}'.") } }),
        )
    };
    let post = FakePost::answering_in_order(vec![
        Ok(refusing("response_format")),
        Ok(refusing("language")),
        Ok(answered(MP3)),
    ]);

    let spoken = speak(&endpoint_of(), "a red kite", None, Some("en"), &post)
        .await
        .expect("a clip");

    assert_eq!(spoken.bytes, MP3);
    let sent = post.sent();
    assert_eq!(sent.len(), 3);
    let second: Value = serde_json::from_str(&sent[1].1.body).expect("JSON");
    assert!(second.get("response_format").is_none(), "{second}");
    assert_eq!(second["language"], "en");
    let third: Value = serde_json::from_str(&sent[2].1.body).expect("JSON");
    assert!(third.get("response_format").is_none(), "{third}");
    assert!(third.get("language").is_none(), "{third}");
}

/// A refusal that names neither field is some other refusal, and asking
/// again would answer it the same way.
#[tokio::test]
async fn another_bad_request_is_not_asked_again() {
    let post = FakePost::answering(Ok(refused(
        400,
        &json!({ "error": { "message": "the text was refused" } }),
    )));

    let error = speak(&endpoint_of(), "a red kite", None, Some("en"), &post)
        .await
        .expect_err("refused");

    assert!(error.0.contains("the text was refused"), "{}", error.0);
    assert_eq!(post.sent().len(), 1);
}

/// A field that was not sent is not dropped: only what was sent can be
/// asked again without.
#[tokio::test]
async fn a_call_without_language_is_not_asked_again() {
    let post = FakePost::answering(Ok(refused(
        400,
        &json!({ "error": { "message": "unknown parameter: 'language'." } }),
    )));

    let error = speak(&endpoint_of(), "a red kite", None, None, &post)
        .await
        .expect_err("refused");

    assert!(error.0.contains("language"), "{}", error.0);
    assert_eq!(post.sent().len(), 1);
}

/// A refusal carries the server's own message.
#[tokio::test]
async fn a_refusal_carries_the_servers_own_message() {
    let post = FakePost::answering(Ok(refused(
        402,
        &json!({ "error": { "message": "no credit left" } }),
    )));

    let error = speak(&endpoint_of(), "a red kite", None, None, &post)
        .await
        .expect_err("refused");

    assert!(error.0.contains("no credit left"), "{}", error.0);
    assert!(error.0.contains("tts"), "{}", error.0);
}

/// The server's message is free text and is posted into a thread, so it is
/// made inert like a caption.
#[tokio::test]
async fn a_refusal_cannot_carry_a_mention_into_the_thread() {
    let post = FakePost::answering(Ok(refused(
        400,
        &json!({ "error": { "message": "ask <@123> about `this`" } }),
    )));

    let error = speak(&endpoint_of(), "a red kite", None, None, &post)
        .await
        .expect_err("refused");

    assert!(!error.0.contains('<'), "{}", error.0);
    assert!(!error.0.contains('`'), "{}", error.0);
}

#[tokio::test]
async fn an_unreachable_endpoint_says_so() {
    let post = FakePost::answering(Err("connection refused".to_owned()));

    let error = speak(&endpoint_of(), "a red kite", None, None, &post)
        .await
        .expect_err("unreachable");

    assert!(error.0.contains("could not be reached"), "{}", error.0);
}

/// What is not MP3 or WAV is refused rather than posted as a file the
/// channel cannot play.
#[tokio::test]
async fn an_answer_that_is_not_audio_is_refused() {
    let post = FakePost::answering(Ok(answered(b"just text")));

    let error = speak(&endpoint_of(), "a red kite", None, None, &post)
        .await
        .expect_err("not audio");

    assert!(error.0.contains("not MP3 or WAV"), "{}", error.0);
}

/// A WAV answer is kept, and named by what its own bytes declare.
#[tokio::test]
async fn a_wav_answer_is_kept_as_wav() {
    let post = FakePost::answering(Ok(answered(b"RIFF\0\0\0\0WAVEfmt ")));

    let spoken = speak(&endpoint_of(), "a red kite", None, None, &post)
        .await
        .expect("a clip");

    assert_eq!(spoken.extension, "wav");
}

/// The endpoint is the provider that serves the named model, which is not
/// always the one a session runs on.
#[test]
fn the_endpoint_is_the_provider_that_serves_the_named_model() {
    let configured = agent(&json!({
        "provider": "anthropic",
        "speechGen": "gateway/tts",
        "providers": {
            "anthropic": { "credential": "secret" },
            "gateway": {
                "credential": "tts-key",
                "baseUrl": "https://speech.example/v1",
            },
        },
    }));

    let endpoint =
        endpoint(&configured, &[], "gateway/tts", &BTreeMap::new()).expect("an endpoint");

    assert_eq!(endpoint.base_url, "https://speech.example/v1");
    assert_eq!(endpoint.model, "tts");
    assert_eq!(endpoint.credential, "tts-key");
}

/// A short name stands for the same model here as it does at a start.
#[test]
fn a_short_name_is_expanded_before_it_is_looked_up() {
    let configured = agent(&json!({
        "provider": "anthropic",
        "providers": {
            "anthropic": { "credential": "secret" },
            "gateway": {
                "credential": "k",
                "baseUrl": "https://speech.example/v1",
            },
        },
        "aliases": { "say": "gateway/tts" },
    }));

    let endpoint = endpoint(&configured, &[], "say", &configured.aliases).expect("an endpoint");

    assert_eq!(endpoint.model, "tts");
    assert_eq!(endpoint.base_url, "https://speech.example/v1");
}

/// A bare name is served by the configured provider.
#[test]
fn a_bare_name_is_served_by_the_configured_provider() {
    let configured = agent(&json!({
        "provider": "gateway",
        "providers": {
            "gateway": {
                "credential": "k",
                "baseUrl": "https://speech.example/v1",
            },
        },
    }));

    let endpoint = endpoint(&configured, &[], "tts-low", &BTreeMap::new()).expect("an endpoint");

    assert_eq!(endpoint.base_url, "https://speech.example/v1");
    assert_eq!(endpoint.model, "tts-low");
}

/// A bare model id is served by whichever provider the catalog lists it on,
/// not by the provider a session runs on.
#[test]
fn a_bare_name_is_served_by_the_provider_that_lists_it() {
    let configured = agent(&json!({
        "provider": "chat",
        "providers": {
            "chat": { "credential": "chat-key", "baseUrl": "https://chat.example/v1" },
            "gateway": {
                "credential": "tts-key",
                "baseUrl": "https://speech.example/v1",
                "speech": { "model": "tts" },
            },
        },
    }));
    let available = vec![AvailableModel {
        provider: "gateway".to_owned(),
        id: "tts".to_owned(),
        default_level: None,
    }];

    let endpoint = endpoint(&configured, &available, "tts", &BTreeMap::new()).expect("an endpoint");

    assert_eq!(endpoint.base_url, "https://speech.example/v1");
    assert_eq!(endpoint.model, "tts");
    assert_eq!(endpoint.credential, "tts-key");
}

/// A name the catalog serves from more than one provider is refused rather
/// than sent to whichever one was looked at first.
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
            id: "tts".to_owned(),
            default_level: None,
        },
        AvailableModel {
            provider: "two".to_owned(),
            id: "tts".to_owned(),
            default_level: None,
        },
    ];

    let error = endpoint(&configured, &available, "tts", &BTreeMap::new()).expect_err("two");

    assert!(error.0.contains("more than one provider"), "{}", error.0);
    assert!(error.0.contains("agent.speechGen"), "{}", error.0);
    assert!(error.0.contains("one/tts"), "{}", error.0);
    assert!(error.0.contains("two/tts"), "{}", error.0);
}

/// A provider with no endpoint has nowhere to send the request, and saying
/// that is better than attempting it against an empty URL.
#[test]
fn a_provider_without_an_endpoint_is_refused() {
    let configured = agent(&json!({
        "provider": "gateway",
        "providers": { "gateway": { "credential": "k" } },
    }));

    let error = endpoint(&configured, &[], "tts", &BTreeMap::new()).expect_err("no endpoint");

    assert!(error.0.contains("no endpoint is configured"), "{}", error.0);
    assert!(error.0.contains("baseUrl"), "{}", error.0);
}

/// A clip over the upload limit is refused, and the refusal names the size
/// and the limit so whoever reads it knows which to change.
#[test]
fn a_clip_over_the_upload_limit_is_refused() {
    let spoken = Spoken {
        bytes: MP3.to_vec(),
        extension: "mp3",
    };

    let error = spoken.into_clip(4).expect_err("over the limit");

    assert!(error.0.contains("over the 4 byte"), "{}", error.0);
}

/// A clip is named for what its bytes declare, and captioned as a clip.
#[test]
fn a_clip_is_named_for_its_type_and_captioned_as_a_clip() {
    let clip = Spoken {
        bytes: MP3.to_vec(),
        extension: "mp3",
    }
    .into_clip(u64::MAX)
    .expect("a clip");

    assert_eq!(clip.name, "clip.mp3");
    assert!(
        clip.caption.starts_with("generated a clip, "),
        "{}",
        clip.caption
    );
}

/// The error is what a thread is told, so it has to read as a sentence.
#[test]
fn an_error_is_the_message_a_thread_reads() {
    let error = SpeechError("it could not be reached".to_owned());
    assert_eq!(error.to_string(), "it could not be reached");
}
