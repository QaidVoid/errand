//! Tests for the configuration in force and what a change to the file does to
//! it.

use super::{LiveConfig, Reloaded};
use crate::config::schema::{Config, ConfigError};
use crate::config::validate::validate_config;
use serde_json::json;

/// A configuration that is valid, with one field set so a change to it is
/// visible.
fn config_with_model(model: &str) -> Config {
    validate_config(&json!({
        "chat": { "token": "a.token.value", "channelId": "chan", "allowedUserIds": ["u1"] },
        "agent": {
            "provider": "zai",
            "model": model,
            "providers": { "zai": { "credential": "a-credential" } },
        },
        "projectRoot": "/tmp/p",
        "stateDir": "/tmp/s",
    }))
    .expect("the test configuration is accepted")
}

fn rejected(problems: &[&str]) -> Result<Config, ConfigError> {
    Err(ConfigError {
        problems: problems
            .iter()
            .map(|problem| (*problem).to_owned())
            .collect(),
    })
}

/// A holder that shares one configuration sees a change made by another, which
/// is the whole reason it is shared rather than copied per holder.
#[test]
fn one_holder_replacing_the_configuration_is_seen_by_another() {
    let held = LiveConfig::new(config_with_model("first"));
    let other = held.clone();

    assert!(other.replace(config_with_model("second")));
    assert_eq!(held.get().agent.model.as_deref(), Some("second"));
    assert_eq!(other.get().agent.model.as_deref(), Some("second"));
}

/// A save with no edit in it is not a change, so a reload that says so is not
/// reported as one.
#[test]
fn a_configuration_that_says_the_same_thing_is_not_a_change() {
    let held = LiveConfig::new(config_with_model("only"));

    assert!(!held.replace(config_with_model("only")));
    assert_eq!(
        held.reload(Ok(config_with_model("only"))),
        Reloaded::Unchanged
    );
    assert_eq!(held.get().agent.model.as_deref(), Some("only"));
}

/// The daemon is serving under a configuration that was valid when it was
/// read. An edit that does not parse has not made that any less true, so
/// nothing is applied and every reason comes back for the log to say.
#[test]
fn a_reload_that_cannot_be_read_leaves_the_running_configuration_alone() {
    let held = LiveConfig::new(config_with_model("working"));
    let other = held.clone();

    let outcome = held.reload(rejected(&[
        "the configuration file is not valid JSON or JSONC",
        "agent.provider is required",
    ]));

    assert_eq!(
        outcome,
        Reloaded::Rejected(vec![
            "the configuration file is not valid JSON or JSONC".to_owned(),
            "agent.provider is required".to_owned(),
        ])
    );
    // Unchanged, and still so through every other holder.
    assert_eq!(other.get().agent.model.as_deref(), Some("working"));
    assert_eq!(other.get().chat.channel_id, "chan");
}

/// A rejection is not a change, and saying otherwise would report a reload
/// that did not happen.
#[test]
fn a_rejection_is_not_reported_as_a_change() {
    let held = LiveConfig::new(config_with_model("working"));

    assert_eq!(
        held.reload(rejected(&["a syntax error"])),
        Reloaded::Rejected(vec!["a syntax error".to_owned()])
    );
    assert_eq!(
        held.reload(Ok(config_with_model("other"))),
        Reloaded::Changed
    );
    assert_eq!(
        held.reload(Ok(config_with_model("other"))),
        Reloaded::Unchanged
    );
}

/// Every reason is carried, not just the first. Reporting one at a time makes
/// fixing the file a guessing game, and the whole point of carrying on is
/// that the operator is the one doing the fixing.
#[test]
fn every_reason_a_reload_was_refused_is_carried() {
    let held = LiveConfig::new(config_with_model("working"));

    let Reloaded::Rejected(problems) = held.reload(rejected(&["first", "second", "third"])) else {
        panic!("the reload was expected to be refused");
    };
    assert_eq!(problems, ["first", "second", "third"]);
}

/// The configuration holds credentials, so a debug rendering of it, which
/// lands in a log or a panic message, must not carry them.
#[test]
fn a_debug_rendering_never_carries_a_credential() {
    let held = LiveConfig::new(config_with_model("only"));

    let shown = format!("{held:?}");
    assert!(
        !shown.contains("a-credential"),
        "the credential was shown: {shown}"
    );
    assert!(
        !shown.contains("a.token.value"),
        "the token was shown: {shown}"
    );
    assert!(shown.contains("LiveConfig"), "{shown}");
}

/// A holder that read the configuration keeps working from its own copy, so a
/// reload reaching a running session is a decision for the session rather than
/// a surprise mid-turn.
#[test]
fn a_holder_that_already_read_it_is_unaffected_by_a_later_change() {
    let held = LiveConfig::new(config_with_model("first"));
    let read = held.get();

    held.replace(config_with_model("second"));

    assert_eq!(read.agent.model.as_deref(), Some("first"));
    assert_eq!(held.get().agent.model.as_deref(), Some("second"));
}

/// Who may drive the bot is part of the whole, and a change to it is a change
/// like any other rather than something pinned by having been read early.
#[test]
fn a_change_to_who_may_use_the_bot_takes_effect_on_reload() {
    let held = LiveConfig::new(config_with_model("only"));

    let mut wider = held.get();
    wider.chat.allowed_user_ids.push("u2".to_owned());
    assert_eq!(held.reload(Ok(wider)), Reloaded::Changed);

    assert_eq!(held.get().chat.allowed_user_ids, ["u1", "u2"]);
}
