//! Tests for an issue as a thread.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{IssueThreads, for_forge};
use crate::log::{LogFields, Logger};
use crate::session::event::{NoticeLevel, SessionEvent};
use crate::session::pr::{Api, ApiReply};
use crate::session::views::SessionView;

/// Every comment posted, by where it was posted and what it carried.
type Posted = Arc<Mutex<Vec<(String, Value)>>>;

/// Records every comment posted, and answers each as GitHub does.
fn api() -> (Api, Posted) {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&posted);
    let api: Api = Arc::new(move |path, init| {
        seen.lock()
            .unwrap()
            .push((path, init.body.unwrap_or(Value::Null)));
        Box::pin(async {
            ApiReply {
                status: 201,
                body: json!({}),
            }
        })
    });
    (api, posted)
}

fn threads(api: Api) -> Arc<IssueThreads> {
    Arc::new(IssueThreads {
        kind: crate::config::schema::ForgeKind::Github,
        api,
        token: "ghp".to_owned(),
        log: Logger::new(LogFields::new(), Arc::new(|_level, _line| {})),
    })
}

/// A tracker notifies everybody watching of every comment, so the issue is
/// told what a turn came to, the agent's last message, once, when it ends,
/// in GitHub's terms. The working and the numbers stay in the thread.
#[tokio::test]
async fn the_issue_is_told_only_what_the_turn_came_to() {
    let (api, posted) = api();
    let view = threads(api)
        .view_for("github:Acme/widgets#7")
        .expect("an issue");

    for event in [
        SessionEvent::BeginTurn { turn: 1 },
        SessionEvent::Post {
            text: "Short answer for the thread, then the detail.".to_owned(),
            card: None,
        },
        SessionEvent::Activity {
            line: "read src/parse.rs".to_owned(),
            tool: None,
        },
        SessionEvent::Reply {
            text: "this session runs on `glm-5.3-flash`".to_owned(),
            command: "!model".to_owned(),
            card: None,
        },
        SessionEvent::Post {
            text: "Fixed for <@github:octocat>, reset <t:1790273365:R>.".to_owned(),
            card: None,
        },
        SessionEvent::Notice {
            text: "<@github:octocat> [done] glm-5.3-flash | 16m27s".to_owned(),
            level: NoticeLevel::Done,
            card: None,
        },
    ] {
        view.observe(&event).await.expect("observed");
    }
    assert!(
        posted.lock().unwrap().is_empty(),
        "nothing until the turn ends"
    );

    view.observe(&SessionEvent::Busy { busy: false })
        .await
        .expect("posted");
    view.observe(&SessionEvent::Busy { busy: false })
        .await
        .expect("nothing more");

    let posted = posted.lock().unwrap();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].0, "/repos/Acme/widgets/issues/7/comments");
    assert_eq!(
        posted[0].1["body"],
        "Fixed for @octocat, reset 2026-09-24 18:09 UTC."
    );
}

/// An answer arrives a paragraph at a time. The issue is told the whole of
/// the last one, and paragraphs said before the agent last acted are not
/// part of it.
#[tokio::test]
async fn the_issue_is_told_the_whole_last_answer_however_it_arrived() {
    let (api, posted) = api();
    let view = threads(api)
        .view_for("github:Acme/widgets#7")
        .expect("an issue");

    for event in [
        SessionEvent::Post {
            text: "Looking first.\n\n".to_owned(),
            card: None,
        },
        SessionEvent::Activity {
            line: "read src/parse.rs".to_owned(),
            tool: None,
        },
        SessionEvent::Post {
            text: "Fixed the parser.\n\n".to_owned(),
            card: None,
        },
        SessionEvent::Post {
            text: "It now reads empty input.".to_owned(),
            card: None,
        },
        SessionEvent::Busy { busy: false },
    ] {
        view.observe(&event).await.expect("observed");
    }

    let posted = posted.lock().unwrap();
    assert_eq!(
        posted[0].1["body"],
        "Fixed the parser.\n\nIt now reads empty input."
    );
}

/// A turn that failed before saying anything still tells the issue why, so
/// whoever asked there is not left waiting.
#[tokio::test]
async fn a_turn_that_said_nothing_tells_the_issue_why_it_failed() {
    let (api, posted) = api();
    let view = threads(api).view_for("github:o/r#2").expect("an issue");

    for event in [
        SessionEvent::Notice {
            text: "the turn failed: 429 rate limit exceeded".to_owned(),
            level: NoticeLevel::Warning,
            card: None,
        },
        SessionEvent::Busy { busy: false },
    ] {
        view.observe(&event).await.expect("observed");
    }

    assert_eq!(
        posted.lock().unwrap()[0].1["body"],
        "the turn failed: 429 rate limit exceeded"
    );
}

