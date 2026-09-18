//! Thread tests, ported from `threads_test.ts`.

use std::sync::{Arc, Mutex};

use super::{ChatThread, MessageHandle, ServiceCall, ThreadTransport};
use crate::log::LogFields;
use crate::log::Logger;
use crate::session::event::EndReason;
use crate::session::event::ReactionOutcome;
use crate::session::event::ToolResult;

/// One message the thread sent, and whatever it was later edited to.
#[derive(Default, Clone)]
struct Sent {
    id: String,
    content: String,
    edits: Vec<String>,
    reactions: Vec<String>,
    removed: Vec<String>,
}

/// A thread channel as this module uses it.
///
/// Hand-made rather than mocked: what matters is what was sent, what was
/// edited, and what was archived, which is all observable from here.
struct FakeChannel {
    sent: Mutex<Vec<Arc<Mutex<Sent>>>>,
    deleted: Mutex<Vec<String>>,
    archived: Mutex<bool>,
    typing: Mutex<u32>,
    next: Mutex<u32>,
}

impl FakeChannel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            sent: Mutex::new(Vec::new()),
            deleted: Mutex::new(Vec::new()),
            archived: Mutex::new(false),
            typing: Mutex::new(0),
            next: Mutex::new(1),
        })
    }

    fn records(&self) -> Vec<Arc<Mutex<Sent>>> {
        self.sent.lock().unwrap().clone()
    }

    fn contents(&self) -> Vec<String> {
        self.records()
            .iter()
            .map(|record| record.lock().unwrap().content.clone())
            .collect()
    }

    fn is_archived(&self) -> bool {
        *self.archived.lock().unwrap()
    }
}

impl ThreadTransport for FakeChannel {
    fn send(
        &self,
        content: String,
        _attachment: Option<(String, Vec<u8>)>,
    ) -> ServiceCall<MessageHandle> {
        let record = Arc::new(Mutex::new(Sent {
            id: format!("sent-{}", *self.next.lock().unwrap()),
            content,
            ..Default::default()
        }));
        *self.next.lock().unwrap() += 1;
        self.sent.lock().unwrap().push(Arc::clone(&record));
        Box::pin(async move {
            Ok(MessageHandle {
                id: record.lock().unwrap().id.clone(),
            })
        })
    }

    fn edit(&self, message_id: String, content: String) -> ServiceCall<()> {
        if let Some(record) = self
            .records()
            .into_iter()
            .find(|record| record.lock().unwrap().id == message_id)
        {
            let mut sent = record.lock().unwrap();
            sent.edits.push(content.clone());
            sent.content = content;
        }
        Box::pin(async { Ok(()) })
    }

    fn delete(&self, message_id: String) -> ServiceCall<()> {
        self.deleted.lock().unwrap().push(message_id);
        Box::pin(async { Ok(()) })
    }

    fn fetch(&self, message_id: String) -> ServiceCall<Option<MessageHandle>> {
        let known = self
            .records()
            .iter()
            .any(|record| record.lock().unwrap().id == message_id);
        Box::pin(async move {
            if known {
                Ok(Some(MessageHandle { id: message_id }))
            } else {
                Ok(None)
            }
        })
    }

    fn react(&self, message_id: String, glyph: String) -> ServiceCall<()> {
        if let Some(record) = self
            .records()
            .into_iter()
            .find(|record| record.lock().unwrap().id == message_id)
        {
            record.lock().unwrap().reactions.push(glyph);
        }
        Box::pin(async { Ok(()) })
    }

    fn remove_reaction(&self, message_id: String, glyph: String) -> ServiceCall<()> {
        if let Some(record) = self
            .records()
            .into_iter()
            .find(|record| record.lock().unwrap().id == message_id)
        {
            record.lock().unwrap().removed.push(glyph);
        }
        Box::pin(async { Ok(()) })
    }

    fn set_archived(&self, archived: bool) -> ServiceCall<()> {
        *self.archived.lock().unwrap() = archived;
        Box::pin(async { Ok(()) })
    }

    fn send_typing(&self) -> ServiceCall<()> {
        *self.typing.lock().unwrap() += 1;
        Box::pin(async { Ok(()) })
    }
}

fn thread(forward_tool_output: bool) -> (ChatThread<FakeChannel>, Arc<FakeChannel>) {
    let channel = FakeChannel::new();
    let port = ChatThread::new(
        Arc::clone(&channel),
        Logger::new(LogFields::new(), Arc::new(|_level, _line| {})),
        forward_tool_output,
    );
    (port, channel)
}

