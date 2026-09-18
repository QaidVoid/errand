//! Tests over the example configuration, ported from `example_test.ts`.
//!
//! The file somebody copies to start from. An example that no longer
//! validates is worse than none: it is copied, it fails, and the first thing
//! this daemon ever did was refuse.

use std::fs;

use serde_json::Value;

use super::schema::defaults;
use super::validate::validate_config;
use crate::test_util::repo_root;

fn tracked_file(name: &str) -> Value {
    let text = fs::read_to_string(repo_root().join(name))
        .unwrap_or_else(|error| panic!("{name} should be readable: {error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{name} should be JSON: {error}"))
}

/// An example that no longer validates is worse than none: it is copied, it
/// fails, and the first thing this daemon ever did was refuse. Checked here
/// so that renaming a field breaks the suite rather than somebody's first run.
#[test]
fn the_example_configuration_is_one_the_daemon_accepts() {
    let example = tracked_file("config.example.json");

    let config = validate_config(&example).expect("the example configuration is accepted");

    assert!(!config.chat.channel_id.is_empty());
    assert_eq!(config.agent.provider, "zai-coding-cn");
    assert_eq!(
        config.agent.delegate.map(|delegate| delegate.model),
        Some("glm-5.3-flash".to_owned())
    );
    assert!(
        config
            .github
            .map(|github| github.user_name)
            .is_some_and(|name| !name.is_empty())
    );
    assert_eq!(config.web.map(|web| web.port), Some(defaults::WEB_PORT));
}

/// Every section the example shows should be one the daemon knows.
#[test]
fn the_example_names_no_setting_the_daemon_would_refuse() {
    let example = tracked_file("config.example.json");

    // Validation refuses an unknown key outright, so reaching here is the check.
    validate_config(&example).expect("the example configuration is accepted");
    assert!(
        !example
            .as_object()
            .expect("an object")
            .contains_key("discord")
    );
}

/// What an editor checks against, and what the daemon checks against.
///
/// Both are generated from the same interfaces, so this asserts they agree on
/// the sections rather than restating either.
#[test]
fn the_schema_offers_every_section_the_daemon_reads() {
    let schema = tracked_file("config.schema.json");
    let example = tracked_file("config.example.json");

    let offered = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("the schema names its properties");
    for key in example.as_object().expect("an object").keys() {
        assert!(offered.contains_key(key), "{key} is not in the schema");
    }
    assert_eq!(
        schema.get("additionalProperties"),
        Some(&Value::Bool(false))
    );
    assert!(offered.contains_key("$schema"));
}

/// A file that names its schema must still be one the daemon accepts.
#[test]
fn naming_the_schema_is_not_a_setting_the_daemon_refuses() {
    let example = tracked_file("config.example.json");

    assert!(matches!(example.get("$schema"), Some(Value::String(_))));
    validate_config(&example).expect("the example configuration is accepted");
}
