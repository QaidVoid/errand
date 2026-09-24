//! Tests for reading what was said to the bot on GitHub.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{Heard, Poller, as_said, decide, mentions, project_name};
use crate::chat::inbound::InboundDecision;
use crate::log::{LogFields, Logger};
use crate::session::pr::{Api, ApiReply};

const BOT: &str = "talaria0101";
const REPO: &str = "QaidVoid/edu";

/// Answers each `METHOD path` from a table, and records every call made.
/// Anything not in the table is a 404, so a call nobody expected shows.
fn api(answers: BTreeMap<String, Value>) -> (Api, Arc<Mutex<Vec<String>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let api: Api = Arc::new(move |path, init| {
        let called = format!("{} {path}", init.method);
        seen.lock().unwrap().push(called.clone());
        let answer = match answers.get(&called) {
            Some(body) => ApiReply {
                status: 200,
                body: body.clone(),
            },
            None if init.method == "PATCH" => ApiReply {
                status: 205,
                body: json!({}),
            },
            None => ApiReply {
                status: 404,
                body: json!({ "message": "Not Found" }),
            },
        };
        Box::pin(async move { answer })
    });
    (api, calls)
}

fn poller(api: Api) -> Poller {
    Poller {
        api,
        token: "ghp".to_owned(),
        bot: BOT.to_owned(),
        allowed: vec!["QaidVoid".to_owned()],
        since: "2026-09-24T10:00:00Z".parse().unwrap(),
        log: Logger::new(LogFields::new(), Arc::new(|_level, _line| {})),
    }
}

fn notification(reason: &str, last_read_at: Option<&str>) -> Value {
    json!({
        "id": "900",
        "reason": reason,
        "last_read_at": last_read_at,
        "updated_at": "2026-09-24T12:30:00Z",
        "subject": { "type": "Issue", "url": format!("https://api.github.com/repos/{REPO}/issues/7") },
        "repository": { "full_name": REPO },
    })
}

fn issue(created_at: &str, body: &str) -> Value {
    json!({
        "title": "The parser drops the last line",
        "body": body,
        "html_url": format!("https://github.com/{REPO}/issues/7"),
        "created_at": created_at,
        "user": { "login": "QaidVoid" },
    })
}

fn comment(id: u64, login: &str, body: &str, at: &str) -> Value {
    json!({ "id": id, "user": { "login": login }, "body": body, "created_at": at })
}

fn answers(
    notification: &Value,
    issue: &Value,
    comments: &Value,
    since: &str,
) -> BTreeMap<String, Value> {
    BTreeMap::from([
        (
            "GET /notifications?participating=true&per_page=50".to_owned(),
            json!([notification]),
        ),
        (format!("GET /repos/{REPO}/issues/7"), issue.clone()),
        (
            format!("GET /repos/{REPO}/issues/7/comments?since={since}&per_page=100"),
            comments.clone(),
        ),
    ])
}

/// A new issue naming the bot starts a session in a workspace of its own,
/// told where it was asked, and the notification is marked read.
#[tokio::test]
async fn a_new_issue_naming_the_bot_is_heard_and_marked_read() {
    let (api, calls) = api(answers(
        &notification("mention", None),
        &issue("2026-09-24T11:00:00Z", &format!("@{BOT} fix the parser")),
        &json!([]),
        "2026-09-24T10:00:00Z",
    ));

    let heard = poller(api).poll().await.expect("read");

    assert_eq!(heard.len(), 1);
    assert!(heard[0].mentions);
    assert_eq!(heard[0].thread_id, "github:QaidVoid/edu#7");
    assert_eq!(heard[0].message.content, "fix the parser");
    assert_eq!(heard[0].message.author_id, "github:qaidvoid");
    assert!(
        heard[0].opening.starts_with(
            "edu-7: fix the parser\n\n(Asked on GitHub, on issue QaidVoid/edu#7, \
             \"The parser drops the last line\": https://github.com/QaidVoid/edu/issues/7."
        ),
        "{}",
        heard[0].opening
    );
    assert!(
        calls
            .lock()
            .unwrap()
            .contains(&"PATCH /notifications/threads/900".to_owned())
    );
}

/// Only what somebody heard said after the last reading, and after the
/// daemon started, is heard; a stranger and the bot itself are not.
#[tokio::test]
async fn only_new_words_from_somebody_heard_are_heard() {
    let (api, _) = api(answers(
        &notification("comment", Some("2026-09-24T12:00:00Z")),
        &issue("2026-09-24T09:00:00Z", "an old issue"),
        &json!([
            comment(1, "QaidVoid", "read already", "2026-09-24T11:59:00Z"),
            comment(
                2,
                "stranger",
                &format!("@{BOT} run this"),
                "2026-09-24T12:01:00Z"
            ),
            comment(3, BOT, "my own answer", "2026-09-24T12:02:00Z"),
            comment(4, "qaidvoid", "and also the tests", "2026-09-24T12:03:00Z"),
        ]),
        "2026-09-24T12:00:00Z",
    ));

    let heard = poller(api).poll().await.expect("read");

    assert_eq!(heard.len(), 1, "{heard:?}");
    assert_eq!(heard[0].message.id, "github-comment:4");
    assert!(!heard[0].mentions);
    assert_eq!(heard[0].message.content, "and also the tests");
}

