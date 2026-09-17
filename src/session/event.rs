//! What a session says, as one enum of events.
//!
//! Kept apart from the session itself so that a surface which renders a
//! session depends on this alone. A chat thread, a stored transcript, and a
//! browser are all readers of the same events with different ideas of what is
//! worth showing.
//!
//! The TypeScript port declared a wide `ThreadPort` interface and a separate
//! tagged union of the same calls for replay. Here both are one type: the
//! session sends events, a view matches exhaustively, and the replay buffer
//! stores them directly. The serialized shape is the transcript's, so what a
//! session holds in memory and what it writes down are the same thing.
//!
//! Every event is about what happened, not about how to draw it. Where a
//! surface needs the parts rather than a rendered line, both are carried, so
//! neither has to unpick the other's text.

use serde::{Deserialize, Serialize};

/// What a session has cost so far.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsage {
    /// Tokens sent, not counting cache reads.
    pub input: u64,
    /// Tokens the model produced.
    pub output: u64,
    /// Tokens served from the prompt cache.
    pub cache_read: u64,
    /// Tokens written into the prompt cache.
    pub cache_write: u64,
    /// Everything above, as the agent totals it.
    pub total_tokens: u64,
    /// What it has cost so far, in the provider's currency.
    pub cost: f64,
    /// Tokens in the most recent request, which is the context it carries.
    pub context_tokens: u64,
    /// How much context the model holds, when the agent has said.
    ///
    /// Without it `context_tokens` is a number with nothing to measure it
    /// against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Turns taken so far.
    pub turns: u64,
    /// The model that answered, when the agent named it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// What kind of thing the daemon is reporting about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoticeLevel {
    Started,
    Warning,
    Done,
    Ended,
}

impl NoticeLevel {
    /// The level as it is written down.
    #[allow(
        dead_code,
        reason = "read by this module's tests, which assert on state the daemon never asks for"
    )]
    pub fn as_str(self) -> &'static str {
        match self {
            NoticeLevel::Started => "started",
            NoticeLevel::Warning => "warning",
            NoticeLevel::Done => "done",
            NoticeLevel::Ended => "ended",
        }
    }
}

/// What a tool was doing, for a surface that renders it itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolActivity {
    /// The agent's own identifier for the call, so a result can find it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The tool's name.
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// What it acted on, when its arguments name one readably.
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Whether it failed, once it has finished.
    pub failed: Option<bool>,
}

/// What a tool produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    /// The agent's own identifier for the call this answers.
    pub id: String,
    /// The tool's name.
    pub name: String,
    /// Whether the call failed.
    pub failed: bool,
    /// Already truncated to the configured limit.
    pub output: String,
}

/// One delegated question and what came of it.
///
/// A session's model may ask a cheaper one about a single artefact. Carried as
/// its parts rather than as a rendered line, because a thread shows one line
/// and an interface shows the question, the answer, and what it saved.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delegated {
    /// What the session's model wanted to know.
    pub question: String,
    /// The model that answered, or none when none was asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// What it was shown, for attributing the answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub describes: Option<String>,
    /// What it said, absent when it was refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Why nothing was asked, absent when something was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    /// Tokens the delegated model was charged, when the provider said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
    /// Characters kept out of the session's own context by asking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept_out: Option<usize>,
}

/// The four states a sender's message can end in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReactionOutcome {
    Accepted,
    Succeeded,
    Failed,
    Interrupted,
}

impl ReactionOutcome {
    /// The outcome as it is written down.
    pub fn as_str(self) -> &'static str {
        match self {
            ReactionOutcome::Accepted => "accepted",
            ReactionOutcome::Succeeded => "succeeded",
            ReactionOutcome::Failed => "failed",
            ReactionOutcome::Interrupted => "interrupted",
        }
    }
}

/// Why a session ended, for the final message and the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EndReason {
    Stopped,
    /// Ended because the agent would not answer an interruption.
    ///
    /// Apart from a deliberate stop, because it is not one: the thread is left
    /// open and its record kept, so what was being worked on can be picked up
    /// again rather than being lost to a turn that would not let go.
    Unresponsive,
    Idle,
    Crashed,
    #[serde(rename = "resource limit")]
    ResourceLimit,
    #[serde(rename = "startup failed")]
    StartupFailed,
    Shutdown,
    #[serde(rename = "thread archived")]
    ThreadArchived,
    #[serde(rename = "protocol violation")]
    ProtocolViolation,
}

