//! Tests for reading what was said to the bot on a forge.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{Heard, Poller, as_said, decide, mentions};
use crate::chat::inbound::InboundDecision;
use crate::config::schema::ForgeKind;
use crate::log::{LogFields, Logger};
use crate::session::pr::{Api, ApiReply};

const BOT: &str = "talaria0101";
const REPO: &str = "QaidVoid/edu";
const GITHUB: ForgeKind = ForgeKind::Github;

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
        kind: GITHUB,
        token: "ghp".to_owned(),
        bot: BOT.to_owned(),
        allowed: vec!["QaidVoid".to_owned()],
        repositories: Vec::new(),
        on_mention: true,
        on_assign: true,
        since: "2026-09-24T10:00:00Z".parse().unwrap(),
        floors: BTreeMap::new(),
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

    let mut poller = poller(api);
    let heard = poller.poll().await.expect("read");

    assert_eq!(heard.len(), 1);
    assert!(heard[0].mentions);
    assert_eq!(heard[0].thread_id, "github:QaidVoid/edu#7");
    assert_eq!(heard[0].message.content, "fix the parser");
    assert_eq!(heard[0].message.author_id, "github:qaidvoid");
    assert!(
        heard[0].opening.starts_with(
            "fix the parser\n\n(Asked on GitHub, on issue QaidVoid/edu#7, \
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

    let mut poller = poller(api);
    let heard = poller.poll().await.expect("read");

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

    let mut poller = poller(api);
    let heard = poller.poll().await.expect("read");

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

        let mut poller = poller(api);
        let heard = poller.poll().await.expect("read");

        assert_eq!(heard.len(), expected, "assigned by {actor}");
        assert!(poller.floors.is_empty(), "GitHub uses last_read_at instead");
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

    {
        let mut poller = poller(api);
        assert!(poller.poll().await.expect("read").is_empty());
    }
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
    assert!(mentions(&format!("@{BOT} please"), BOT, GITHUB));
    assert!(mentions(
        &format!("thanks, @{}!", BOT.to_uppercase()),
        BOT,
        GITHUB
    ));
    assert!(!mentions(&format!("@{BOT}1 please"), BOT, GITHUB));
    assert!(!mentions(&format!("mail me at me@{BOT}.dev"), BOT, GITHUB));
    assert!(!mentions(&format!("x@{BOT} not a mention"), BOT, GITHUB));
    assert_eq!(
        as_said(
            &format!("@{BOT} fix the\n```\nparser\n```\nwith @Alice-B, me@x.dev"),
            BOT,
            GITHUB
        ),
        "fix the\n```\nparser\n```\nwith <@github:alice-b>, me@x.dev"
    );
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
        opening: "the words\n\n(Asked on GitHub...)".to_owned(),
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
    assert!(message.content.starts_with("the words\n\n(Asked on GitHub"));
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

    {
        let mut poller = poller(api);
        assert!(poller.poll().await.expect("read").is_empty());
    }
    assert_eq!(
        *calls.lock().unwrap(),
        [
            "GET /notifications?participating=true&per_page=50".to_owned(),
            "PATCH /notifications/threads/900".to_owned(),
        ]
    );
}

/// Only the repositories listened in are asked about; the rest are marked
/// read, so they do not wait in the queue.
#[tokio::test]
async fn a_repository_not_listened_in_is_not_asked_about() {
    for (listened, expected) in [
        (vec!["QaidVoid/edu".to_owned()], 1),
        (vec!["qaidvoid/*".to_owned()], 1),
        (
            vec!["somebody-else/*".to_owned(), "QaidVoid/other".to_owned()],
            0,
        ),
    ] {
        let (answering, calls) = api(answers(
            &notification("mention", None),
            &issue("2026-09-24T11:00:00Z", &format!("@{BOT} fix the parser")),
            &json!([]),
            "2026-09-24T10:00:00Z",
        ));
        let mut poller = poller(answering);
        poller.repositories = listened.clone();

        let heard = poller.poll().await.expect("read");

        assert_eq!(heard.len(), expected, "{listened:?}");
        assert!(
            calls
                .lock()
                .unwrap()
                .contains(&"PATCH /notifications/threads/900".to_owned())
        );
    }
}

/// `on` says what may start a session: with mentions off, a mention is heard
/// but starts nothing, and with assignments off, an assignment is not asked
/// about at all.
#[tokio::test]
async fn only_what_on_names_starts_a_session() {
    let (mentioned, _) = api(answers(
        &notification("mention", None),
        &issue("2026-09-24T11:00:00Z", &format!("@{BOT} fix the parser")),
        &json!([]),
        "2026-09-24T10:00:00Z",
    ));
    let mut mentions_off = poller(mentioned);
    mentions_off.on_mention = false;
    let heard = mentions_off.poll().await.expect("read");
    assert_eq!(heard.len(), 1);
    assert!(!heard[0].mentions, "heard, but it starts nothing");

    let (assigned, calls) = api(answers(
        &notification("assign", None),
        &issue("2026-09-24T09:00:00Z", "Make the parser stream."),
        &json!([]),
        "2026-09-24T10:00:00Z",
    ));
    let mut assign_off = poller(assigned);
    assign_off.on_assign = false;
    assert!(assign_off.poll().await.expect("read").is_empty());
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call.contains("/events"))
    );
}