/// Asked in a comment, the session is told what the issue itself says too.
#[tokio::test]
async fn a_comment_naming_the_bot_carries_the_issue_with_it() {
    let (api, _) = api(answers(
        &notification("mention", None),
        &issue("2026-09-24T09:00:00Z", "Steps: run it on an empty file."),
        &json!([comment(
            5,
            "QaidVoid",
            &format!("@{BOT} can you look?"),
            "2026-09-24T11:00:00Z"
        )]),
        "2026-09-24T10:00:00Z",
    ));

    let heard = poller(api).poll().await.expect("read");

    assert_eq!(heard.len(), 1);
    assert!(heard[0].mentions);
    assert!(
        heard[0]
            .opening
            .ends_with("The issue says:\n\nSteps: run it on an empty file."),
        "{}",
        heard[0].opening
    );
}

/// An assignment is only as good as who made it.
#[tokio::test]
async fn an_assignment_counts_only_from_somebody_heard() {
    for (actor, expected) in [("QaidVoid", 1), ("stranger", 0)] {
        let mut table = answers(
            &notification("assign", None),
            &issue("2026-09-24T09:00:00Z", "Make the parser stream."),
            &json!([]),
            "2026-09-24T10:00:00Z",
        );
        table.insert(
            format!("GET /repos/{REPO}/issues/7/events?per_page=100"),
            json!([{
                "id": 77,
                "event": "assigned",
                "assignee": { "login": BOT },
                "actor": { "login": actor },
                "created_at": "2026-09-24T11:00:00Z",
            }]),
        );
        let (api, _) = api(table);

        let heard = poller(api).poll().await.expect("read");

        assert_eq!(heard.len(), expected, "assigned by {actor}");
        if let Some(heard) = heard.first() {
            assert!(heard.mentions);
            assert_eq!(heard.message.content, "Make the parser stream.");
        }
    }
}

/// A notification that could not be read stays unread, so it is tried again
/// rather than lost.
#[tokio::test]
async fn an_unreadable_notification_is_left_unread() {
    let mut table = answers(
        &notification("mention", None),
        &issue("2026-09-24T11:00:00Z", "x"),
        &json!([]),
        "2026-09-24T10:00:00Z",
    );
    table.remove(&format!("GET /repos/{REPO}/issues/7"));
    let (api, calls) = api(table);

    assert!(poller(api).poll().await.expect("read").is_empty());
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call.starts_with("PATCH"))
    );
}

#[test]
fn a_mention_is_the_login_standing_on_its_own() {
    assert!(mentions(&format!("@{BOT} please"), BOT));
    assert!(mentions(&format!("thanks, @{}!", BOT.to_uppercase()), BOT));
    assert!(!mentions(&format!("@{BOT}1 please"), BOT));
    assert!(!mentions(&format!("mail me at me@{BOT}.dev"), BOT));
    assert!(!mentions(&format!("x@{BOT} not a mention"), BOT));
    assert_eq!(
        as_said(
            &format!("@{BOT} fix the\n```\nparser\n```\nwith @Alice-B, me@x.dev"),
            BOT
        ),
        "fix the\n```\nparser\n```\nwith <@github:alice-b>, me@x.dev"
    );
}

#[test]
fn an_issue_gets_a_workspace_named_after_its_repository_and_number() {
    assert_eq!(
        project_name("QaidVoid/Edu.Playground", 19),
        "edu.playground-19"
    );
    assert_eq!(project_name("o/.hidden", 3), "r.hidden-3");
    assert_eq!(project_name("o/a+b", 1), "a-b-1");
    assert!(project_name(&format!("o/{}", "x".repeat(80)), 12345).len() <= 64);
}

fn heard(mentions: bool) -> Heard {
    Heard {
        thread_id: "github:o/r#1".to_owned(),
        mentions,
        message: crate::chat::inbound::RawMessage {
            id: "github-comment:1".to_owned(),
            author_id: "github:qaidvoid".to_owned(),
            author_name: Some("QaidVoid".to_owned()),
            author_is_bot: false,
            channel_id: "github:o/r#1".to_owned(),
            parent_channel_id: None,
            content: "the words".to_owned(),
            attachments: Vec::new(),
        },
        opening: "r-1: the words\n\n(Asked on GitHub...)".to_owned(),
    }
}

/// An issue answered by a live thread hears every comment, as the thread
/// hears every reply; one without starts a session only when the bot was
/// named.
#[test]
fn an_issue_with_a_session_continues_it_and_one_without_needs_the_bot_named() {
    let (message, decision) =
        decide(heard(false), Some("1552528761872851095".to_owned())).expect("continued");
    assert_eq!(message.content, "the words");
    assert_eq!(
        decision,
        InboundDecision::Thread {
            thread_id: "1552528761872851095".to_owned()
        }
    );

    let (message, decision) = decide(heard(true), None).expect("started");
    assert!(message.content.starts_with("r-1: the words"));
    assert_eq!(decision, InboundDecision::Start);

    assert_eq!(decide(heard(false), None), None);
}

/// A notification with nothing since the daemon started holds nothing new,
/// so it is marked read without asking what is in it.
#[tokio::test]
async fn a_notification_quiet_since_the_start_is_not_asked_about() {
    let mut quiet = notification("mention", None);
    quiet["updated_at"] = json!("2026-09-24T09:00:00Z");
    let (api, calls) = api(BTreeMap::from([(
        "GET /notifications?participating=true&per_page=50".to_owned(),
        json!([quiet]),
    )]));

    assert!(poller(api).poll().await.expect("read").is_empty());
    assert_eq!(
        *calls.lock().unwrap(),
        [
            "GET /notifications?participating=true&per_page=50".to_owned(),
            "PATCH /notifications/threads/900".to_owned(),
        ]
    );
}
