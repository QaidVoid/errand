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

/// The `command` of a [`SessionEvent::Reply`] that refuses somebody rather
/// than answering a command they ran.
pub const REFUSAL: &str = "refused";

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

/// The state a card stands for, which a surface turns into a color.
///
/// Named by what the session means rather than by any palette, so every
/// surface colors the same state the same way without sharing swatches. A
/// card is a post that deserves reading at a glance: green when things are
/// fine, blue while working, amber for a warning, red for a refusal, gray
/// for something that has settled and asks for nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CardColor {
    Ready,
    Working,
    Warning,
    Refused,
    Settled,
}

/// One labeled cell of a card's grid.
///
/// A surface that lays fields out side by side turns a list of these into a
/// table the service renders itself; a surface without them reads each as a
/// line of the card's words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardField {
    /// The label above the value.
    pub name: String,
    /// The text under the label, in the markdown the surface renders.
    pub value: String,
    /// Whether the cell may share a row with its neighbours, for short
    /// figures read together rather than distinct things listed one a row.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inline: bool,
}

/// A post a surface may draw as a card instead of plain text.
///
/// The text always rides alongside, so a surface without cards shows the
/// same words and a surface with them lays the same words out under a title
/// and a colored edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    /// The state the card reports.
    pub color: CardColor,
    /// The card's heading.
    pub title: String,
    /// The body, in the markdown the surface renders.
    pub description: String,
    /// Labeled cells laid out in a grid, when the card is a table of things.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<CardField>,
    /// A quiet line under the body, when there is one worth saying.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footer: Option<String>,
    /// The pages of a card that holds more than one card's worth, in order.
    /// Empty for an ordinary card, whose body and cells are its only page.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<CardPage>,
}

/// The most body and cells a page holds before the next group starts a page
/// of its own, in the UTF-16 units the service counts in.
pub const PAGE_BUDGET: usize = 1_500;

/// The most cells a page holds before the next group starts a page of its
/// own. The service takes 25, but more than ten stacked cells is a long scroll.
pub const FIELDS_PER_PAGE: usize = 10;

/// Lays `groups` onto pages in order. A group joins the page before it while
/// both fit [`PAGE_BUDGET`] and [`FIELDS_PER_PAGE`], so small groups share a
/// page and no group is split. A page that holds several groups is labeled
/// by its first and last.
pub fn pack_pages(groups: Vec<CardPage>) -> Vec<CardPage> {
    let size = |page: &CardPage| {
        page.description.encode_utf16().count()
            + page
                .fields
                .iter()
                .map(|field| field.name.encode_utf16().count() + field.value.encode_utf16().count())
                .sum::<usize>()
    };
    let mut pages: Vec<(CardPage, String, usize)> = Vec::new();
    for group in groups {
        if let Some((page, first, joined)) = pages.last_mut()
            && size(page) + size(&group) <= PAGE_BUDGET
            && page.fields.len() + group.fields.len() <= FIELDS_PER_PAGE
        {
            *joined += 1;
            page.label = if *joined == 2 {
                format!("{first}, {}", group.label)
            } else {
                format!("{first} to {}", group.label)
            };
            if !group.description.is_empty() {
                if !page.description.is_empty() {
                    page.description.push_str("\n\n");
                }
                page.description.push_str(&group.description);
            }
            page.fields.extend(group.fields);
            continue;
        }
        let first = group.label.clone();
        pages.push((group, first, 1));
    }
    pages.into_iter().map(|(page, _, _)| page).collect()
}

/// One page of a card that holds more than one card's worth.
///
/// A surface that can turn pages shows one at a time under the card's title;
/// one that cannot shows them all, one after another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardPage {
    /// What the page holds, such as the group it lists, shown with its number.
    pub label: String,
    /// The page's body.
    pub description: String,
    /// The page's cells.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<CardField>,
}

impl Card {
    /// A card of `pages`, one at a time where the surface can turn them.
    ///
    /// One page is an ordinary card, with nothing to turn.
    pub fn paged(
        color: CardColor,
        title: String,
        mut pages: Vec<CardPage>,
        footer: Option<String>,
    ) -> Self {
        let single = (pages.len() == 1).then(|| pages.remove(0));
        let (description, fields) = single.map_or_else(
            || (String::new(), Vec::new()),
            |page| (page.description, page.fields),
        );
        Self {
            color,
            title,
            description,
            fields,
            footer,
            pages,
        }
    }

    /// How many pages the card has. An ordinary card has one.
    pub fn page_count(&self) -> usize {
        self.pages.len().max(1)
    }