/// A Forgejo instance asks its notifications without `participating`, names
/// a subject `issue` in the lower case, numbers its threads, and lists its
/// comments without pagination. An assignment is read off the timeline, whose
/// entry names the assignee and who made it.
#[tokio::test]
async fn a_forgejo_notification_is_heard_by_its_own_vocabulary() {
    let forgejo_notification = json!({
        "id": 900_i64,
        "unread": true,
        "updated_at": "2026-09-24T12:30:00Z",
        "subject": {
            "type": "issue",
            "title": "The parser drops the last line",
            "url": format!("http://forge.example.com/api/v1/repos/{REPO}/issues/7"),
        },
        "repository": { "full_name": REPO },
    });
    let forgejo_issue = json!({
        "title": "The parser drops the last line",
        "body": format!("@{BOT} fix the parser"),
        "html_url": format!("http://forge.example.com/{REPO}/issues/7"),
        "created_at": "2026-09-24T11:00:00Z",
        "user": { "login": "QaidVoid" },
    });
    let timeline = json!([{
        "type": "assignees",
        "assignee": { "login": BOT },
        "user": { "login": "QaidVoid" },
        "created_at": "2026-09-24T11:30:00Z",
        "id": 7_i64,
    }]);
    let (answering, calls) = api(BTreeMap::from([
        (
            "GET /notifications?limit=50&status-types=unread".to_owned(),
            json!([forgejo_notification]),
        ),
        (format!("GET /repos/{REPO}/issues/7"), forgejo_issue),
        (
            format!("GET /repos/{REPO}/issues/7/comments?since=2026-09-24T10:00:00Z"),
            json!([]),
        ),
        (
            format!(
                "GET /repos/{REPO}/issues/7/timeline?since=2026-09-24T10:00:00Z&limit=100&page=1"
            ),
            timeline,
        ),
        (
            format!(
                "GET /repos/{REPO}/issues/7/timeline?since=2026-09-24T10:00:00Z&limit=100&page=2"
            ),
            json!([]),
        ),
    ]));
    let mut forgejo = poller(answering);
    forgejo.kind = ForgeKind::Forgejo;

    let heard = forgejo.poll().await.expect("read");

    // No `reason` field exists to say why the instance notified, so the
    // mention on the issue and the assignment on the timeline are both read.
    assert_eq!(heard.len(), 2);
    assert!(
        heard
            .iter()
            .all(|heard| heard.thread_id == "forgejo:QaidVoid/edu#7")
    );
    let mention = heard
        .iter()
        .find(|heard| heard.message.id == "forgejo-issue:QaidVoid/edu#7")
        .expect("the mention");
    assert_eq!(mention.message.author_id, "forgejo:qaidvoid");
    assert!(
        mention
            .opening
            .contains("http://forge.example.com/QaidVoid/edu/issues/7")
    );
    let assigned = heard
        .iter()
        .find(|heard| heard.message.id == "forgejo-assign:7")
        .expect("the assignment");
    assert_eq!(assigned.message.author_id, "forgejo:qaidvoid");
    assert!(
        calls
            .lock()
            .unwrap()
            .contains(&"PATCH /notifications/threads/900".to_owned()),
        "a numbered thread is marked read"
    );
}

