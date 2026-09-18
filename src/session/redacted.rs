//! Scrubs the provider credential out of everything a session reports.
//!
//! The agent authenticates with a key held in its environment, so the key is
//! reachable by the agent by necessity. Anything the agent then prints is
//! forwarded: a tool that dumps the environment, a library that puts a request
//! header in an error, a config file it decides to read out. That output goes
//! to a chat channel, to the web interface, and to the transcript on disk, and
//! a credential posted into a channel has left the machine for good.
//!
//! Scrubbing here, at the one door a session reports through rather than at
//! each view, is what makes this hold: every surface is behind it, including
//! the transcript, so there is no path that reports something the others
//! scrubbed.
//!
//! It replaces the value verbatim. An agent that splits or re-encodes the key
//! defeats it, which is why this is damage control on an unavoidable exposure
//! rather than a boundary.

use std::sync::Arc;

use crate::config::redact::redact_text;
use crate::session::event::{Delegated, SessionEvent, ToolActivity, ToolResult};
use crate::session::views::ViewFanOut;

/// Scrubs secret values out of an event, in every field the agent can
/// influence.
///
/// A message id is not one of them: it is minted by the chat service and never
/// carries agent output, and neither does a turn number, a reaction, a usage
/// total, nor the author a message is attributed to.
pub fn redact_event(event: SessionEvent, secrets: &[String]) -> SessionEvent {
    if secrets.is_empty() {
        return event;
    }
    let clean = |text: &str| redact_text(text, secrets);
    let maybe = |text: &Option<String>| text.as_deref().map(clean);

    match event {
        SessionEvent::Post { text } => SessionEvent::Post { text: clean(&text) },
        SessionEvent::Prompt {
            author,
            text,
            id,
            withdrawn,
        } => SessionEvent::Prompt {
            author,
            text: clean(&text),
            id,
            withdrawn,
        },
        SessionEvent::Aside {
            author,
            text,
            id,
            withdrawn,
        } => SessionEvent::Aside {
            author,
            text: clean(&text),
            id,
            withdrawn,
        },
        SessionEvent::Notice { text, level } => SessionEvent::Notice {
            text: clean(&text),
            level,
        },
        SessionEvent::Thinking { text } => SessionEvent::Thinking { text: clean(&text) },
        SessionEvent::Reply { text, command } => SessionEvent::Reply {
            text: clean(&text),
            command: clean(&command),
        },
        SessionEvent::ToolResult { result } => SessionEvent::ToolResult {
            result: ToolResult {
                output: clean(&result.output),
                ..result
            },
        },
        SessionEvent::Delegation { delegated } => SessionEvent::Delegation {
            delegated: Delegated {
                question: clean(&delegated.question),
                answer: maybe(&delegated.answer),
                refused: maybe(&delegated.refused),
                ..delegated
            },
        },
        SessionEvent::Activity { line, tool } => SessionEvent::Activity {
            line: clean(&line),
            tool: tool.map(|tool| ToolActivity {
                target: maybe(&tool.target),
                ..tool
            }),
        },
        SessionEvent::Diff {
            path,
            added,
            removed,
            body,
            cause,
        } => SessionEvent::Diff {
            path: clean(&path),
            added,
            removed,
            body: clean(&body),
            cause: maybe(&cause),
        },
        SessionEvent::Upload {
            name,
            bytes,
            caption,
        } => SessionEvent::Upload {
            name: clean(&name),
            bytes,
            caption: clean(&caption),
        },
        SessionEvent::Waiting { text } => SessionEvent::Waiting { text: maybe(&text) },
        // Reactions, usage, and the frame around the conversation carry
        // nothing the agent said. An attachment is recorded, never delivered.
        SessionEvent::Attachment { .. }
        | SessionEvent::Reaction { .. }
        | SessionEvent::Usage { .. }
        | SessionEvent::Busy { .. }
        | SessionEvent::BeginTurn { .. }
        | SessionEvent::Close { .. } => event,
    }
}

/// The one door a session reports through, with secrets scrubbed at it.
///
/// A session holds one of these instead of the fan-out itself, so no surface
/// behind the fan-out can be reached by an unscrubbed report.
pub struct Redacting {
    fan: Arc<ViewFanOut>,
    secrets: Vec<String>,
}

impl Redacting {
    /// Wraps a fan-out with the secrets of the day.
    pub fn new(fan: Arc<ViewFanOut>, secrets: Vec<String>) -> Self {
        Self { fan, secrets }
    }

    /// Delivers an event to every attached view, with secrets scrubbed first.
    pub async fn send(&self, event: SessionEvent) {
        self.fan.send(redact_event(event, &self.secrets)).await;
    }
}

#[cfg(test)]
mod tests;
