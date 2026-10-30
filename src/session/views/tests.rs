use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use super::{
    DEFAULT_TRANSCRIPT_LIMIT, Held, Recorder, SessionView, ViewError, ViewFanOut, WITHDRAWN_NOTE,
    label_of, shown_text,
};
use crate::log::{LogFields, Logger};
use crate::session::event::{Delegated, NoticeLevel, SessionEvent, SessionUsage};

fn silent() -> Logger {
    Logger::new(LogFields::new(), Arc::new(|_level, _line| {}))
}

/// A view that writes down everything it was told, in order.
struct FakeView {
    seen: Mutex<Vec<String>>,
    /// Deliveries whose label fails, standing in for a surface that broke.
    fail_on: Vec<&'static str>,
}

impl FakeView {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            fail_on: Vec::new(),
        })
    }

    fn new_failing_on(fail_on: Vec<&'static str>) -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            fail_on,
        })
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    fn said(&self, needle: &str) -> bool {
        self.seen().iter().any(|line| line.contains(needle))
    }
}

impl SessionView for FakeView {
    fn observe<'a>(
        &'a self,
        event: &'a SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), ViewError>> + Send + 'a>> {
        Box::pin(async move {
            if self.fail_on.contains(&label_of(event)) {
                return Err(Box::new(std::io::Error::other("this view is gone")) as ViewError);
            }
            self.seen.lock().unwrap().push(render(event));
            Ok(())
        })
    }
}

/// One line per event, the way the TypeScript fake wrote them.
fn render(event: &SessionEvent) -> String {
    match event {
        SessionEvent::Post { text } => format!("post:{text}"),
        SessionEvent::Question { title, .. } => format!("question:{title}"),
        SessionEvent::Prompt { author, text, .. } => format!("prompt:{author}:{text}"),
        SessionEvent::Aside { author, text, .. } => format!("aside:{author}:{text}"),
        SessionEvent::Notice { text, level } => {
            format!("notice:{}:{text}", level.as_str())
        }
        SessionEvent::Thinking { text } => format!("thinking:{text}"),
        SessionEvent::Reply { text, command } => format!("reply:{command}:{text}"),
        SessionEvent::ToolResult { result } => {
            format!("result:{}:{}", result.id, result.failed)
        }
        SessionEvent::Activity { line, .. } => format!("activity:{line}"),
        SessionEvent::Delegation { delegated } => format!(
            "delegated:{}",
            delegated
                .model
                .clone()
                .or_else(|| delegated.refused.clone())
                .unwrap_or_else(|| "undefined".to_owned())
        ),
        SessionEvent::Diff {
            path,
            added,
            removed,
            ..
        } => format!("diff:{path}:+{added}-{removed}"),
        SessionEvent::Attachment { name, file, .. } => {
            format!("attachment:{name}:{}", file.as_deref().unwrap_or("-"))
        }
        SessionEvent::Upload { name, .. } => format!("upload:{name}"),
        SessionEvent::Close { .. } => String::new(),
        SessionEvent::Usage { usage } => format!("usage:{}", usage.total_tokens),
        SessionEvent::Waiting { text } => {
            format!("waiting:{}", text.as_deref().unwrap_or("null"))
        }
        SessionEvent::Reaction {
            message_id,
            outcome,
        } => format!("reaction:{message_id}:{}", outcome.as_str()),
        SessionEvent::Busy { busy } => format!("busy:{busy}"),
        SessionEvent::BeginTurn { turn } => format!("turn:{turn}"),
    }
}

fn usage(total_tokens: u64) -> SessionUsage {
    SessionUsage {
        input: 10,
        output: 2,
        cache_read: 90,
        cache_write: 0,
        total_tokens,
        cost: 0.01,
        context_tokens: 100,
        context_window: None,
        turns: 1,
        model: None,
    }
}

fn fan_out() -> Arc<ViewFanOut> {
    Arc::new(ViewFanOut::new(silent()))
}

fn fan_out_with(limit: usize, recorder: Option<Arc<dyn Recorder>>) -> Arc<ViewFanOut> {
    Arc::new(ViewFanOut::with_recorder(silent(), limit, recorder))
}

/// Remembers what the fan-out wrote down, as the transcript would read it.
struct WrittenDown(Mutex<Vec<(u32, String)>>);

impl Recorder for WrittenDown {
    fn append(&self, entry: &SessionEvent, turn: u32) {
        let call = serde_json::to_value(entry)
            .unwrap()
            .get("call")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.0.lock().unwrap().push((turn, call));
    }
}

