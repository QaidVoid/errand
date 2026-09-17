//! Tests for delegation parsing and resolution, ported from
//! `delegation_test.ts`.

use std::sync::Arc;

use serde_json::json;

use super::{Outcome, Sources, is_refused, parse_delegation, resolve_source};
use crate::agent::delegation::Delegation;
use tempfile::TempDir;

const ROOT: &str = "/projects/demo";

struct TestSources {
    root: String,
    read_fails: bool,
}

impl Default for TestSources {
    fn default() -> Self {
        Self {
            root: ROOT.to_owned(),
            read_fails: false,
        }
    }
}

impl Sources for TestSources {
    fn project_root(&self) -> &str {
        &self.root
    }

    async fn read_file(&self, path: &str) -> std::io::Result<String> {
        if self.read_fails {
            return Err(std::io::Error::other("no such file"));
        }
        Ok(format!("contents of {path}"))
    }

    fn output_of(&self, call_id: &str) -> Option<String> {
        (call_id == "c1").then(|| "test result: 3 failed".to_owned())
    }

    fn attachment(&self, name: &str) -> Option<String> {
        (name == "shot.png").then(|| "an image".to_owned())
    }
}

fn sources() -> TestSources {
    TestSources::default()
}

fn accepted(raw: serde_json::Value) -> Delegation {
    match parse_delegation(&raw) {
        Outcome::Ready(delegation) => delegation,
        Outcome::Refused(refused) => {
            panic!("expected acceptance, got: {}", refused.refused)
        }
    }
}

#[test]
fn a_question_about_a_file_is_accepted() {
    assert_eq!(
        accepted(json!({ "question": "which tests fail?", "path": "out.log" })),
        Delegation {
            question: "which tests fail?".to_owned(),
            source: super::Source::File {
                path: "out.log".to_owned()
            },
        }
    );
}

#[test]
fn a_question_about_a_calls_output_or_an_attachment_is_accepted() {
    assert_eq!(
        accepted(json!({ "question": "what broke?", "callId": "c1" })).source,
        super::Source::Output {
            call_id: "c1".to_owned()
        }
    );
    assert_eq!(
        accepted(json!({ "question": "what is shown?", "attachment": "shot.png" })).source,
        super::Source::Attachment {
            name: "shot.png".to_owned()
        }
    );
}

/// A delegation with no artefact is a conversation with a second model, which
/// is the thing this deliberately is not.
#[test]
fn a_delegation_that_names_nothing_is_refused_with_a_reason() {
    let refused = parse_delegation(&json!({
        "question": "what should I do about the flaky test?",
    }));
    assert!(is_refused(&refused));
    match refused {
        Outcome::Refused(refused) => assert!(refused.refused.contains("must name what to look at")),
        Outcome::Ready(_) => panic!("expected a refusal"),
    }
}

#[test]
fn a_delegation_with_no_question_is_refused() {
    assert!(is_refused(&parse_delegation(&json!({ "path": "out.log" }))));
    assert!(is_refused(&parse_delegation(
        &json!({ "question": "   ", "path": "out.log" })
    )));
}

#[test]
fn a_delegation_names_one_thing_not_several() {
    assert!(is_refused(&parse_delegation(&json!({
        "question": "q", "path": "a.log", "callId": "c1",
    }))));
}

#[test]
fn anything_that_is_not_a_request_at_all_is_refused() {
    assert!(is_refused(&parse_delegation(&serde_json::Value::Null)));
    assert!(is_refused(&parse_delegation(&json!(
        "please describe the log"
    ))));
}

#[tokio::test]
async fn a_file_is_read_and_named_for_attribution() {
    let resolved = resolve_source(
        &accepted(json!({ "question": "q", "path": "logs/out.txt" })),
        &sources(),
    )
    .await;

    let Outcome::Ready(resolved) = resolved else {
        panic!("expected a resolution");
    };
    assert_eq!(resolved.describes, "logs/out.txt");
    assert_eq!(resolved.content, "contents of /projects/demo/logs/out.txt");
}

/// A delegation must not read what the session itself could not.
#[tokio::test]
async fn a_file_outside_the_project_is_refused_however_it_is_spelled() {
    for path in ["../secrets.env", "/etc/passwd", "a/../../out"] {
        let resolved = resolve_source(
            &accepted(json!({ "question": "q", "path": path })),
            &sources(),
        )
        .await;
        assert!(is_refused(&resolved), "{path}");
        match resolved {
            Outcome::Refused(refused) => assert!(
                refused.refused.contains("outside this session's project"),
                "{path}"
            ),
            Outcome::Ready(_) => panic!("expected a refusal for {path}"),
        }
    }
}

#[tokio::test]
async fn a_file_that_cannot_be_read_is_refused_rather_than_throwing() {
    let test_sources = TestSources {
        read_fails: true,
        ..TestSources::default()
    };
    let resolved = resolve_source(
        &accepted(json!({ "question": "q", "path": "missing.txt" })),
        &test_sources,
    )
    .await;

    match resolved {
        Outcome::Refused(refused) => {
            assert!(refused.refused.contains("could not be read"))
        }
        Outcome::Ready(_) => panic!("expected a refusal"),
    }
}

#[tokio::test]
async fn a_calls_output_is_found_by_id_and_an_unknown_id_is_refused() {
    let found = resolve_source(
        &accepted(json!({ "question": "q", "callId": "c1" })),
        &sources(),
    )
    .await;
    match found {
        Outcome::Ready(resolved) => assert_eq!(resolved.content, "test result: 3 failed"),
        Outcome::Refused(_) => panic!("expected the output"),
    }

    let missing = resolve_source(
        &accepted(json!({ "question": "q", "callId": "c9" })),
        &sources(),
    )
    .await;
    assert!(is_refused(&missing));
}

#[tokio::test]
async fn an_attachment_is_found_by_name_and_an_unknown_one_is_refused() {
    let found = resolve_source(
        &accepted(json!({ "question": "q", "attachment": "shot.png" })),
        &sources(),
    )
    .await;
    match found {
        Outcome::Ready(resolved) => assert_eq!(resolved.describes, "shot.png"),
        Outcome::Refused(_) => panic!("expected the attachment"),
    }

    let missing = resolve_source(
        &accepted(json!({ "question": "q", "attachment": "other.png" })),
        &sources(),
    )
    .await;
    assert!(is_refused(&missing));
}

/// The temporary directory keeps the fixture shape without being read.
#[allow(dead_code)]
fn _root_fixture(temporary: Arc<TempDir>) -> Arc<TempDir> {
    temporary
}