#[test]
fn chat_markup_is_said_in_githubs_terms_and_the_rest_is_left_alone() {
    assert_eq!(for_forge("hi <@github:OctoCat>"), "hi @OctoCat");
    assert_eq!(for_forge("at <t:0:R>"), "at 1970-01-01 00:00 UTC");
    assert_eq!(
        for_forge("<@123> kept, a < b > c, <t:x:R>, <open"),
        "<@123> kept, a < b > c, <t:x:R>, <open"
    );
}

#[test]
fn only_an_issue_has_a_view() {
    let (api, _) = api();
    assert!(threads(api).view_for("1552528761872851095").is_none());
}

#[test]
fn a_view_never_crosses_forge_kinds() {
    let (api, _) = api();
    let mut views = threads(api);
    assert!(views.view_for("forgejo:o/r#4").is_none());
    Arc::get_mut(&mut views).unwrap().kind = crate::config::schema::ForgeKind::Forgejo;
    assert!(views.view_for("github:o/r#4").is_none());
    assert!(views.view_for("forgejo:o/r#4").is_some());
}

#[tokio::test]
async fn comments_are_cut_short_only_when_the_translated_text_exceeds_the_limit() {
    let (api, posted) = api();
    let view = threads(api).view_for("github:o/r#2").unwrap();
    let limit = super::COMMENT_LIMIT;
    let suffix = "\n\n(cut short; the rest is in the session's transcript)";

    for text in [
        "\u{e9}".repeat(limit - 1),
        "\u{e9}".repeat(limit),
        "\u{e9}".repeat(limit + 1),
        format!("{}<@github:x>", "\u{e9}".repeat(limit - 2)),
    ] {
        view.observe(&SessionEvent::Post { text, card: None })
            .await
            .unwrap();
        view.observe(&SessionEvent::Busy { busy: false })
            .await
            .unwrap();
    }

    let posted = posted.lock().unwrap();
    assert_eq!(posted[0].1["body"], "\u{e9}".repeat(limit - 1));
    assert_eq!(posted[1].1["body"], "\u{e9}".repeat(limit));
    let cut = posted[2].1["body"].as_str().unwrap();
    assert_eq!(cut.chars().count(), limit);
    assert_eq!(
        cut,
        format!(
            "{}{suffix}",
            "\u{e9}".repeat(limit - suffix.chars().count())
        )
    );
    assert_eq!(
        posted[3].1["body"],
        format!("{}@x", "\u{e9}".repeat(limit - 2))
    );
}

#[tokio::test]
async fn comment_body_survives_json_serialization() {
    let (api, posted) = api();
    let view = threads(api).view_for("github:o/r#2").unwrap();
    let text = "Quotes \" and backslash \\, newline\n, null \0, and \u{96ea}";
    view.observe(&SessionEvent::Post {
        text: text.to_owned(),
        card: None,
    })
    .await
    .unwrap();
    view.observe(&SessionEvent::Busy { busy: false })
        .await
        .unwrap();
    let posted = posted.lock().unwrap();
    let wire = serde_json::to_string(&posted[0].1).unwrap();
    let decoded: Value = serde_json::from_str(&wire).unwrap();
    assert_eq!(decoded["body"], text);
}

#[tokio::test]
async fn failed_comment_is_reported_and_not_posted_again() {
    let calls = Arc::new(Mutex::new(0));
    let seen = Arc::clone(&calls);
    let api: Api = Arc::new(move |_, _| {
        *seen.lock().unwrap() += 1;
        Box::pin(async {
            ApiReply {
                status: 422,
                body: json!({"message": "invalid"}),
            }
        })
    });
    let view = threads(api).view_for("github:o/r#2").unwrap();
    view.observe(&SessionEvent::Post {
        text: "answer".to_owned(),
        card: None,
    })
    .await
    .unwrap();
    let error = view
        .observe(&SessionEvent::Busy { busy: false })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("422"));
    assert!(error.to_string().contains("invalid"));
    view.observe(&SessionEvent::Busy { busy: false })
        .await
        .unwrap();
    assert_eq!(*calls.lock().unwrap(), 1);
}
