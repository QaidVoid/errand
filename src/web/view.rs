//! A connected browser, as a view of a session.
//!
//! Implements the same view trait a chat thread does, so the session layer
//! does not know or care which it is talking to. Everything it is told becomes
//! an event on one server-sent stream.
//!
//! Nothing here blocks: a browser that has stopped reading must not hold up
//! the session or the other views, so a full queue drops rather than waits.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::session::event::SessionEvent;
use crate::session::views::{SessionView, ViewError};

/// How much is held for a browser that has stopped reading before dropping.
const MAX_QUEUED: usize = 1_000;

/// Looks up who an account id belongs to.
pub type NameLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Rewrites chat mentions for a reader who is not in the chat service.
///
/// A mention is markup that the service turns into a name. Here it would show
/// as `<@1234> [done]`, so a known id becomes the name and an unknown one is
/// dropped rather than shown as a number nobody can place.
pub fn without_mentions(text: &str, names: Option<&NameLookup>) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut at = 0;
    while let Some(open) = text[at..].find("<@") {
        let start = at + open;
        let body_start = start + 2;
        let body_start = if bytes.get(body_start) == Some(&b'!') {
            body_start + 1
        } else {
            body_start
        };
        let Some(close) = text[body_start..].find('>') else {
            break;
        };
        let id = &text[body_start..body_start + close];
        out.push_str(&text[at..start]);
        let known = id
            .parse::<u64>()
            .ok()
            .and_then(|numeric| names.and_then(|names| (names)(&numeric.to_string())));
        if let Some(name) = known {
            out.push('@');
            out.push_str(&name);
            out.push(' ');
        }
        at = body_start + close + 1;
        // The space after a mention is markup spacing, not prose.
        while bytes.get(at) == Some(&b' ') {
            at += 1;
        }
    }
    out.push_str(&text[at..]);
    out.trim().to_owned()
}

/// A browser attached to one session.
///
/// The receiver end of the channel is the response body; everything the
/// session reports is serialised and queued for it.
pub struct WebView {
    names: Option<NameLookup>,
    sender: tokio::sync::mpsc::Sender<String>,
    turn: Mutex<Option<u32>>,
}

impl WebView {
    /// Creates a view and the stream a browser reads.
    ///
    /// The first event tells the interface to clear what it has, because
    /// everything after it is the session's record from the beginning. A
    /// reconnecting browser therefore ends up correct rather than doubled.
    pub fn new(names: Option<NameLookup>) -> (Arc<Self>, tokio::sync::mpsc::Receiver<String>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(MAX_QUEUED);
        let view = Arc::new(Self {
            names,
            sender,
            turn: Mutex::new(None),
        });
        view.push("reset", &serde_json::json!({}));
        (Arc::clone(&view), receiver)
    }

    /// True once the browser has gone, so the view can be detached.
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    /// Queues one server-sent event, dropping it when nobody is keeping up.
    fn push(&self, event: &str, payload: &Value) {
        let _ = self
            .sender
            .try_send(format!("event: {event}\ndata: {payload}\n\n"));
    }

    fn entry(&self, mut entry: Value) {
        if let Some(turn) = *self.turn.lock().expect("the view turn lock") {
            entry["turn"] = Value::from(turn);
        }
        self.push("entry", &entry);
    }

    #[allow(clippy::needless_pass_by_value)]
    fn state(&self, state: Value) {
        self.push("state", &state);
    }

    fn clean(&self, text: &str) -> String {
        without_mentions(text, self.names.as_ref())
    }
}