#[tokio::test]
async fn everything_reported_reaches_every_attached_view() {
    let fan = fan_out();
    let first = FakeView::new();
    let second = FakeView::new();
    fan.clone().attach(first.clone()).await;
    fan.clone().attach(second.clone()).await;

    fan.send(SessionEvent::Post {
        text: "hello".to_owned(),
    })
    .await;
    fan.send(SessionEvent::Notice {
        text: "started".to_owned(),
        level: NoticeLevel::Started,
    })
    .await;

    assert_eq!(first.seen(), ["post:hello", "notice:started:started"]);
    assert_eq!(second.seen(), first.seen());
}

/// A browser that closed must not stop the chat thread being told.
#[tokio::test]
async fn a_view_that_fails_is_skipped_and_the_others_still_hear_it() {
    let fan = fan_out();
    let good = FakeView::new();
    let broken = FakeView::new_failing_on(vec!["post"]);

    fan.clone().attach(broken.clone()).await;
    fan.clone().attach(good.clone()).await;
    fan.send(SessionEvent::Post {
        text: "still delivered".to_owned(),
    })
    .await;

    assert_eq!(good.seen(), ["post:still delivered"]);
}

/// A view that fails on some things and not others keeps being sent the rest.
#[tokio::test]
async fn a_view_that_fails_synchronously_is_skipped_too() {
    let fan = fan_out();
    let good = FakeView::new();
    let broken = FakeView::new_failing_on(vec!["turn", "busy"]);

    fan.clone().attach(broken.clone()).await;
    fan.clone().attach(good.clone()).await;

    fan.send(SessionEvent::BeginTurn { turn: 3 }).await;
    fan.send(SessionEvent::Busy { busy: true }).await;
    fan.send(SessionEvent::Post {
        text: "still delivered".to_owned(),
    })
    .await;

    assert_eq!(good.seen(), ["turn:3", "busy:true", "post:still delivered"]);
}

#[tokio::test]
async fn a_view_that_attaches_late_is_shown_what_it_missed() {
    let fan = fan_out();
    fan.send(SessionEvent::Post {
        text: "before it joined".to_owned(),
    })
    .await;
    fan.send(SessionEvent::Activity {
        line: "ran something".to_owned(),
        tool: None,
    })
    .await;

    let late = FakeView::new();
    fan.clone().attach(late.clone()).await;

    assert_eq!(
        late.seen(),
        ["turn:0", "post:before it joined", "activity:ran something"]
    );
}

#[tokio::test]
async fn detaching_leaves_the_session_and_the_other_views_alone() {
    let fan = fan_out();
    let staying = FakeView::new();
    let leaving = FakeView::new();
    fan.clone().attach(staying.clone()).await;
    let attached = fan.clone().attach(leaving.clone()).await;

    attached.detach();
    fan.send(SessionEvent::Post {
        text: "after one left".to_owned(),
    })
    .await;

    assert_eq!(fan.size(), 1);
    assert_eq!(staying.seen(), ["post:after one left"]);
    assert!(leaving.seen().is_empty());
}

/// Only the latest total is meaningful, so a late view is told what it is now
/// rather than watching it climb through every turn.
#[tokio::test]
async fn cost_is_state_and_the_latest_is_given_none_of_it_is_replayed() {
    let fan = fan_out();
    fan.send(SessionEvent::Usage { usage: usage(100) }).await;
    fan.send(SessionEvent::Usage { usage: usage(250) }).await;

    let late = FakeView::new();
    fan.clone().attach(late.clone()).await;

    let usage_lines: Vec<String> = late
        .seen()
        .into_iter()
        .filter(|line| line.starts_with("usage:"))
        .collect();
    assert_eq!(usage_lines, ["usage:250"]);
}

#[tokio::test]
async fn a_late_view_is_told_the_queue_position_and_that_work_is_running() {
    let fan = fan_out();
    fan.send(SessionEvent::Waiting {
        text: Some("waiting for a turn slot, position 2".to_owned()),
    })
    .await;
    fan.send(SessionEvent::Busy { busy: true }).await;

    let late = FakeView::new();
    fan.clone().attach(late.clone()).await;

    assert!(late.said("waiting:waiting for a turn slot, position 2"));
    assert!(late.said("busy:true"));
}

/// A view that just attached already assumes nothing is running.
#[tokio::test]
async fn a_session_that_is_not_working_says_nothing_about_it() {
    let fan = fan_out();
    fan.send(SessionEvent::Busy { busy: false }).await;

    let late = FakeView::new();
    fan.clone().attach(late.clone()).await;

    assert!(!late.seen().iter().any(|line| line.starts_with("busy:")));
}

