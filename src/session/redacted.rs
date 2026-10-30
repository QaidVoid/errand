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
//! It replaces a configured value verbatim, and a secret minted for the
//! session by its shape. An agent that splits or re-encodes either defeats
//! it, which is why this is damage control on an unavoidable exposure
//! rather than a boundary.

use std::sync::Arc;

use crate::config::redact::{contains_secret, redact_text};
use crate::session::event::{Delegated, SessionEvent, ToolActivity, ToolResult};
use crate::session::views::ViewFanOut;

/// Scrubs secret values out of an event, in every field the agent can
/// influence.
///
/// A message id is not one of them: it is minted by the chat service and never
/// carries agent output, and neither does a turn number, a reaction, a usage
/// total, nor the author a message is attributed to.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per event keeps what is scrubbed on each readable"
)]
pub fn redact_event(event: SessionEvent, secrets: &[String]) -> SessionEvent {
    let clean = |text: &str| redact_text(text, secrets);
    let maybe = |text: &Option<String>| text.as_deref().map(clean);

    match event {
        SessionEvent::Post { text } => SessionEvent::Post { text: clean(&text) },
        SessionEvent::Prompt {
            author,
            text,
            id,
            withdrawn,
            from_interface,
        } => SessionEvent::Prompt {
            author,
            text: clean(&text),
            id,
            withdrawn,
            from_interface,
        },
        SessionEvent::Aside {
            author,
            text,
            id,
            withdrawn,
            from_interface,
        } => SessionEvent::Aside {
            author,
            text: clean(&text),
            id,
            withdrawn,
            from_interface,
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
        SessionEvent::Question {
            text,
            title,
            message,
            choices,
        } => SessionEvent::Question {
            text: clean(&text),
            title: clean(&title),
            message: maybe(&message),
            choices: choices.iter().map(|choice| clean(choice)).collect(),
        },
        // Reactions, usage, and the frame around the conversation carry
        // nothing the agent said. An attachment is made from an upload that
        // was scrubbed here already.
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
    secrets: std::sync::RwLock<Vec<String>>,
}

impl Redacting {
    /// Wraps a fan-out with the secrets of the day.
    pub fn new(fan: Arc<ViewFanOut>, secrets: Vec<String>) -> Self {
        Self {
            fan,
            secrets: std::sync::RwLock::new(secrets),
        }
    }

    /// Scrubs `secret` too from here on, for one the session came to hold
    /// after it started, such as a minted token.
    pub fn add_secret(&self, secret: String) {
        let mut secrets = self.secrets.write().expect("the secrets lock");
        if !secrets.contains(&secret) {
            secrets.push(secret);
        }
    }

    /// What this door scrubs now, for a surface that shows the session's
    /// writing other than through its reports.
    pub fn secrets(&self) -> Vec<String> {
        self.secrets.read().expect("the secrets lock").clone()
    }

    /// Stops scrubbing `secret`, for one that is worth nothing any more.
    pub fn remove_secret(&self, secret: &str) {
        self.secrets
            .write()
            .expect("the secrets lock")
            .retain(|held| held != secret);
    }

    /// Whether `bytes` hold one of the secrets this door scrubs, for what it
    /// cannot scrub, such as an upload.
    pub fn contains_secret(&self, bytes: &[u8]) -> bool {
        contains_secret(bytes, &self.secrets.read().expect("the secrets lock"))
    }

    /// Delivers an event to every attached view, with secrets scrubbed first.
    pub async fn send(&self, event: SessionEvent) {
        let redacted = redact_event(event, &self.secrets.read().expect("the secrets lock"));
        self.fan.send(redacted).await;
    }
}

#[cfg(test)]
mod tests;