impl SessionView for WebView {
    fn observe<'a>(
        &'a self,
        event: &'a SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), ViewError>> + Send + 'a>> {
        Box::pin(async move {
            let at = crate::log::now_ms();
            match event {
                SessionEvent::BeginTurn { turn } => {
                    *self.turn.lock().expect("the view turn lock") = Some(*turn);
                }
                SessionEvent::Post { text, .. } => self.entry(serde_json::json!({
                    "kind": "message",
                    "text": self.clean(text),
                    "at": at,
                })),
                SessionEvent::Prompt { author, text, .. } => self.entry(serde_json::json!({
                    "kind": "prompt",
                    "author": author,
                    "text": self.clean(text),
                    "at": at,
                })),
                SessionEvent::Aside { author, text, .. } => self.entry(serde_json::json!({
                    "kind": "aside",
                    "author": author,
                    "text": self.clean(text),
                    "at": at,
                })),
                SessionEvent::Notice { text, level } => self.entry(serde_json::json!({
                    "kind": "notice",
                    "text": self.clean(text),
                    "level": level,
                    "at": at,
                })),
                SessionEvent::Thinking { text, .. } => self.entry(serde_json::json!({
                    "kind": "thinking",
                    "text": text,
                    "at": at,
                })),
                // A command's answer goes to the thread it was asked in,
                // not here: the interface shows what the agent did.
                // Reactions are a chat affordance with no counterpart here:
                // the interface shows a message's fate through the state.
                SessionEvent::Reply { .. } | SessionEvent::Reaction { .. } => {}
                SessionEvent::ToolResult { result } => self.entry(serde_json::json!({
                    "kind": "toolResult",
                    "result": result,
                    "at": at,
                })),
                SessionEvent::Activity { line, tool } => self.entry(serde_json::json!({
                    "kind": "activity",
                    "line": line,
                    "tool": tool,
                    "at": at,
                })),
                SessionEvent::Delegation { delegated } => self.entry(serde_json::json!({
                    "kind": "delegation",
                    "delegated": delegated,
                    "at": at,
                })),
                SessionEvent::Diff {
                    path,
                    added,
                    removed,
                    body,
                    cause,
                    ..
                } => self.entry(serde_json::json!({
                    "kind": "diff",
                    "path": path,
                    "added": added,
                    "removed": removed,
                    "body": body,
                    "cause": cause,
                    "at": at,
                })),
                SessionEvent::Attachment { name, size } => self.entry(serde_json::json!({
                    "kind": "file",
                    "name": name,
                    "size": size,
                    "at": at,
                })),
                SessionEvent::Upload { name, bytes, .. } => self.entry(serde_json::json!({
                    "kind": "file",
                    "name": name,
                    "size": bytes.len(),
                    "at": at,
                })),
                SessionEvent::Usage { usage } => {
                    self.state(serde_json::json!({ "usage": usage }));
                }
                SessionEvent::Waiting { text } => {
                    self.state(serde_json::json!({ "waiting": text }));
                }
                SessionEvent::Busy { busy } => {
                    self.state(serde_json::json!({ "busy": busy }));
                }
                // The stream is left open: a stopped session can be picked up
                // again, and a browser watching one should see that happen
                // rather than be disconnected.
                SessionEvent::Close { .. } => {
                    self.state(serde_json::json!({ "ended": true, "busy": false }));
                }
            }
            Ok(())
        })
    }
}

/// One recorded thing, as the interface reads it back from a transcript.
///
/// The same shape a live stream carries, so a browser renders either the same
/// way. Usage never becomes an entry: it is state, and the transcript caller
/// reports the latest total separately.
pub fn wire(entry: &SessionEvent, at: i64, turn: Option<u32>, names: Option<&NameLookup>) -> Value {
    let clean = |text: &str| without_mentions(text, names);
    let mut shown = match entry {
        SessionEvent::Post { text, .. } => serde_json::json!({
            "kind": "message",
            "text": clean(text),
            "at": at,
        }),
        SessionEvent::Prompt { author, text, .. } => serde_json::json!({
            "kind": "prompt",
            "author": author,
            "text": clean(text),
            "at": at,
        }),
        SessionEvent::Aside { author, text, .. } => serde_json::json!({
            "kind": "aside",
            "author": author,
            "text": clean(text),
            "at": at,
        }),
        SessionEvent::Notice { text, level } => serde_json::json!({
            "kind": "notice",
            "text": clean(text),
            "level": level,
            "at": at,
        }),
        SessionEvent::Thinking { text, .. } => serde_json::json!({
            "kind": "thinking",
            "text": text,
            "at": at,
        }),
        SessionEvent::Reply { text, command, .. } => serde_json::json!({
            "kind": "reply",
            "text": clean(text),
            "command": command,
            "at": at,
        }),
        SessionEvent::ToolResult { result } => serde_json::json!({
            "kind": "toolResult",
            "result": result,
            "at": at,
        }),
        SessionEvent::Activity { line, tool } => serde_json::json!({
            "kind": "activity",
            "line": line,
            "tool": tool,
            "at": at,
        }),
        SessionEvent::Delegation { delegated } => serde_json::json!({
            "kind": "delegation",
            "delegated": delegated,
            "at": at,
        }),
        SessionEvent::Diff {
            path,
            added,
            removed,
            body,
            cause,
            ..
        } => serde_json::json!({
            "kind": "diff",
            "path": path,
            "added": added,
            "removed": removed,
            "body": body,
            "cause": cause,
            "at": at,
        }),
        SessionEvent::Attachment { name, size } => serde_json::json!({
            "kind": "file",
            "name": name,
            "size": size,
            "at": at,
        }),
        // Never recorded, so never read back; the caller filters these out.
        SessionEvent::Upload { .. }
        | SessionEvent::Usage { .. }
        | SessionEvent::Waiting { .. }
        | SessionEvent::Reaction { .. }
        | SessionEvent::Busy { .. }
        | SessionEvent::BeginTurn { .. }
        | SessionEvent::Close { .. } => Value::Null,
    };
    if let Some(turn) = turn {
        shown["turn"] = Value::from(turn);
    }
    shown
}

#[cfg(test)]
#[path = "view/tests.rs"]
mod tests;