    /// The card as page `index` alone shows it: that page's body and cells,
    /// with its number and label under the card's own footer. An index past
    /// the last shows the last.
    pub fn page(&self, index: usize) -> Self {
        let Some(page) = self
            .pages
            .get(index.min(self.pages.len().saturating_sub(1)))
        else {
            return self.clone();
        };
        let number = index.min(self.pages.len() - 1) + 1;
        let position = if page.label.is_empty() {
            format!("page {number} of {}", self.pages.len())
        } else {
            format!("page {number} of {}: {}", self.pages.len(), page.label)
        };
        Self {
            color: self.color,
            title: self.title.clone(),
            description: page.description.clone(),
            fields: page.fields.clone(),
            footer: Some(match &self.footer {
                Some(footer) => format!("{footer}\n{position}"),
                None => position,
            }),
            pages: Vec::new(),
        }
    }

    /// The words of the card, which a surface without cards shows: every
    /// page, one after another, under its label.
    pub fn words(&self) -> String {
        use std::fmt::Write as _;

        let mut said = self.title.clone();
        let mut body = |description: &str, fields: &[CardField]| {
            if !description.is_empty() {
                said.push('\n');
                said.push_str(description);
            }
            for field in fields {
                let _ = write!(said, "\n{}: {}", field.name, field.value);
            }
        };
        if self.pages.is_empty() {
            body(&self.description, &self.fields);
        } else {
            for page in &self.pages {
                body(&format!("\n{}", page.label), &[]);
                body(&page.description, &page.fields);
            }
        }
        if let Some(footer) = &self.footer {
            said.push('\n');
            said.push_str(footer);
        }
        said
    }
}

/// What the daemon answers by itself: words, or words worth a card.
///
/// Most answers are words; a listing of things, which is the same words every
/// time, is a card. A surface that has no cards takes [`Answer::words`] and
/// shows what it would have shown anyway.
#[derive(Debug, Clone)]
pub enum Answer {
    /// Plain words.
    Text(String),
    /// Words laid out as a card.
    Card(Card),
}

impl From<String> for Answer {
    fn from(text: String) -> Self {
        Answer::Text(text)
    }
}

impl Answer {
    /// The words of the answer, whatever shape it came in.
    pub fn words(self) -> String {
        match self {
            Answer::Text(text) => text,
            Answer::Card(card) => card.words(),
        }
    }
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
    ///
    /// A card, when present, is the same words drawn as a card by a surface
    /// that has them. It is never split: a post carrying one fits in a card,
    /// or it does not carry one.
    Post {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        card: Option<Card>,
    },
    /// Notes that somebody asked the agent for something.
    ///
    /// A chat thread already holds the message that started a turn, so it
    /// does nothing with this unless it was said in the interface. Every
    /// other surface has no such copy, and without it would show the agent
    /// talking to itself.
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
        /// Set when it was said in the interface, so the thread has to be
        /// shown it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        from_interface: bool,
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
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        from_interface: bool,
    },
    /// Reports something the daemon did, rather than something the agent said.
    ///
    /// A thread has one voice and shows these as ordinary messages. Anywhere
    /// else they are the frame around the conversation rather than part of it,
    /// so they are marked as what they are instead of arriving as the agent
    /// talking.
    ///
    /// A card, when present, is the same report laid out for a surface that
    /// draws cards. Without one, such a surface draws the text.
    Notice {
        text: String,
        level: NoticeLevel,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        card: Option<Card>,
    },
    /// Notes what the agent reasoned before answering.
    ///
    /// A thread does not show this: it is long, and a conversation is not the
    /// place for it. A surface that can fold it away shows it.
    Thinking { text: String },
    /// Asks something the agent is waiting on an answer to.
    ///
    /// Carried as the text a thread posts and as its parts, so a surface with
    /// buttons can offer the choices rather than ask for a typed reply. A
    /// choice answers the question when it is sent back as it is written.
    Question {
        text: String,
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default)]
        choices: Vec<String>,
    },
    /// Answers a command.
    ///
    /// A reply belongs to the command that asked for it rather than to the
    /// conversation, so a surface can attach it to that instead of showing it
    /// as the agent having spoken. `command` is the command as it was asked,
    /// or [`REFUSAL`] for a refusal, which is never recorded. A card, when
    /// present, is the same words drawn as a card by a surface that has them.
    Reply {
        text: String,
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        card: Option<Card>,
    },
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
    /// long session pinning every file it ever sent in memory. The bytes are
    /// kept on disk beside the record instead, under `file`, so a surface
    /// that can show a file asks for it there.
    Attachment {
        name: String,
        size: u64,
        /// The name it was kept under beside the record, absent in an older
        /// recording or when it could not be kept.
        #[serde(skip_serializing_if = "Option::is_none")]
        file: Option<String>,
        /// What it was posted with, when it was posted with anything.
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
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
mod tests;