/// One thing a session says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "call",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SessionEvent {
    /// Posts a message, splitting it if needed.
    Post { text: String },
    /// Notes that somebody asked the agent for something.
    ///
    /// A chat thread already holds the message that started a turn, so it
    /// does nothing with this. Every other surface has no such copy, and
    /// without it would show the agent talking to itself.
    Prompt {
        author: String,
        text: String,
        /// The chat message it was said under, when it had one. A withdrawal
        /// carries only this id, so an entry without one can never be found
        /// again to withdraw.
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Set when the person who sent it took it back.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        withdrawn: bool,
    },
    /// Notes something said to the people in the thread, not to the agent.
    ///
    /// A thread already holds the message and nobody there can mistake it for
    /// the agent having been told. Anywhere else it has to be marked, or
    /// reading a session back would show the agent being told something it
    /// never heard.
    Aside {
        author: String,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        withdrawn: bool,
    },
    /// Reports something the daemon did, rather than something the agent said.
    ///
    /// A thread has one voice and shows these as ordinary messages. Anywhere
    /// else they are the frame around the conversation rather than part of it,
    /// so they are marked as what they are instead of arriving as the agent
    /// talking.
    Notice { text: String, level: NoticeLevel },
    /// Notes what the agent reasoned before answering.
    ///
    /// A thread does not show this: it is long, and a conversation is not the
    /// place for it. A surface that can fold it away shows it.
    Thinking { text: String },
    /// Answers a command.
    ///
    /// A reply belongs to the command that asked for it rather than to the
    /// conversation, so a surface can attach it to that instead of showing it
    /// as the agent having spoken.
    Reply { text: String, command: String },
    /// Reports what a tool produced, once it has finished.
    ///
    /// Separate from the call itself, so a surface can attach the result to it
    /// rather than showing the two as unrelated events.
    ToolResult { result: ToolResult },
    /// Adds a line of tool activity.
    ///
    /// The rendered line is what a thread shows. The parts are carried
    /// alongside it so another surface can lay them out itself rather than
    /// unpicking text.
    Activity {
        line: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool: Option<ToolActivity>,
    },
    /// Reports that a cheaper model was asked about something.
    ///
    /// Shown wherever the conversation is read, so a delegated answer can
    /// never be mistaken for the session model's own words.
    Delegation { delegated: Delegated },
    /// Reports what an edit changed.
    ///
    /// Carried as its parts rather than as rendered text, so a thread can
    /// render a fenced block and an interface a real diff, from the same
    /// report.
    Diff {
        path: String,
        added: u64,
        removed: u64,
        body: String,
        /// The tool call that made the change, absent in an older recording.
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<String>,
    },
    /// What an upload is kept as once it has been sent.
    ///
    /// By name and size, never by contents: holding the bytes would mean a
    /// long session pinning every file it ever sent in memory.
    Attachment { name: String, size: u64 },
    /// Uploads a file so it can be read or downloaded.
    Upload {
        name: String,
        bytes: Vec<u8>,
        caption: String,
    },
    /// Reports what the session has cost so far.
    ///
    /// State rather than history: only the current total is meaningful, so a
    /// surface shows the latest instead of every step towards it.
    Usage { usage: SessionUsage },
    /// Creates or updates the single message reporting queue position.
    Waiting { text: Option<String> },
    /// Sets the outcome reaction on a message, replacing any earlier one.
    Reaction {
        message_id: String,
        outcome: ReactionOutcome,
    },
    /// Marks the thread as having work in progress, or as having none.
    Busy { busy: bool },
    /// Opens a turn, which everything reported after it belongs to.
    ///
    /// A thread shows a conversation in order and has no use for the
    /// boundary. A surface that groups what it shows needs to be told where
    /// one is.
    BeginTurn { turn: u32 },
    /// Reports that the session has ended, and why.
    ///
    /// The reason decides what a chat thread does with itself. Somebody typing
    /// `!stop` is finished with it, so it is archived. Anything else, an idle
    /// timeout or a crash or a restart, leaves it open: those sessions can be
    /// resumed, and a thread archived out of the sidebar is one its own author
    /// has to go hunting for.
    Close { reason: EndReason },
}

#[cfg(test)]
#[path = "event/tests.rs"]
mod tests;