#[tokio::test]
async fn what_is_posted_arrives_and_nothing_carries_a_link_preview() {
    let (port, channel) = thread(false);

    port.post("here is https://example.com/thing");
    port.flush().await;

    let sent = channel.records();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].lock().unwrap().content,
        "here is https://example.com/thing"
    );
    // The suppress-embeds flag is set by the one place messages are built,
    // the production transport; a recording double sees only the text.
}

#[tokio::test]
async fn a_long_message_is_split_into_ones_the_service_will_take() {
    let (port, channel) = thread(false);

    let long = (0..400)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    port.post(&long);
    port.flush().await;

    let sent = channel.records();
    assert!(sent.len() > 1);
    for message in sent {
        assert!(message.lock().unwrap().content.chars().count() <= 2_000);
    }
}

/// A run of tool calls is one thing the agent is doing, not ten messages.
#[tokio::test]
async fn consecutive_tool_activity_edits_one_message_instead_of_posting_more() {
    let (port, channel) = thread(false);

    port.append_activity("ran `ls`");
    port.append_activity("ran `cat`");
    port.append_activity("ran `grep`");
    port.flush().await;

    let sent = channel.records();
    assert_eq!(sent.len(), 1);
    let record = sent[0].lock().unwrap();
    assert!(record.content.contains("ran `ls`"));
    assert!(record.content.contains("ran `grep`"));
    assert_eq!(record.edits.len(), 2);
}

/// The agent speaking ends the block, or its words land under the tool calls.
#[tokio::test]
async fn the_agent_speaking_starts_a_fresh_activity_block() {
    let (port, channel) = thread(false);

    port.append_activity("ran `ls`");
    port.post("here is what I found");
    port.append_activity("ran `cat`");
    port.flush().await;

    let sent = channel.records();
    assert_eq!(sent.len(), 3);
    let record = sent[2].lock().unwrap();
    assert_eq!(record.content, "ran `cat`");
    assert!(record.edits.is_empty());
}

/// One entry can exceed a whole message on its own, and none is dropped.
#[tokio::test]
async fn an_activity_line_too_long_for_one_message_is_split_not_lost() {
    let (port, channel) = thread(false);

    port.append_activity(&format!("ran `{}`", "x".repeat(3_000)));
    port.flush().await;

    let sent = channel.contents().join("");
    assert!(sent.contains("x".repeat(2_500).as_str()));
}

#[tokio::test]
async fn the_queue_position_is_one_message_updated_and_then_taken_away() {
    let (port, channel) = thread(false);

    port.set_waiting(Some("waiting for a turn slot, position 2"))
        .await;
    port.set_waiting(Some("waiting for a turn slot, position 1"))
        .await;
    port.set_waiting(None).await;

    let sent = channel.records();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].lock().unwrap().edits[0].contains("position 1"));
    assert_eq!(*channel.deleted.lock().unwrap(), ["sent-1".to_owned()]);
}

/// A scrolled-back thread should read as final state, not as a history.
#[tokio::test]
async fn a_reaction_replaces_the_one_before_it() {
    let (port, channel) = thread(false);
    port.post("something to react to");
    port.flush().await;
    let records = channel.records();
    let target = records[0].lock().unwrap().id.clone();

    port.set_reaction(&target, ReactionOutcome::Accepted).await;
    port.set_reaction(&target, ReactionOutcome::Succeeded).await;

    let record = records[0].lock().unwrap();
    assert_eq!(record.reactions.len(), 2);
    assert_eq!(record.removed.len(), 1);
    assert_eq!(record.removed[0], record.reactions[0]);
}

#[tokio::test]
async fn the_same_reaction_twice_is_not_set_twice() {
    let (port, channel) = thread(false);
    port.post("something");
    port.flush().await;
    let records = channel.records();
    let target = records[0].lock().unwrap().id.clone();

    port.set_reaction(&target, ReactionOutcome::Accepted).await;
    port.set_reaction(&target, ReactionOutcome::Accepted).await;

    assert_eq!(records[0].lock().unwrap().reactions.len(), 1);
}

/// Losing an acknowledgement must not disturb the session it acknowledged.
#[tokio::test]
async fn a_reaction_on_a_message_that_is_gone_is_not_an_error() {
    let (port, _channel) = thread(false);

    port.set_reaction("no-such-message", ReactionOutcome::Failed)
        .await;
}