/// The timeline is oldest first, so an assignment beyond its first page
/// must be read before the notification's floor moves forward.
#[tokio::test]
async fn a_forgejo_assignment_after_the_first_hundred_events_is_heard() {
    for page_size in [50, 100] {
        let floor = "2026-09-24T10:00:00Z";
        let mut first_page = vec![json!({ "type": "comment" }); page_size];
        first_page[0] = json!({
            "id": 1,
            "type": "assignees",
            "assignee": { "login": BOT },
            "user": { "login": "stranger" },
            "created_at": "2026-09-24T11:00:00Z",
        });
        let (answering, calls) = api(BTreeMap::from([
            (
                "GET /notifications?limit=50&status-types=unread".to_owned(),
                json!([notification("", None)]),
            ),
            (
                format!("GET /repos/{REPO}/issues/7"),
                issue("2026-09-24T09:00:00Z", "Fix the parser."),
            ),
            (
                format!("GET /repos/{REPO}/issues/7/comments?since={floor}"),
                json!([]),
            ),
            (
                format!("GET /repos/{REPO}/issues/7/timeline?since={floor}&limit=100&page=1"),
                json!(first_page),
            ),
            (
                format!("GET /repos/{REPO}/issues/7/timeline?since={floor}&limit=100&page=2"),
                json!([{
                    "id": 101,
                    "type": "assignees",
                    "assignee": { "login": BOT },
                    "user": { "login": "QaidVoid" },
                    "created_at": "2026-09-24T12:00:00Z",
                }]),
            ),
            (
                format!("GET /repos/{REPO}/issues/7/timeline?since={floor}&limit=100&page=3"),
                json!([]),
            ),
        ]));
        let mut forgejo = poller(answering);
        forgejo.kind = ForgeKind::Forgejo;

        let heard = forgejo.poll().await.expect("read every timeline page");

        assert_eq!(heard.len(), 1);
        assert_eq!(heard[0].message.id, "forgejo-assign:101");
        assert!(heard[0].mentions);
        assert_eq!(
            forgejo.floors[&format!("{REPO}#7")],
            "2026-09-24T12:30:00Z".parse::<jiff::Timestamp>().unwrap()
        );
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.contains("/timeline?"))
                .count(),
            3
        );
        assert!(calls.contains(&"PATCH /notifications/threads/900".to_owned()));
    }
}

