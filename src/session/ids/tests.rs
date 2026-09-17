//! Tests for session identifiers, ported from `ids_test.ts`.

use super::{TOKEN_LENGTH, session_id, session_token};
use crate::session::projects::ProjectSelection;

fn selection(name: &str, was_explicit: bool) -> ProjectSelection {
    ProjectSelection {
        name: name.to_owned(),
        path: format!("/srv/{name}"),
        prompt: "go".to_owned(),
        was_explicit,
    }
}

#[test]
fn a_token_is_the_declared_length_and_only_lower_case_base_36() {
    for _ in 0..500 {
        let token = session_token(TOKEN_LENGTH);
        assert_eq!(token.len(), TOKEN_LENGTH);
        assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()),
            "{token}"
        );
    }
}

#[test]
fn tokens_do_not_repeat_and_do_not_follow_a_counter() {
    // The old generator prefixed an in-memory counter that restarted with the
    // daemon, so the first session of every run began with the same character.
    let mut drawn = std::collections::HashSet::new();
    let mut firsts = std::collections::HashSet::new();
    for _ in 0..2_000 {
        let token = session_token(TOKEN_LENGTH);
        firsts.insert(token.chars().next().expect("a character"));
        assert!(drawn.insert(token));
    }
    assert!(firsts.len() > 20);
}

#[test]
fn every_digit_of_the_alphabet_turns_up_so_none_is_quietly_unreachable() {
    // 256 is not a multiple of 36. Folding the high bytes instead of redrawing
    // them would make 0 to 3 appear about a seventh more often than the rest.
    let mut seen = std::collections::HashSet::new();
    for _ in 0..20_000 {
        for character in session_token(TOKEN_LENGTH).chars() {
            seen.insert(character);
        }
    }
    assert_eq!(seen.len(), 36);
}

#[test]
fn a_named_project_is_written_into_the_id_ahead_of_the_token() {
    assert_eq!(
        session_id(&selection("errand", true), "6a82hff"),
        "errand-6a82hff"
    );
}

#[test]
fn an_unnamed_session_is_its_token_alone_not_the_token_twice() {
    // Its project directory is already named after the token.
    assert_eq!(
        session_id(&selection("6a82hff", false), "6a82hff"),
        "6a82hff"
    );
}

#[test]
fn two_sessions_in_one_project_stay_distinct() {
    let first = session_id(&selection("errand", true), &session_token(TOKEN_LENGTH));
    let second = session_id(&selection("errand", true), &session_token(TOKEN_LENGTH));
    assert_ne!(first, second);
}