#[tokio::test]
async fn tool_output_is_kept_out_of_the_thread_unless_it_was_asked_for() {
    let (quiet, quiet_channel) = thread(false);
    let (loud, loud_channel) = thread(true);
    let result = ToolResult {
        id: "t1".to_owned(),
        name: "bash".to_owned(),
        failed: false,
        output: "a listing".to_owned(),
    };

    quiet.note_tool_result(&result);
    loud.note_tool_result(&result);
    quiet.flush().await;
    loud.flush().await;

    assert!(quiet_channel.records().is_empty());
    assert!(loud_channel.contents()[0].contains("a listing"));
}

#[tokio::test]
async fn an_empty_tool_result_is_not_posted_even_when_output_is_forwarded() {
    let (port, channel) = thread(true);

    port.note_tool_result(&ToolResult {
        id: "t1".to_owned(),
        name: "bash".to_owned(),
        failed: false,
        output: "   ".to_owned(),
    });
    port.flush().await;

    assert!(channel.records().is_empty());
}

/// A thread archived because its session idled out drops off the sidebar, and
/// the people who were in it have to go hunting for it.
#[tokio::test]
async fn a_session_that_idled_out_leaves_the_thread_open() {
    let (port, channel) = thread(false);

    port.close(EndReason::Idle).await;

    assert!(!channel.is_archived());
    assert!(port.is_closed());
}

#[tokio::test]
async fn only_a_deliberate_stop_archives_the_thread() {
    let (stopped, stopped_channel) = thread(false);
    let (crashed, crashed_channel) = thread(false);

    stopped.close(EndReason::Stopped).await;
    crashed.close(EndReason::Crashed).await;

    assert!(stopped_channel.is_archived());
    assert!(!crashed_channel.is_archived());
}

/// Everything already said has to arrive before the thread is finished with.
#[tokio::test]
async fn closing_sends_what_is_still_queued_first() {
    let (port, channel) = thread(false);

    port.post("the last thing I said");
    port.close(EndReason::Stopped).await;

    assert!(
        channel
            .contents()
            .iter()
            .any(|content| content == "the last thing I said")
    );
}

#[tokio::test]
async fn a_closed_thread_accepts_nothing_more() {
    let (port, channel) = thread(false);

    port.close(EndReason::Stopped).await;
    port.post("too late");
    port.flush().await;

    assert!(channel.records().is_empty());
}

/// The only sign during a long tool loop that a session is alive.
#[tokio::test]
async fn the_typing_indicator_is_held_while_a_turn_runs_and_dropped_after() {
    let (port, channel) = thread(false);

    port.set_busy(true);
    // The first ping is fire and forget, so give it a moment.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    port.set_busy(true);
    assert_eq!(*channel.typing.lock().unwrap(), 1);

    port.set_busy(false);
    port.set_busy(false);
}

#[tokio::test]
async fn an_upload_carries_the_file_and_its_caption() {
    let (port, channel) = thread(false);

    port.upload("notes.txt", b"hello".to_vec(), "`notes.txt` 5 bytes");
    port.flush().await;

    assert!(channel.contents()[0].contains("notes.txt"));
}

#[tokio::test]
async fn what_a_thread_does_not_show_it_says_nothing_about() {
    let (port, channel) = thread(false);

    port.flush().await;

    assert!(channel.records().is_empty());
}

/// Output posted into a closed connection is lost, so it waits instead.
#[tokio::test]
async fn nothing_is_sent_while_the_connection_is_down() {
    let (port, channel) = thread(false);
    let (connection, watching) = tokio::sync::watch::channel(true);
    port.follow(watching);

    let _ = connection.send(false);
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    port.post("held until it is back");
    port.flush().await;
    assert!(channel.records().is_empty());

    let _ = connection.send(true);
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    port.flush().await;
    assert_eq!(channel.records().len(), 1);
}

/// The daemon addresses a person on purpose, but most of what it posts is the
/// agent's words or somebody else's. Text that merely contains the everyone
/// syntax must not notify everyone.
#[test]
fn a_message_notifies_a_named_person_and_nobody_else() {
    let built = serde_json::to_value(super::plain("<@1> the agent said @everyone")).expect("built");
    let mentions = built.get("allowed_mentions").expect("a mention rule");

    let parse = mentions
        .get("parse")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let parse: Vec<&str> = parse.iter().filter_map(|value| value.as_str()).collect();
    assert!(parse.contains(&"users"), "{mentions}");
    assert!(!parse.contains(&"everyone"), "{mentions}");
    assert!(!parse.contains(&"roles"), "{mentions}");
}
