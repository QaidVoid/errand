//! Tests for the standing instructions, ported from `rules_test.ts`.

use super::{RULES_HEADING, house_rules_text, rules_block};
use tempfile::TempDir;

#[test]
fn a_file_with_rules_in_it_becomes_a_block_under_the_heading() {
    let block = rules_block("Always run the suite before committing.");

    let block = block.expect("a block");
    assert!(block.contains(RULES_HEADING));
    assert!(block.contains("Always run the suite before committing."));
}

#[test]
fn an_empty_file_is_no_rules_rather_than_a_heading_over_nothing() {
    // An operator who empties the file is saying there are no rules. A heading
    // with nothing under it reads to the agent as an omission it should ask
    // about.
    assert_eq!(rules_block(""), None);
    assert_eq!(rules_block("   \n\n\t\n"), None);
}

#[test]
fn the_block_says_where_the_rules_came_from_so_a_conflict_is_reportable() {
    // The agent has the project's own instructions too. Without knowing which
    // is which it cannot say that the two disagree, it can only silently pick
    // one.
    let block = rules_block("Never force push.").unwrap_or_default();

    assert!(block.contains("every"));
    assert!(block.contains("disagree"));
}

#[test]
fn surrounding_blank_lines_in_the_file_do_not_survive_into_the_block() {
    let padded = rules_block("\n\n  Keep commits small.  \n\n").unwrap_or_default();

    assert!(padded.contains("Keep commits small."));
    assert!(!padded.contains("\n\n\n"));
}

#[test]
fn the_rules_are_readable_back_for_scrubbing_and_absent_when_there_are_none() {
    let root = TempDir::with_prefix("errand-rules-").expect("a temporary directory");
    let path = root.path().join("AGENTS.md");
    std::fs::write(&path, "  Prefer jj over git.\nNo em-dashes.  \n").expect("written");
    let path = path.to_string_lossy().into_owned();
    assert_eq!(
        house_rules_text(Some(&path)),
        Some("Prefer jj over git.\nNo em-dashes.".to_owned())
    );

    // Nothing configured, an unreadable path, and an emptied file are all
    // "no rules" rather than a failure.
    assert_eq!(house_rules_text(None), None);
    let absent = root.path().join("absent.md").to_string_lossy().into_owned();
    assert_eq!(house_rules_text(Some(&absent)), None);
    std::fs::write(&path, "   \n\n").expect("emptied");
    assert_eq!(house_rules_text(Some(&path)), None);
}
