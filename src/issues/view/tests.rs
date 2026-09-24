//! Tests for an issue as a thread.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{IssueThreads, for_github};
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
        .view_for("github:QaidVoid/edu#7")
        .expect("an issue");

    for event in [
        SessionEvent::BeginTurn { turn: 1 },
        SessionEvent::Post {
            text: "Short answer for the thread, then the detail.".to_owned(),
        },
        SessionEvent::Activity {
            line: "read src/parse.rs".to_owned(),
            tool: None,
        },
        SessionEvent::Reply {
            text: "this session runs on `glm-5.3-flash`".to_owned(),
            command: "!model".to_owned(),
        },
        SessionEvent::Post {
            text: "Fixed for <@github:qaidvoid>, reset <t:1790273365:R>.".to_owned(),
        },
        SessionEvent::Notice {
            text: "<@github:qaidvoid> [done] glm-5.3-flash | 16m27s".to_owned(),
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
        "Fixed for @qaidvoid, reset 2026-09-24 18:09 UTC."
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
    assert_eq!(for_github("hi <@github:QaidVoid>"), "hi @QaidVoid");
    assert_eq!(for_github("at <t:0:R>"), "at 1970-01-01 00:00 UTC");
    assert_eq!(
        for_github("<@123> kept, a < b > c, <t:x:R>, <open"),
        "<@123> kept, a < b > c, <t:x:R>, <open"
    );
}

#[test]
fn only_an_issue_has_a_view() {
    let (api, _) = api();
    assert!(threads(api).view_for("1552528761872851095").is_none());
}
