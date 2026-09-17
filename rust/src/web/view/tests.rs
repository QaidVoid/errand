use std::sync::Arc;

use serde_json::Value;

use super::{NameLookup, WebView, without_mentions};
use crate::session::event::{
    EndReason, NoticeLevel, ReactionOutcome, SessionEvent, SessionUsage, ToolActivity,
};
use crate::session::views::SessionView;

fn usage() -> SessionUsage {
    SessionUsage {
        input: 10,
        output: 2,
        cache_read: 0,
        cache_write: 0,
        total_tokens: 12,
        cost: 0.01,
        context_tokens: 10,
        context_window: None,
        turns: 1,
        model: None,
    }
}

/// Reads the stream as the browser does, one event at a time.
async fn events_of(
    receiver: &mut tokio::sync::mpsc::Receiver<String>,
    count: usize,
) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    while found.len() < count {
        let Some(chunk) = receiver.recv().await else {
            break;
        };
        for block in chunk.split("\n\n") {
            let mut lines = block.lines();
            let event = lines.next().unwrap_or("").strip_prefix("event: ");
            let data = lines.next().unwrap_or("").strip_prefix("data: ");
            if let (Some(event), Some(data)) = (event, data) {
                found.push((
                    event.to_owned(),
                    serde_json::from_str(data).unwrap_or(Value::Null),
                ));
            }
        }
    }
    found.truncate(count);
    found
}

/// A reconnecting browser must end up correct rather than doubled.
#[tokio::test]
async fn the_stream_opens_by_telling_the_browser_to_clear_what_it_has() {
    let (_view, mut receiver) = WebView::new(None);

    assert_eq!(events_of(&mut receiver, 1).await.remove(0).0, "reset");
}

#[tokio::test]
async fn what_a_session_says_becomes_one_event_each_with_its_turn() {
    let (view, mut receiver) = WebView::new(None);
    view.observe(&SessionEvent::BeginTurn { turn: 3 })
        .await
        .unwrap();
    view.observe(&SessionEvent::Post {
        text: "I did the thing".to_owned(),
    })
    .await
    .unwrap();
    view.observe(&SessionEvent::Activity {
        line: "ran `ls`".to_owned(),
        tool: Some(ToolActivity {
            id: None,
            name: "bash".to_owned(),
            target: Some("ls".to_owned()),
            failed: None,
        }),
    })
    .await
    .unwrap();

    let events = events_of(&mut receiver, 3).await;

    assert_eq!(events[1].0, "entry");
    let message = events[1].1.clone();
    assert_eq!(message["kind"], "message");
    assert_eq!(message["text"], "I did the thing");
    assert_eq!(message["turn"], 3);
    assert_eq!(events[2].1["tool"]["name"], "bash");
}

#[tokio::test]
async fn state_is_sent_as_state_not_as_something_said() {
    let (view, mut receiver) = WebView::new(None);
    view.observe(&SessionEvent::Busy { busy: true })
        .await
        .unwrap();
    view.observe(&SessionEvent::Usage { usage: usage() })
        .await
        .unwrap();
    view.observe(&SessionEvent::Waiting {
        text: Some("waiting for a turn slot, position 2".to_owned()),
    })
    .await
    .unwrap();

    let events = events_of(&mut receiver, 4).await;

    let kinds: Vec<String> = events[1..].iter().map(|(event, _)| event.clone()).collect();
    assert_eq!(kinds, ["state", "state", "state"]);
    assert_eq!(events[1].1["busy"], true);
    assert_eq!(events[2].1["usage"]["totalTokens"], 12);
}

/// A stopped session can be picked up again, and a browser watching one
/// should see that happen rather than be disconnected from it.
#[tokio::test]
async fn a_session_ending_is_state_and_the_stream_stays_open() {
    let (view, mut receiver) = WebView::new(None);
    view.observe(&SessionEvent::Close {
        reason: EndReason::Idle,
    })
    .await
    .unwrap();

    let events = events_of(&mut receiver, 2).await;

    assert_eq!(events[1].1["ended"], true);
    assert_eq!(events[1].1["busy"], false);
    assert!(!view.is_closed());
}

/// Nothing said after the browser has gone is worth trying to send.
#[tokio::test]
async fn a_closed_view_accepts_calls_and_sends_nothing() {
    let (view, mut receiver) = WebView::new(None);
    let _ = receiver.recv().await;
    receiver.close();

    view.observe(&SessionEvent::Post {
        text: "into the void".to_owned(),
    })
    .await
    .unwrap();
    view.observe(&SessionEvent::Busy { busy: true })
        .await
        .unwrap();

    assert!(view.is_closed());
    assert!(receiver.try_recv().is_err());
}

/// A command's answer belongs to the thread it was asked in.
#[tokio::test]
async fn what_a_thread_shows_and_the_interface_does_not_is_not_sent() {
    let (view, mut receiver) = WebView::new(None);
    view.observe(&SessionEvent::Reply {
        text: "a listing".to_owned(),
        command: "ls".to_owned(),
    })
    .await
    .unwrap();
    view.observe(&SessionEvent::Reaction {
        message_id: "m1".to_owned(),
        outcome: ReactionOutcome::Succeeded,
    })
    .await
    .unwrap();
    view.observe(&SessionEvent::Post {
        text: "the only thing said".to_owned(),
    })
    .await
    .unwrap();

    let events = events_of(&mut receiver, 2).await;

    assert_eq!(events.len(), 2);
    assert!(events[1].1.to_string().contains("the only thing said"));
}

/// `<@1523363748427993218> [done]` names nobody to a reader in a browser.
#[test]
fn a_mention_becomes_a_name_or_goes_if_the_name_is_not_known() {
    let names: Option<NameLookup> = Some(Arc::new(|id: &str| {
        (id == "111").then(|| "amelia".to_owned())
    }));

    assert_eq!(
        without_mentions("<@111> [done] 12k tokens", names.as_ref()),
        "@amelia [done] 12k tokens"
    );
    assert_eq!(without_mentions("<@999> [done]", names.as_ref()), "[done]");
    assert_eq!(
        without_mentions("<@!111> hello", names.as_ref()),
        "@amelia hello"
    );
    assert_eq!(
        without_mentions("nothing to rewrite", names.as_ref()),
        "nothing to rewrite"
    );
}

#[tokio::test]
async fn what_a_session_says_reaches_the_browser_with_mentions_rewritten() {
    let names: Option<NameLookup> = Some(Arc::new(|id: &str| {
        (id == "111").then(|| "amelia".to_owned())
    }));
    let (view, mut receiver) = WebView::new(names);
    view.observe(&SessionEvent::Notice {
        text: "<@111> [done]".to_owned(),
        level: NoticeLevel::Done,
    })
    .await
    .unwrap();

    let events = events_of(&mut receiver, 2).await;

    assert!(events[1].1.to_string().contains("@amelia [done]"));
}
