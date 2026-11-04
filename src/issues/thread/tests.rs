//! Tests for naming an issue as a thread.

use super::{ForgeKind, account_id, is_forge_thread, issue_of, logins_plainly, thread_id};

#[test]
fn an_issue_is_named_as_a_thread_and_read_back() {
    let named = thread_id("talaria0101/edu-playground", 19, ForgeKind::Github);
    assert_eq!(named, "github:talaria0101/edu-playground#19");
    assert!(is_forge_thread(&named));
    assert_eq!(issue_of(&named), Some(("talaria0101/edu-playground", 19)));

    assert!(!is_forge_thread("1552528761872851095"));
    assert_eq!(issue_of("github:no-slash#1"), None);
    assert_eq!(issue_of("github:a/b/c#1"), None);
    assert_eq!(issue_of("github:a/b#one"), None);
    assert_eq!(account_id("QaidVoid", ForgeKind::Github), "github:qaidvoid");
}

#[test]
fn a_forgejo_thread_is_named_with_its_own_kind() {
    let named = thread_id("talaria0101/edu-playground", 19, ForgeKind::Forgejo);
    assert_eq!(named, "forgejo:talaria0101/edu-playground#19");
    assert!(is_forge_thread(&named));
    assert_eq!(issue_of(&named), Some(("talaria0101/edu-playground", 19)));
    assert_eq!(
        account_id("QaidVoid", ForgeKind::Forgejo),
        "forgejo:qaidvoid"
    );
}

#[test]
fn recognized_prefixes_match_the_forge_names_used_to_create_threads() {
    let expected = [ForgeKind::Github, ForgeKind::Forgejo].map(crate::forge::thread_prefix);
    assert_eq!(super::prefixes(), expected);
    for kind in [ForgeKind::Github, ForgeKind::Forgejo] {
        let named = thread_id("owner/repo", 42, kind);
        assert!(is_forge_thread(&named));
        assert_eq!(issue_of(&named), Some(("owner/repo", 42)));
    }
}

#[test]
fn malformed_forge_thread_shapes_do_not_name_issues() {
    for prefix in super::prefixes() {
        for suffix in [
            "",
            "owner/repo",
            "owner/repo#",
            "/repo#1",
            "owner/#1",
            "owner/repo/extra#1",
            "owner/repo#not-a-number",
            "owner/repo#18446744073709551616",
        ] {
            assert_eq!(
                issue_of(&format!("{prefix}{suffix}")),
                None,
                "{prefix}{suffix}"
            );
        }
    }
    assert_eq!(issue_of("chat:owner/repo#1"), None);
}

#[test]
fn both_forge_account_mentions_are_rendered_plainly_in_order() {
    assert_eq!(
        logins_plainly("<@forgejo:Alice> and <@github:Bob> and <@forgejo:carol>"),
        "Alice and Bob and carol"
    );
}

#[test]
fn a_github_account_is_named_plainly_where_it_cannot_be_mentioned() {
    assert_eq!(
        logins_plainly("<@github:qaidvoid> done; <@12345> too, <@github:open"),
        "qaidvoid done; <@12345> too, <@github:open"
    );
}
