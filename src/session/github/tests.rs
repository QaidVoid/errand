use std::collections::BTreeMap;

use super::{
    REQUEST_FILENAME, SessionLinks, attribution_footer, gh_shim_contents, git_config_contents,
    git_identity_env, review_instructions, thread_link, transcript_link,
};
use crate::config::schema::GithubConfig;
use crate::sandbox::backend::{AGENT_BIN, STATE_PATH};

fn github() -> GithubConfig {
    GithubConfig {
        token: "ghp-value".to_owned(),
        user_name: "errand-bot".to_owned(),
        user_email: "bot@example.com".to_owned(),
    }
}

#[test]
fn the_git_config_commits_as_the_bot_and_gets_its_token_from_gh() {
    let config = git_config_contents(&github());

    assert!(config.contains("name = errand-bot"));
    assert!(config.contains("email = bot@example.com"));
    assert!(config.contains("helper = !gh auth git-credential"));
}

/// A second copy of the token on disk buys nothing and can be read.
#[test]
fn the_token_is_never_written_into_the_git_config() {
    assert!(!git_config_contents(&github()).contains("ghp-value"));
}

/// The config file alone was not enough: an agent passing its own name at
/// commit time authored as itself, and the environment beats the file.
#[test]
fn the_identity_is_in_the_environment_as_well_as_in_the_file() {
    assert_eq!(
        git_identity_env(&github()),
        BTreeMap::from([
            ("GIT_AUTHOR_NAME".to_owned(), "errand-bot".to_owned()),
            ("GIT_AUTHOR_EMAIL".to_owned(), "bot@example.com".to_owned()),
            ("GIT_COMMITTER_NAME".to_owned(), "errand-bot".to_owned()),
            (
                "GIT_COMMITTER_EMAIL".to_owned(),
                "bot@example.com".to_owned()
            ),
        ])
    );
}

#[test]
fn the_gh_wrapper_refuses_to_open_a_pull_request_and_says_what_to_do() {
    let shim = gh_shim_contents();

    assert!(shim.contains("[ \"$1\" = \"pr\" ] && [ \"$2\" = \"create\" ]"));
    assert!(shim.contains(&format!("{STATE_PATH}/{REQUEST_FILENAME}")));
    assert!(shim.contains("exit 1"));
}

/// Anything else has to reach the real program, or the session loses `gh`.
#[test]
fn the_wrapper_hands_every_other_command_to_the_real_gh() {
    let shim = gh_shim_contents();

    assert!(shim.contains("exec \"$dir/gh\" \"$@\""));
    assert!(shim.contains(&format!("[ \"$dir\" != \"{AGENT_BIN}\" ]")));
    assert!(shim.contains("#!/bin/sh"));
}

/// The account that matches a chat name on GitHub belongs to somebody who
/// asked for nothing. This has notified the wrong person once already.
#[test]
fn the_footer_names_who_asked_and_mentions_nobody() {
    let footer = attribution_footer(
        "amelia",
        &SessionLinks {
            thread: Some("https://discord.com/channels/1/2".to_owned()),
            transcript: Some("https://errand.example/?session=abc".to_owned()),
        },
    );

    assert!(footer.contains("Requested by amelia via errand."));
    assert!(footer.contains("https://discord.com/channels/1/2"));
    assert!(!footer.contains('@'));
}

#[test]
fn a_footer_with_nowhere_to_point_at_is_still_an_attribution() {
    assert_eq!(
        attribution_footer("amelia", &SessionLinks::default()),
        "Requested by amelia via errand."
    );
}

#[test]
fn the_links_are_the_addresses_a_reader_can_open() {
    assert_eq!(
        thread_link("111", "222"),
        "https://discord.com/channels/111/222"
    );
    assert_eq!(
        transcript_link("https://errand.example/", "s 1"),
        "https://errand.example/?session=s%201"
    );
}

#[test]
fn the_instructions_carry_the_exact_footer_for_the_agent_to_copy() {
    let instructions = review_instructions(
        &github(),
        "amelia",
        &SessionLinks {
            thread: Some("https://t/1".to_owned()),
            transcript: None,
        },
    );

    assert!(instructions.contains(&attribution_footer(
        "amelia",
        &SessionLinks {
            thread: Some("https://t/1".to_owned()),
            transcript: None,
        }
    )));
    assert!(instructions.contains("Open nothing unless you were asked to"));
    assert!(instructions.contains("errand-bot <bot@example.com>"));
}

/// Announcing a pull request before the daemon opens it has happened.
#[test]
fn the_instructions_say_the_request_is_not_the_result() {
    let instructions = review_instructions(&github(), "amelia", &SessionLinks::default());

    assert!(instructions.contains("a request, not the result"));
    assert!(instructions.contains("do not say a pull request"));
    assert!(instructions.contains("Never run `gh pr create`"));
}
