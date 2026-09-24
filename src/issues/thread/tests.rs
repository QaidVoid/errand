//! Tests for naming an issue as a thread.

use super::{account_id, is_github_thread, issue_of, thread_id};

#[test]
fn an_issue_is_named_as_a_thread_and_read_back() {
    let named = thread_id("talaria0101/edu-playground", 19);
    assert_eq!(named, "github:talaria0101/edu-playground#19");
    assert!(is_github_thread(&named));
    assert_eq!(issue_of(&named), Some(("talaria0101/edu-playground", 19)));

    assert!(!is_github_thread("1552528761872851095"));
    assert_eq!(issue_of("github:no-slash#1"), None);
    assert_eq!(issue_of("github:a/b/c#1"), None);
    assert_eq!(issue_of("github:a/b#one"), None);
    assert_eq!(account_id("QaidVoid"), "github:qaidvoid");
}