/// A later page failing must not acknowledge an earlier assignment or
/// comment, or move the floor past the events that still need a retry.
#[tokio::test]
async fn a_failed_forgejo_timeline_page_leaves_the_notification_and_floor_unread() {
    let floor = "2026-09-24T11:00:00Z";
    let assignment = json!({
        "id": 1,
        "type": "assignees",
        "assignee": { "login": BOT },
        "user": { "login": "QaidVoid" },
        "created_at": "2026-09-24T12:00:00Z",
    });
    let (answering, calls) = api(BTreeMap::from([
        (
            "GET /notifications?limit=50&status-types=unread".to_owned(),
            json!([notification("", None)]),
        ),
        (
            format!("GET /repos/{REPO}/issues/7"),
            issue("2026-09-24T11:30:00Z", &format!("@{BOT} fix it")),
        ),
        (
            format!("GET /repos/{REPO}/issues/7/comments?since={floor}"),
            json!([comment(
                2,
                "QaidVoid",
                "And its tests",
                "2026-09-24T12:00:00Z"
            )]),
        ),
        (
            format!("GET /repos/{REPO}/issues/7/timeline?since={floor}&limit=100&page=1"),
            json!(vec![assignment; 100]),
        ),
    ]));
    let mut forgejo = poller(answering);
    forgejo.kind = ForgeKind::Forgejo;
    let remembered = floor.parse::<jiff::Timestamp>().unwrap();
    forgejo.floors.insert(format!("{REPO}#7"), remembered);

    for _ in 0..2 {
        assert!(forgejo.poll().await.expect("retry failed page").is_empty());
        assert_eq!(forgejo.floors[&format!("{REPO}#7")], remembered);
    }

    let calls = calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.ends_with("&page=2"))
            .count(),
        2
    );
    assert!(!calls.iter().any(|call| call.starts_with("PATCH")));
}

#[test]
fn forge_mentions_follow_their_own_login_grammar_without_prefix_matches() {
    let forgejo = ForgeKind::Forgejo;
    for login in ["Bot_Name", "Bot.Name", &"a".repeat(40)] {
        assert!(mentions(&format!("@{login} please"), login, forgejo));
        assert_eq!(
            as_said(&format!("@{login} please"), login, forgejo),
            "please"
        );
        assert_eq!(
            as_said(&format!("ask @{login}"), BOT, forgejo),
            format!("ask <@forgejo:{}>", login.to_ascii_lowercase())
        );
    }
    for kind in [GITHUB, forgejo] {
        assert!(!mentions("@bot_name please", "bot", kind));
        assert!(!mentions("mail me@bot_name.example", "bot_name", kind));
    }
    assert_eq!(
        as_said("@bot_name please", "bot", GITHUB),
        "@bot_name please"
    );
    assert_eq!(
        as_said("@bot_name please", "bot", forgejo),
        "<@forgejo:bot_name> please"
    );
    assert!(mentions("thanks, @bot_name.", "bot_name", forgejo));
    assert!(!mentions("@bot_name.other", "bot_name", forgejo));
    assert!(mentions("thanks, @bot.", "bot", GITHUB));
    assert!(!mentions(
        &format!("@{}", "a".repeat(40)),
        &"a".repeat(39),
        GITHUB
    ));
    assert!(!mentions(
        &format!("@{}", "a".repeat(41)),
        &"a".repeat(40),
        forgejo
    ));
}

#[tokio::test]
async fn forgejo_issue_and_comment_mentions_keep_complete_account_names() {
    let bot = "bot_name";
    let login = "a".repeat(40);
    let floor = "2026-09-24T10:00:00Z";
    let (answering, _) = api(BTreeMap::from([
        (
            "GET /notifications?limit=50&status-types=unread".to_owned(),
            json!([notification("", None)]),
        ),
        (
            format!("GET /repos/{REPO}/issues/7"),
            issue("2026-09-24T11:00:00Z", &format!("@{bot} ask @Alice_B")),
        ),
        (
            format!("GET /repos/{REPO}/issues/7/comments?since={floor}"),
            json!([comment(
                3,
                "QaidVoid",
                &format!("@{bot} ask @{login}"),
                "2026-09-24T12:00:00Z"
            )]),
        ),
    ]));
    let mut forgejo = poller(answering);
    forgejo.kind = ForgeKind::Forgejo;
    forgejo.bot = bot.to_owned();
    forgejo.on_assign = false;

    let heard = forgejo.poll().await.expect("read");

    assert_eq!(heard.len(), 2);
    assert!(heard.iter().all(|heard| heard.mentions));
    assert_eq!(heard[0].message.content, "ask <@forgejo:alice_b>");
    assert_eq!(heard[1].message.content, format!("ask <@forgejo:{login}>"));
}
