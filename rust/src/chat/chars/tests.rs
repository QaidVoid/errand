//! Tests for the character table, ported from `chars_test.ts`.

use super::{PrefixKey, ReactionKey, all_chars, glyph, prefixed, reaction};
use crate::test_util::repo_root;

#[test]
fn every_entry_renders_to_the_character_it_names() {
    assert_eq!(reaction(ReactionKey::Accepted), "\u{23f3}");
    assert_eq!(reaction(ReactionKey::Succeeded), "\u{2705}");
    assert_eq!(reaction(ReactionKey::Failed), "\u{274c}");
    assert_eq!(reaction(ReactionKey::Interrupted), "\u{23f9}\u{fe0f}");
    assert_eq!(glyph(&PrefixKey::Tool.entry()), "\u{1F527}");
}

#[test]
fn a_status_line_leads_with_the_glyph_for_its_state() {
    assert_eq!(
        prefixed(PrefixKey::Warning, "the provider is backing off"),
        "\u{26a0}\u{fe0f} the provider is backing off"
    );
}

/// Each names one state, so two states cannot share a character.
#[test]
fn no_character_is_used_for_more_than_one_state() {
    let rendered: Vec<String> = all_chars().iter().map(glyph).collect();
    let distinct: std::collections::HashSet<&String> = rendered.iter().collect();
    assert_eq!(distinct.len(), rendered.len());
}

#[test]
fn every_entry_says_what_it_means_and_what_it_is_called() {
    for entry in all_chars() {
        assert!(!entry.name.is_empty());
        assert!(!entry.meaning.is_empty());
        assert!(!entry.codepoints.is_empty());
    }
}

/// The table is the whole set, so nothing may reach chat from anywhere else.
#[test]
fn the_table_covers_every_reaction_and_prefix_there_is() {
    assert_eq!(all_chars().len(), 4 + 6);
}

/// Declared as codepoints, so the file holding them is itself ASCII.
#[test]
fn the_table_is_written_without_a_single_literal_glyph() {
    let source = std::fs::read_to_string(repo_root().join("rust/src/chat/chars.rs"))
        .expect("the table source is readable");
    assert!(source.chars().all(|character| character.is_ascii()));
}
