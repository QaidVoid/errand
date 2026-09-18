//! Tests for redaction, ported from `redact_test.ts`.

use serde_json::{Value, json};

use super::{REDACTION, redact_config, redact_text, secret_values};
use crate::config::load::{Environment, load_config};
use crate::config::schema::{Config, SECRET_PATHS};
use crate::config::validate::validate_config;

const TOKEN: &str = "chat-token-2f8a41cc";
const CREDENTIAL: &str = "sk-live-9f3c7a11d4e6";
const GITHUB_TOKEN: &str = "ghp-4b1d90ff7ac2";
const PROVIDER_KEY: &str = "zai-5c2e80bb41af";
const SECOND_KEY: &str = "muse-77ad13e9f0b2";

fn config() -> Config {
    load_config(
        "/anywhere",
        |_| {
            Ok(json!({
                "chat": { "token": TOKEN, "channelId": "c", "allowedUserIds": ["u"] },
                "agent": {
                    "provider": "anthropic",
                    "providers": {
                        "anthropic": { "credentialName": "ANTHROPIC_API_KEY",
                                       "credential": CREDENTIAL },
                        "zai": { "credential": PROVIDER_KEY,
                                 "options": { "baseURL": "https://api.example/v1" } },
                        "muse": { "credential": SECOND_KEY },
                    },
                },
                "github": { "token": GITHUB_TOKEN, "userName": "errand-bot",
                            "userEmail": "bot@example.com" },
                "projectRoot": "/tmp/errand/projects",
                "stateDir": "/tmp/errand/state",
            })
            .to_string())
        },
        &Environment::new(),
        |_| false,
    )
    .expect("a valid configuration")
}

#[test]
fn the_configuration_can_be_logged_with_every_secret_field_blanked() {
    let shown = redact_config(&config()).to_string();

    assert!(!shown.contains(TOKEN));
    assert!(!shown.contains(CREDENTIAL));
    assert!(!shown.contains(GITHUB_TOKEN));
    // An operator names their own providers, so these are found by walking
    // rather than by the fixed paths.
    assert!(
        !shown.contains(PROVIDER_KEY),
        "a defined provider's key: {shown}"
    );
    assert!(
        !shown.contains(SECOND_KEY),
        "a second provider's key: {shown}"
    );
    assert_eq!(shown.matches(REDACTION).count(), SECRET_PATHS.len() + 3);
    // What is not a secret is still there to read.
    assert!(shown.contains("https://api.example/v1"));
}

/// Blanking a copy: the running daemon still needs the real values.
#[test]
fn redacting_for_the_log_does_not_disturb_the_configuration() {
    let held = config();
    let _ = redact_config(&held);

    assert_eq!(held.chat.token, TOKEN);
}

#[test]
fn what_a_running_session_must_scrub_is_every_secret_it_holds() {
    let mut held = secret_values(&config());
    held.sort();
    let mut expected = vec![
        TOKEN.to_owned(),
        CREDENTIAL.to_owned(),
        GITHUB_TOKEN.to_owned(),
        PROVIDER_KEY.to_owned(),
        SECOND_KEY.to_owned(),
    ];
    expected.sort();

    assert_eq!(held, expected);
}

/// A section that is not configured has no secret, and gains no field.
#[test]
fn a_secret_in_a_section_that_was_omitted_is_simply_not_there() {
    let mut without_github = config();
    without_github.github = None;

    let mut held = secret_values(&without_github);
    held.sort();
    let mut expected = vec![
        TOKEN.to_owned(),
        CREDENTIAL.to_owned(),
        PROVIDER_KEY.to_owned(),
        SECOND_KEY.to_owned(),
    ];
    expected.sort();
    assert_eq!(held, expected);
    assert_eq!(
        redact_config(&without_github).get("github"),
        Some(&Value::Null)
    );
}

#[test]
fn a_secret_is_scrubbed_wherever_it_appears_in_free_text() {
    let text = format!("Authorization: Bearer {CREDENTIAL}\nretrying with {CREDENTIAL}");

    let scrubbed = redact_text(&text, &[CREDENTIAL.to_owned()]);

    assert!(!scrubbed.contains(CREDENTIAL));
    assert_eq!(scrubbed.matches(REDACTION).count(), 2);
    assert!(scrubbed.starts_with("Authorization: Bearer "));
}

/// A configuration can hold a short value in a secret field. Replacing it
/// everywhere would rewrite ordinary prose without hiding anything worth
/// hiding, so it is left alone in text and blanked structurally instead.
#[test]
fn a_value_too_short_to_be_a_credential_is_not_scrubbed_from_prose() {
    assert_eq!(
        redact_text("the cat sat on the mat", &["cat".to_owned()]),
        "the cat sat on the mat"
    );

    let mut shortened = config();
    shortened.chat.token = "short".to_owned();
    let mut held = secret_values(&shortened);
    held.sort();
    let mut expected = vec![
        CREDENTIAL.to_owned(),
        GITHUB_TOKEN.to_owned(),
        PROVIDER_KEY.to_owned(),
        SECOND_KEY.to_owned(),
    ];
    expected.sort();
    assert_eq!(held, expected);
}

#[test]
fn text_holding_no_secret_is_returned_as_it_was() {
    assert_eq!(
        redact_text("nothing to see", &[CREDENTIAL.to_owned()]),
        "nothing to see"
    );
    assert_eq!(REDACTION, "[redacted]");
}

/// A second provider's key is as much a secret as the first one's.
#[test]
fn a_defined_providers_credential_is_scrubbed_too() {
    let config = validate_config(&json!({
        "chat": { "token": "chat-token-value", "channelId": "c", "allowedUserIds": ["u"] },
        "agent": {
            "provider": "zai",
            "providers": {
                "zai": { "credentialName": "K", "credential": "the-default-key" },
                "meta": { "baseUrl": "https://api.meta.example/v1",
                                     "credential": "the-meta-key" },
            },
        },
        "projectRoot": "/tmp/p",
        "stateDir": "/tmp/s",
    }))
    .expect("resolves");

    let secrets = secret_values(&config);
    assert!(secrets.contains(&"the-meta-key".to_owned()));
    assert!(secrets.contains(&"the-default-key".to_owned()));
    assert!(redact_text("key=the-meta-key here", &secrets).contains(REDACTION));
}