#[tokio::test]
async fn the_record_is_bounded_and_a_replay_admits_what_it_dropped() {
    let fan = fan_out_with(3, None);
    for text in ["one", "two", "three", "four", "five"] {
        fan.send(SessionEvent::Post {
            text: text.to_owned(),
        })
        .await;
    }

    let late = FakeView::new();
    fan.clone().attach(late.clone()).await;

    assert_eq!(fan.dropped_count(), 2);
    assert!(late.seen()[0].contains("2 earlier line(s) not kept"));
    // The notice about the drop is itself a post, so it is not one of the three.
    let posts = late
        .seen()
        .iter()
        .skip(1)
        .filter(|line| line.starts_with("post:"))
        .count();
    assert_eq!(posts, 3);
}

#[tokio::test]
async fn a_turn_is_announced_once_at_its_boundary_not_on_every_line() {
    let fan = fan_out();
    fan.send(SessionEvent::BeginTurn { turn: 1 }).await;
    fan.send(SessionEvent::Post {
        text: "first".to_owned(),
    })
    .await;
    fan.send(SessionEvent::Post {
        text: "second".to_owned(),
    })
    .await;
    fan.send(SessionEvent::BeginTurn { turn: 2 }).await;
    fan.send(SessionEvent::Post {
        text: "third".to_owned(),
    })
    .await;

    let late = FakeView::new();
    fan.clone().attach(late.clone()).await;

    assert_eq!(
        late.seen(),
        [
            "turn:1",
            "post:first",
            "post:second",
            "turn:2",
            "post:third"
        ]
    );
}

/// A command's answer is kept, so a browser that opens the session later
/// sees what a command it ran said, while a refusal stays with the person it
/// refused.
#[tokio::test]
async fn a_reply_is_kept_and_a_refusal_is_not() {
    let fan = fan_out();
    let live = FakeView::new();
    fan.clone().attach(live.clone()).await;
    fan.send(SessionEvent::Reply {
        text: "a listing".to_owned(),
        command: "!ls src".to_owned(),
    })
    .await;
    fan.send(SessionEvent::Reply {
        text: "not yours".to_owned(),
        command: crate::session::event::REFUSAL.to_owned(),
    })
    .await;

    let late = FakeView::new();
    fan.clone().attach(late.clone()).await;

    assert!(live.said("reply:!ls src:a listing"));
    assert!(live.said("reply:refused:not yours"));
    assert!(late.said("reply:!ls src:a listing"));
    assert!(!late.seen().iter().any(|line| line.contains("not yours")));
}

#[tokio::test]
async fn what_is_reported_is_written_down_for_later_with_its_turn() {
    let sink = Arc::new(WrittenDown(Mutex::new(Vec::new())));
    let recorded: Arc<dyn Recorder> = sink.clone();
    let fan = fan_out_with(DEFAULT_TRANSCRIPT_LIMIT, Some(recorded));

    fan.send(SessionEvent::BeginTurn { turn: 4 }).await;
    fan.send(SessionEvent::Post {
        text: "said something".to_owned(),
    })
    .await;
    fan.send(SessionEvent::Activity {
        line: "did something".to_owned(),
        tool: None,
    })
    .await;

    assert_eq!(
        *sink.0.lock().unwrap(),
        vec![(4, "post".to_owned()), (4, "activity".to_owned())]
    );
}

/// Restoring twice would otherwise double a session's history.
#[tokio::test]
async fn restoring_replaces_what_is_held_rather_than_adding_to_it() {
    let fan = fan_out();
    let held = vec![
        Held {
            turn: Some(1),
            at: 0,
            entry: SessionEvent::Post {
                text: "from before".to_owned(),
            },
        },
        Held {
            turn: Some(2),
            at: 0,
            entry: SessionEvent::Usage { usage: usage(900) },
        },
    ];

    fan.restore(&held, 0);
    fan.restore(&held, 0);

    let late = FakeView::new();
    fan.clone().attach(late.clone()).await;

    assert_eq!(
        late.seen()
            .iter()
            .filter(|line| **line == "post:from before")
            .count(),
        1
    );
    assert_eq!(fan.current_turn(), 2);
    let usage_lines: Vec<String> = late
        .seen()
        .into_iter()
        .filter(|line| line.starts_with("usage:"))
        .collect();
    assert_eq!(usage_lines, ["usage:900"]);
}

/// A resumed session must not label a new exchange with a number already used.
#[test]
fn restoring_carries_the_turn_numbering_on() {
    let fan = fan_out();
    assert_eq!(fan.current_turn(), 0);

    fan.restore(
        &[
            Held {
                turn: Some(1),
                at: 0,
                entry: SessionEvent::Post {
                    text: "one".to_owned(),
                },
            },
            Held {
                turn: Some(3),
                at: 0,
                entry: SessionEvent::Post {
                    text: "three".to_owned(),
                },
            },
        ],
        0,
    );

    assert_eq!(fan.current_turn(), 3);
}

