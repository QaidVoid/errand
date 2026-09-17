//! Slash command tests, ported from the command half of `gateway_test.ts`.

use serenity::model::application::CommandDataOption;

use super::{build_commands, translate};
use crate::session::commands::COMMANDS;

/// Builds one option from the shape the service sends, since the library's
/// struct cannot be constructed from outside it.
fn text_option(name: &str, value: &str) -> CommandDataOption {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "type": 3,
        "value": value,
    }))
    .expect("a string option")
}

fn user_option(name: &str, id: u64) -> CommandDataOption {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "type": 6,
        "value": id.to_string(),
    }))
    .expect("a user option")
}

/// A slash command is the same command, not a second implementation: it
/// becomes the text a person would have typed and runs through the same code.
#[test]
fn an_interaction_becomes_the_text_command_it_stands_for() {
    let command = translate(
        "cat",
        &[text_option("path", "src/main.ts")],
        "thread-1",
        true,
        "u1",
        "amelia",
    );

    assert_eq!(command.content, "!cat src/main.ts");
    assert_eq!(command.thread_id.as_deref(), Some("thread-1"));
    assert_eq!(command.user_id, "u1");
    assert_eq!(command.user_name, "amelia");
}

#[test]
fn a_command_naming_an_account_carries_the_account_id() {
    let command = translate(
        "allow",
        &[user_option("user", 900000000000000002)],
        "thread-1",
        true,
        "u1",
        "amelia",
    );

    assert_eq!(command.content, "!allow 900000000000000002");
}

#[test]
fn a_command_with_no_argument_is_just_the_command() {
    let command = translate("status", &[], "chan", false, "u1", "amelia");

    assert_eq!(command.content, "!status");
    assert_eq!(command.thread_id, None);
}

/// One table, so what can be typed and what can be picked cannot drift.
#[test]
fn every_command_is_offered_as_a_slash_command_too() {
    // The definitions are built by a one-to-one map over the table, so the
    // count is the compiler's to enforce; what a test can see is the names
    // the commands are registered under.
    assert_eq!(build_commands().len(), COMMANDS.len());
    for (name, _) in COMMANDS {
        assert_eq!(
            super::slash_name(name),
            name.strip_prefix('!').unwrap_or(name)
        );
    }
}

/// Whoever reads the picker should know before they try.
#[test]
fn a_commands_description_says_who_may_run_it() {
    let named = |name: &str| {
        COMMANDS
            .iter()
            .find(|(command, _)| *command == name)
            .map(|(_, meta)| super::describe_for_test(meta.access, meta.summary))
            .unwrap_or_default()
    };

    assert!(named("!stop").contains("owner only"));
    assert!(named("!ls").contains("owner and invited"));
    assert!(named("!shutdown").contains("named accounts"));
    let status = named("!status");
    assert!(!status.contains('('));
}

/// A command that takes an argument declares it, and says whether it must be
/// there.
#[test]
fn a_command_that_takes_an_argument_declares_it() {
    // The table drives both the picker and the translation, so the flags are
    // read where they are declared.
    let option_of = |name: &str| {
        super::TEXT_OPTION
            .iter()
            .find(|(command, _, _, _)| *command == name)
    };

    assert_eq!(
        option_of("!cat").map(|(_, _, _, required)| *required),
        Some(true)
    );
    assert_eq!(
        option_of("!ls").map(|(_, _, _, required)| *required),
        Some(false)
    );
    assert_eq!(option_of("!status"), None);
}
