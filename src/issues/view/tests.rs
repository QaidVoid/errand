//! Tests for an issue as a thread.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{IssueThreads, for_github};
use crate::log::{LogFields, Logger};
use crate::session::event::{NoticeLevel, SessionEvent};
use crate::session::manager::ThreadFactory;
use crate::session::pr::{Api, ApiReply};
use crate::session::session::IncomingMessage;

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
        api,
        token: "ghp".to_owned(),
        log: Logger::new(LogFields::new(), Arc::new(|_level, _line| {})),
    })
}

/// A tracker notifies everybody watching of every comment, so a turn is
/// gathered and posted once, when it ends, in GitHub's terms.
#[tokio::test]
async fn a_turn_is_one_comment_posted_when_it_ends() {
    let (api, posted) = api();
    let view = threads(api)
        .port_for("github:QaidVoid/edu#7".to_owned())
        .await
        .expect("an issue");

    for event in [
        SessionEvent::BeginTurn { turn: 1 },
        SessionEvent::Post {
            text: "Found it: the last line has no newline.".to_owned(),
        },
        SessionEvent::Activity {
            line: "read src/parse.rs".to_owned(),
            tool: None,
        },
        SessionEvent::Post {
            text: "Fixed on the branch `fix-last-line`.".to_owned(),
        },
        SessionEvent::Notice {
            text: "<@github:qaidvoid> done, resets <t:1790273365:R>".to_owned(),
            level: NoticeLevel::Done,
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
    assert_eq!(posted[0].0, "/repos/QaidVoid/edu/issues/7/comments");
    assert_eq!(
        posted[0].1["body"],
        "Found it: the last line has no newline.\n\nFixed on the branch `fix-last-line`.\n\n\
         @qaidvoid done, resets 2026-09-24 18:09 UTC"
    );
}

/// A command is answered at once, apart from any turn.
#[tokio::test]
async fn a_command_is_answered_at_once() {
    let (api, posted) = api();
    let view = threads(api)
        .port_for("github:o/r#2".to_owned())
        .await
        .expect("an issue");

    view.observe(&SessionEvent::Reply {
        text: "this session runs on `glm-5.3-flash`".to_owned(),
        command: "!model".to_owned(),
    })
    .await
    .expect("posted");

    assert_eq!(posted.lock().unwrap().len(), 1);
}

#[test]
fn chat_markup_is_said_in_githubs_terms_and_the_rest_is_left_alone() {
    assert_eq!(for_github("hi <@github:QaidVoid>"), "hi @QaidVoid");
    assert_eq!(for_github("at <t:0:R>"), "at 1970-01-01 00:00 UTC");
    assert_eq!(
        for_github("<@123> kept, a < b > c, <t:x:R>, <open"),
        "<@123> kept, a < b > c, <t:x:R>, <open"
    );
}

/// The issue a session is started on is its thread; nothing is made, and a
/// chat thread is not an issue.
#[tokio::test]
async fn the_issue_is_the_thread_and_a_chat_thread_is_not_one() {
    let (api, _) = api();
    let issues = threads(api);
    let message = |channel_id: &str| IncomingMessage {
        id: "github-comment:1".to_owned(),
        author_id: "github:qaidvoid".to_owned(),
        channel_id: channel_id.to_owned(),
        author_name: None,
        content: String::new(),
        attachments: Vec::new(),
    };

    let made = Arc::clone(&issues)
        .create(message("github:o/r#3"), "unused".to_owned())
        .await
        .expect("made");
    assert_eq!(made.id, "github:o/r#3");
    assert!(
        Arc::clone(&issues)
            .create(message("1552528761872851095"), "unused".to_owned())
            .await
            .is_err()
    );
    assert!(
        issues
            .port_for("1552528761872851095".to_owned())
            .await
            .is_none()
    );
}