/// A resumed session must not lose the delegations it made.
#[tokio::test]
async fn a_delegation_is_replayed_to_a_view_that_attaches_later() {
    let fan = fan_out();
    fan.send(SessionEvent::Delegation {
        delegated: Delegated {
            question: "what failed?".to_owned(),
            model: Some("flash".to_owned()),
            describes: Some("call t1".to_owned()),
            ..Default::default()
        },
    })
    .await;

    let late = FakeView::new();
    fan.clone().attach(late.clone()).await;

    assert_eq!(late.seen(), ["turn:0", "delegated:flash"]);
}

/// A withdrawn turn keeps its place and loses its words. Showing the empty
/// text would read as somebody having said nothing, which is untrue and makes
/// the replies around it stop following.
#[test]
fn a_withdrawn_entry_is_shown_as_a_marker_not_as_silence() {
    assert_eq!(shown_text(false, "hello"), "hello");
    assert_eq!(shown_text(true, ""), WITHDRAWN_NOTE);
    // The text is gone from the entry, so the marker is the only thing
    // standing between a reader and an apparently empty turn.
    assert!(!shown_text(true, "").is_empty());
}

/// Remembers when each replayed thing is said to have happened.
struct Timed(Mutex<Vec<(String, i64)>>);

impl SessionView for Timed {
    fn observe<'a>(
        &'a self,
        event: &'a SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), ViewError>> + Send + 'a>> {
        self.observe_at(event, -1)
    }

    fn observe_at<'a>(
        &'a self,
        event: &'a SessionEvent,
        at: i64,
    ) -> Pin<Box<dyn Future<Output = Result<(), ViewError>> + Send + 'a>> {
        self.0
            .lock()
            .unwrap()
            .push((label_of(event).to_owned(), at));
        Box::pin(async { Ok(()) })
    }
}

/// A view attaching late is told when each thing happened, not when it
/// attached, so a browser opened on a running session shows real times.
#[tokio::test]
async fn a_replay_carries_when_each_thing_happened() {
    let fan = Arc::new(ViewFanOut::new(silent()).with_clock(|| 1_234));
    fan.send(SessionEvent::Prompt {
        author: "amelia".to_owned(),
        text: "go".to_owned(),
        id: None,
        withdrawn: false,
        from_interface: false,
    })
    .await;
    fan.send(SessionEvent::Post {
        text: "done".to_owned(),
    })
    .await;

    let late = Arc::new(Timed(Mutex::new(Vec::new())));
    fan.clone().attach(late.clone()).await;

    let seen = late.0.lock().unwrap().clone();
    assert!(seen.contains(&("prompt".to_owned(), 1_234)), "{seen:?}");
    assert!(seen.contains(&("post".to_owned(), 1_234)), "{seen:?}");
}

/// Keeps every file it is given, as the transcript does, under a fixed name.
struct Keeping(Mutex<Vec<String>>);

impl Recorder for Keeping {
    fn append(&self, entry: &SessionEvent, _turn: u32) {
        self.0
            .lock()
            .unwrap()
            .push(serde_json::to_string(entry).unwrap());
    }

    fn keep(&self, name: &str, _bytes: &[u8]) -> Option<String> {
        Some(format!("kept-{name}"))
    }
}

/// An upload is recorded as where it was kept, never with its bytes, and a
/// view is shown the upload and then that, so a thread can post the file and
/// an interface can fetch it.
#[tokio::test]
async fn an_upload_is_recorded_as_where_it_was_kept() {
    let sink = Arc::new(Keeping(Mutex::new(Vec::new())));
    let recorded: Arc<dyn Recorder> = sink.clone();
    let fan = Arc::new(ViewFanOut::with_recorder(
        Logger::new(LogFields::new(), Arc::new(|_, _| {})),
        DEFAULT_TRANSCRIPT_LIMIT,
        Some(recorded),
    ));
    let live = FakeView::new();
    fan.clone().attach(live.clone()).await;

    fan.send(SessionEvent::Upload {
        name: "image.png".to_owned(),
        bytes: vec![1, 2, 3],
        caption: "> a lighthouse".to_owned(),
    })
    .await;

    let written = sink.0.lock().unwrap().clone();
    assert_eq!(written.len(), 1, "{written:?}");
    let entry: serde_json::Value = serde_json::from_str(&written[0]).unwrap();
    assert_eq!(entry["call"], "attachment");
    assert_eq!(entry["file"], "kept-image.png");
    assert_eq!(entry["caption"], "> a lighthouse");
    assert_eq!(entry["size"], 3);
    let seen = live.seen();
    let upload = seen
        .iter()
        .position(|line| line.starts_with("upload"))
        .expect("the upload");
    let attachment = seen
        .iter()
        .position(|line| line.starts_with("attachment"))
        .expect("what it was kept as");
    assert!(upload < attachment, "{seen:?}");
}
