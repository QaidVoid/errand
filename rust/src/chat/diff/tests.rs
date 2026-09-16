//! Tests for the diff renderer, ported from `diff_test.ts`.

use super::{CONTEXT_LINES, MAX_DIFF_LINES, file_diff, render_diff};

#[test]
fn a_file_that_did_not_change_has_nothing_to_show() {
    assert_eq!(
        file_diff("same\ntext", "same\ntext"),
        crate::chat::diff::FileDiff {
            empty: true,
            added: 0,
            removed: 0,
            body: String::new(),
        }
    );
}

#[test]
fn a_changed_line_reads_as_one_added_and_one_removed() {
    let diff = file_diff("one\ntwo\nthree", "one\nTWO\nthree");

    assert_eq!(diff.added, 1);
    assert_eq!(diff.removed, 1);
    assert!(diff.body.contains("-two"));
    assert!(diff.body.contains("+TWO"));
    assert!(diff.body.contains(" one"));
}

/// The point of a diff: a small change in a large file reads as a small one.
#[test]
fn unchanged_regions_are_dropped_apart_from_a_little_context() {
    let before: Vec<String> = (0..200).map(|index| format!("line {index}")).collect();
    let mut after = before.clone();
    after[100] = "changed".to_owned();

    let diff = file_diff(&before.join("\n"), &after.join("\n"));

    let shown: Vec<&str> = diff.body.split('\n').filter(|line| *line != "@@").collect();
    assert_eq!(shown.len(), 2 * CONTEXT_LINES + 2);
    assert!(diff.body.contains("+changed"));
}

/// Two changes far apart must not read as one continuous region.
#[test]
fn a_gap_between_changes_is_marked_rather_than_silently_closed() {
    let before: Vec<String> = (0..60).map(|index| format!("line {index}")).collect();
    let mut after = before.clone();
    after[5] = "first".to_owned();
    after[40] = "second".to_owned();

    let diff = file_diff(&before.join("\n"), &after.join("\n"));

    assert_eq!(
        diff.body.split('\n').filter(|line| *line == "@@").count(),
        1
    );
}

#[test]
fn a_very_large_change_is_cut_and_says_how_much_it_left_out() {
    let before: Vec<String> = (0..300).map(|index| format!("old {index}")).collect();
    let after: Vec<String> = (0..300).map(|index| format!("new {index}")).collect();

    let diff = file_diff(&before.join("\n"), &after.join("\n"));

    let body: Vec<&str> = diff.body.split('\n').collect();
    assert_eq!(body.len(), MAX_DIFF_LINES + 1);
    assert!(body[body.len() - 1].contains("further line(s) not shown"));
    assert_eq!(diff.added, 300);
}

/// A minified file is one line, and would otherwise be posted whole.
#[test]
fn a_single_enormous_line_is_clipped_not_posted_entire() {
    let diff = file_diff("short", &"x".repeat(5_000));

    for line in diff.body.split('\n') {
        assert!(line.chars().count() < 300);
    }
    assert!(diff.body.contains("..."));
}

#[test]
fn an_added_file_is_all_additions() {
    let diff = file_diff("", "one\ntwo");

    assert_eq!(diff.removed, 1);
    assert_eq!(diff.added, 2);
}

#[test]
fn the_message_says_which_file_and_by_how_much() {
    let message = render_diff("src/main.ts", &file_diff("one", "two"));

    assert!(message.contains("`src/main.ts` +1 -1"));
    assert!(message.contains("```diff"));
    assert_eq!(message.matches("```").count(), 2);
}
