//! Thread creation, output, and the one case where a thread is closed.
//!
//! The only place that turns session events into calls against the chat
//! service, which is what keeps the session layer free of chat types and
//! testable without a connection.
//!
//! No message gets a link preview: a coding agent quotes URLs constantly, and
//! a preview card for each one buries the conversation. The production
//! transport is the only thing that builds a message, and it always sets the
//! suppress-embeds flag, so a new call site cannot reintroduce embeds by
//! forgetting it.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serenity::builder::{
    CreateAllowedMentions, CreateAttachment, CreateMessage, EditMessage, EditThread,
};
use serenity::model::channel::Channel;
use serenity::model::channel::ChannelType;
use serenity::model::channel::MessageFlags;
use serenity::model::channel::ReactionType;
use serenity::model::id::{ChannelId, MessageId};
use tokio::sync::watch;

use crate::chat::chars::reaction;
use crate::chat::diff::FileDiff;
use crate::chat::diff::render_diff;
use crate::chat::outbox::{DEFAULT_MAX_BUFFERED, Outbox, TaskError};
use crate::chat::render::{MESSAGE_LIMIT, delegation_line, split_message};
use crate::log::{LogValue, Logger, fields};
use crate::session::event::Delegated;
use crate::session::event::ToolResult;
use crate::session::event::{EndReason, ReactionOutcome, SessionEvent};
use crate::session::views::SessionView;
use crate::session::views::ViewError;

/// The service expires a typing indicator after about ten seconds.
pub const TYPING_REFRESH_MS: u64 = 8_000;

/// Everything one send or edit leaves behind, for later edits and reactions.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageHandle {
    /// The service's own id for the message, which an edit needs.
    pub id: String,
}

/// Why a call to the chat service failed.
pub type ServiceError = Box<dyn std::error::Error + Send + Sync>;

/// A call to the chat service, as a boxed future.
pub type ServiceCall<T> = Pin<Box<dyn Future<Output = Result<T, ServiceError>> + Send>>;

/// What a thread does against the chat service.
///
/// One production implementation talks to serenity's REST client; a test
/// hands the thread a recording double, which is the seam the original's
/// fake channel sat in.
pub trait ThreadTransport: Send + Sync + 'static {
    /// Sends a message, optionally carrying a named file. Returns its id.
    fn send(
        &self,
        content: String,
        attachment: Option<(String, Vec<u8>)>,
    ) -> ServiceCall<MessageHandle>;

    /// Edits a message's text.
    fn edit(&self, message_id: String, content: String) -> ServiceCall<()>;

    /// Deletes a message.
    fn delete(&self, message_id: String) -> ServiceCall<()>;

    /// Reports whether a message still exists.
    fn fetch(&self, message_id: String) -> ServiceCall<Option<MessageHandle>>;

    /// Adds a reaction named by its glyph.
    fn react(&self, message_id: String, glyph: String) -> ServiceCall<()>;

    /// Removes one reaction, the daemon's own, from a message.
    fn remove_reaction(&self, message_id: String, glyph: String) -> ServiceCall<()>;

    /// Archives or reopens the thread.
    fn set_archived(&self, archived: bool) -> ServiceCall<()>;

    /// Shows the typing indicator once; holding it is the caller's job.
    fn send_typing(&self) -> ServiceCall<()>;
}

/// Who a message the daemon posts may notify.
///
/// The daemon addresses a person on purpose, to say a turn they asked for has
/// settled, so user mentions are parsed. Nothing it posts ever means to reach
/// everyone or a role, and most of what it posts is the agent's words or a
/// person's own: without this, text that merely happens to contain the
/// everyone or a role syntax would notify them.
fn addressed_to_people() -> CreateAllowedMentions {
    CreateAllowedMentions::new()
        .all_users(true)
        .all_roles(false)
        .everyone(false)
}

/// The message options for the production transport.
///
/// The one way a message is built, so neither the suppress-embeds flag nor
/// the mention rule can be lost at a new call site.
pub fn plain(content: &str) -> CreateMessage {
    CreateMessage::new()
        .content(content.to_owned())
        .flags(MessageFlags::SUPPRESS_EMBEDS)
        .allowed_mentions(addressed_to_people())
}

/// Parses one message id and one reaction glyph, or fails trying.
fn prepare_reaction(
    message_id: &str,
    glyph: String,
) -> Result<(u64, ReactionType), std::num::ParseIntError> {
    let id = message_id.parse::<u64>()?;
    let glyph = ReactionType::try_from(glyph.as_str()).unwrap_or(ReactionType::Unicode(glyph));
    Ok((id, glyph))
}

/// A call that failed before it could be made.
fn parse_failure(error: std::num::ParseIntError) -> ServiceCall<()> {
    Box::pin(async move { Err(Box::new(error) as ServiceError) })
}

/// The production transport, over serenity's REST client.
#[derive(Clone)]
pub struct SerenityThread {
    thread_id: ChannelId,
    http: Arc<serenity::http::Http>,
}

impl SerenityThread {
    /// A transport for one thread channel.
    pub fn new(thread_id: ChannelId, http: Arc<serenity::http::Http>) -> Self {
        Self { thread_id, http }
    }
}

impl ThreadTransport for SerenityThread {
    fn send(
        &self,
        content: String,
        attachment: Option<(String, Vec<u8>)>,
    ) -> ServiceCall<MessageHandle> {
        let thread_id = self.thread_id;
        let http = Arc::clone(&self.http);
        Box::pin(async move {
            let mut message = plain(&content);
            if let Some((name, bytes)) = attachment {
                message = message.files(vec![CreateAttachment::bytes(bytes, name)]);
            }
            let sent = thread_id.send_message(&http, message).await?;
            Ok(MessageHandle {
                id: sent.id.get().to_string(),
            })
        })
    }

    fn edit(&self, message_id: String, content: String) -> ServiceCall<()> {
        let thread_id = self.thread_id;
        let http = Arc::clone(&self.http);
        let id = match message_id.parse::<u64>() {
            Ok(id) => id,
            Err(error) => return parse_failure(error),
        };
        Box::pin(async move {
            let edited = EditMessage::new()
                .content(content)
                .flags(MessageFlags::SUPPRESS_EMBEDS)
                .allowed_mentions(addressed_to_people());
            thread_id
                .edit_message(&http, MessageId::new(id), edited)
                .await?;
            Ok(())
        })
    }

    fn delete(&self, message_id: String) -> ServiceCall<()> {
        let thread_id = self.thread_id;
        let http = Arc::clone(&self.http);
        let id = match message_id.parse::<u64>() {
            Ok(id) => id,
            Err(error) => return parse_failure(error),
        };
        Box::pin(async move {
            thread_id.delete_message(&http, MessageId::new(id)).await?;
            Ok(())
        })
    }

    fn fetch(&self, message_id: String) -> ServiceCall<Option<MessageHandle>> {
        let thread_id = self.thread_id;
        let http = Arc::clone(&self.http);
        let Ok(id) = message_id.parse::<u64>() else {
            return Box::pin(async { Ok(None) });
        };
        Box::pin(async move {
            match thread_id.message(&http, MessageId::new(id)).await {
                Ok(found) => Ok(Some(MessageHandle {
                    id: found.id.get().to_string(),
                })),
                Err(_) => Ok(None),
            }
        })
    }

    fn react(&self, message_id: String, glyph: String) -> ServiceCall<()> {
        let thread_id = self.thread_id;
        let http = Arc::clone(&self.http);
        let (id, glyph) = match prepare_reaction(&message_id, glyph) {
            Ok(pair) => pair,
            Err(error) => return parse_failure(error),
        };
        Box::pin(async move {
            thread_id
                .create_reaction(&http, MessageId::new(id), glyph)
                .await?;
            Ok(())
        })
    }

    fn remove_reaction(&self, message_id: String, glyph: String) -> ServiceCall<()> {
        let thread_id = self.thread_id;
        let http = Arc::clone(&self.http);
        let (id, glyph) = match prepare_reaction(&message_id, glyph) {
            Ok(pair) => pair,
            Err(error) => return parse_failure(error),
        };
        Box::pin(async move {
            thread_id
                .delete_reaction(&http, MessageId::new(id), None, glyph)
                .await?;
            Ok(())
        })
    }

    fn set_archived(&self, archived: bool) -> ServiceCall<()> {
        let thread_id = self.thread_id;
        let http = Arc::clone(&self.http);
        Box::pin(async move {
            thread_id
                .edit_thread(&http, EditThread::new().archived(archived))
                .await?;
            Ok(())
        })
    }

    fn send_typing(&self) -> ServiceCall<()> {
        let thread_id = self.thread_id;
        let http = Arc::clone(&self.http);
        Box::pin(async move {
            // Not `start_typing`: that returns a guard which stops the
            // indicator when it is dropped, so a caller that wants one
            // moment of typing and repeats it gets none at all. This posts
            // the one moment, which the service expires on its own.
            http.broadcast_typing(thread_id).await?;
            Ok(())
        })
    }
}

/// One gap announcement, as a boxed future.
pub type Announce =
    dyn Fn(usize) -> Pin<Box<dyn Future<Output = Result<(), TaskError>> + Send>> + Send + Sync;

/// The thread's mutable side: what it is showing and holding.
struct ThreadState {
    waiting_message_id: Option<String>,
    reactions: HashMap<String, String>,
    activity_message_id: Option<String>,
    activity_text: String,
    typing: Option<tokio::task::JoinHandle<()>>,
}

/// A thread a session posts to.
///
/// Reactions are replaced rather than accumulated: a scrolled-back thread
/// should read as final state, not as a history of transitions.
pub struct ChatThread<T: ThreadTransport> {
    transport: Arc<T>,
    log: Logger,
    forward_tool_output: bool,
    outbox: Outbox,
    state: Arc<Mutex<ThreadState>>,
}

impl<T: ThreadTransport> ChatThread<T> {
    /// A thread over `transport`, announcing dropped work to itself.
    pub fn new(transport: Arc<T>, log: Logger, forward_tool_output: bool) -> Self {
        let announcer: Arc<Announce> = {
            let transport = transport.clone();
            Arc::new(move |count| {
                let transport = transport.clone();
                Box::pin(async move {
                    transport
                        .send(
                            format!("[{count} earlier message(s) dropped while disconnected]"),
                            None,
                        )
                        .await
                        .map(|_| ())
                })
            })
        };
        Self {
            transport,
            outbox: Outbox::new(log.clone(), announcer, DEFAULT_MAX_BUFFERED),
            log,
            forward_tool_output,
            state: Arc::new(Mutex::new(ThreadState {
                waiting_message_id: None,
                reactions: HashMap::new(),
                activity_message_id: None,
                activity_text: String::new(),
                typing: None,
            })),
        }
    }

    /// True once closed, after which nothing more will ever be sent.
    ///
    /// Closing closes the outbox, which is correct for a session that is over
    /// and fatal for one that is starting: a reused closed port accepts posts
    /// and silently drops every one of them.
    #[allow(
        dead_code,
        reason = "read by this module's tests, which assert on state the daemon never asks for"
    )]
    pub fn is_closed(&self) -> bool {
        self.outbox.is_closed()
    }

    /// Waits for queued work to run. Used by tests, which have no connection.
    #[allow(
        dead_code,
        reason = "read by this module's tests, which assert on state the daemon never asks for"
    )]
    pub async fn flush(&self) {
        self.outbox.flush().await;
    }

    /// Shows a typing indicator while the agent is working.
    ///
    /// The service expires the indicator after about ten seconds, so holding
    /// it means repeating it. This is the only signal during a long tool loop
    /// that the session is alive rather than stuck.
    pub fn set_busy(&self, busy: bool) {
        let mut state = self.state.lock().expect("the thread state lock");
        if busy {
            if state.typing.is_some() {
                return;
            }
            let transport = Arc::clone(&self.transport);
            let log = self.log.clone();
            tokio::spawn(async move {
                if let Err(error) = transport.send_typing().await {
                    log.warn(
                        "the typing indicator could not be shown",
                        &fields([("detail", LogValue::from(error.to_string()))]),
                    );
                }
            });
            state.typing = Some(tokio::spawn(typing_loop(self.transport.clone())));
            return;
        }

        if let Some(running) = state.typing.take() {
            running.abort();
        }
    }

    /// Posts the message, split into pieces the service will take.
    pub fn post(&self, text: &str) {
        // The agent speaking ends the current run of tool calls, so the next
        // one starts a fresh block rather than being appended below prose.
        self.reset_activity();
        for chunk in split_message(text, MESSAGE_LIMIT) {
            let transport = Arc::clone(&self.transport);
            let chunk = chunk.clone();
            self.outbox.enqueue(Box::new(move || {
                let transport = Arc::clone(&transport);
                let chunk = chunk.clone();
                Box::pin(async move { transport.send(chunk, None).await.map(|_| ()) })
                    as Pin<Box<dyn Future<Output = Result<(), TaskError>> + Send>>
            }));
        }
    }

    /// Renders a change as a fenced diff, which is all a thread can show.
    pub fn post_diff(&self, path: &str, added: u64, removed: u64, body: &str) {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "diff line counts sit far below any pointer-width limit"
        )]
        let diff = FileDiff {
            empty: false,
            added: added as usize,
            removed: removed as usize,
            body: body.to_owned(),
        };
        self.post(&render_diff(path, &diff));
    }

    /// Shows that a cheaper model was asked something.
    ///
    /// One line, not the answer: the answer goes to the agent, and what a
    /// reader needs is that part of this turn was not the session's own
    /// model.
    pub fn note_delegation(&self, delegated: &Delegated) {
        self.post(&delegation_line(delegated));
    }

    /// Posts what a tool produced, when the thread is configured to forward
    /// it.
    ///
    /// A thread cannot attach output to the call above it, so forwarding is
    /// the only way to show it there, and it stays off by default because it
    /// is a lot of text in a conversation.
    pub fn note_tool_result(&self, result: &ToolResult) {
        if !self.forward_tool_output {
            return;
        }
        let body = result.output.trim();
        if body.is_empty() {
            return;
        }
        self.post(&format!("```\n{body}\n```"));
    }

    /// Adds a line of tool activity, extending the current block when there
    /// is one.
    ///
    /// A run of tool calls is one thing the agent is doing, not ten. Editing
    /// one message keeps it as one block, and keeps a busy turn from pushing
    /// the agent's own words off the screen.
    pub fn append_activity(&self, line: &str) {
        let line = line.to_owned();
        let transport = Arc::clone(&self.transport);
        let state = Arc::clone(&self.state);
        self.outbox.enqueue(Box::new(move || {
            let transport = Arc::clone(&transport);
            let state = Arc::clone(&state);
            let line = line.clone();
            Box::pin(async move {
                let extended = {
                    let state = state.lock().expect("the thread state lock");
                    if state.activity_text.is_empty() {
                        line.clone()
                    } else {
                        format!("{}\n{line}", state.activity_text)
                    }
                };

                let fresh = {
                    let mut state = state.lock().expect("the thread state lock");
                    let fresh = state.activity_message_id.is_none()
                        || extended.chars().count() > MESSAGE_LIMIT;
                    if !fresh {
                        state.activity_text.clear();
                        state.activity_text.push_str(&extended);
                    }
                    fresh
                };

                if !fresh {
                    let id = state
                        .lock()
                        .expect("the thread state lock")
                        .activity_message_id
                        .clone()
                        .unwrap_or_default();
                    transport.edit(id, extended).await?;
                    return Ok(());
                }

                // One entry can exceed a whole message on its own. Nothing is
                // dropped: it is split, and the block continues from the
                // final piece.
                let pieces = split_message(&line, MESSAGE_LIMIT);
                let mut last: Option<MessageHandle> = None;
                for piece in &pieces {
                    let sent = transport.send(piece.clone(), None).await?;
                    last = Some(sent);
                }
                let mut state = state.lock().expect("the thread state lock");
                state.activity_message_id = last.map(|sent| sent.id);
                state.activity_text = pieces.last().cloned().unwrap_or_default();
                Ok(())
            })
        }));
    }

    /// Buffers while the gateway is down and drains when it comes back.
    pub fn follow(&self, connection: watch::Receiver<bool>) {
        self.outbox.follow(connection);
    }

    /// Creates, updates, or takes away the one message reporting queue
    /// position.
    pub async fn set_waiting(&self, text: Option<&str>) {
        match text {
            None => {
                let id = self
                    .state
                    .lock()
                    .expect("the thread state lock")
                    .waiting_message_id
                    .take();
                if let Some(id) = id
                    && let Err(error) = self.transport.delete(id).await
                {
                    // A missing waiting message is cosmetic; the session
                    // continues.
                    self.warn_waiting(&error);
                }
            }
            Some(text) => {
                let shown = format!("[{text}]");
                let existing = self
                    .state
                    .lock()
                    .expect("the thread state lock")
                    .waiting_message_id
                    .clone();
                match existing {
                    None => match self.transport.send(shown, None).await {
                        Ok(sent) => {
                            self.state
                                .lock()
                                .expect("the thread state lock")
                                .waiting_message_id = Some(sent.id);
                        }
                        Err(error) => self.warn_waiting(&error),
                    },
                    Some(id) => {
                        if let Err(error) = self.transport.edit(id, shown).await {
                            self.warn_waiting(&error);
                            self.state
                                .lock()
                                .expect("the thread state lock")
                                .waiting_message_id = None;
                        }
                    }
                }
            }
        }
    }

    fn warn_waiting(&self, error: &dyn std::fmt::Display) {
        self.log.warn(
            "could not update the waiting message",
            &fields([("detail", LogValue::from(error.to_string()))]),
        );
    }

    /// Sets the outcome reaction on a message, replacing any earlier one.
    pub async fn set_reaction(&self, message_id: &str, outcome: ReactionOutcome) {
        let glyph = reaction(outcome.into());
        let previous = {
            let state = self.state.lock().expect("the thread state lock");
            state.reactions.get(message_id).cloned()
        };
        if previous.as_deref() == Some(glyph.as_str()) {
            return;
        }

        let found = self
            .transport
            .fetch(message_id.to_owned())
            .await
            .ok()
            .flatten();
        let Some(_) = found else { return };

        if let Some(previous) = &previous
            && let Err(error) = self
                .transport
                .remove_reaction(message_id.to_owned(), previous.clone())
                .await
        {
            // A reaction is an acknowledgement, not the work. Losing one
            // must not disturb the session it was acknowledging.
            self.warn_reaction(&error);
            return;
        }
        match self
            .transport
            .react(message_id.to_owned(), glyph.clone())
            .await
        {
            Ok(()) => {
                self.state
                    .lock()
                    .expect("the thread state lock")
                    .reactions
                    .insert(message_id.to_owned(), glyph);
            }
            Err(error) => self.warn_reaction(&error),
        }
    }

    fn warn_reaction(&self, error: &dyn std::fmt::Display) {
        self.log.warn(
            "could not set a reaction",
            &fields([("detail", LogValue::from(error.to_string()))]),
        );
    }

    /// Uploads a file with its caption.
    ///
    /// The bytes are taken by value and cloned into the queued task, so the
    /// caller keeps what it passed.
    pub fn upload(&self, name: &str, bytes: Vec<u8>, caption: &str) {
        self.reset_activity();
        let transport = Arc::clone(&self.transport);
        let name = name.to_owned();
        let caption = caption.to_owned();
        self.outbox.enqueue(Box::new(move || {
            let transport = Arc::clone(&transport);
            let name = name.clone();
            let bytes = bytes.clone();
            let caption = caption.clone();
            Box::pin(async move {
                transport
                    .send(caption, Some((name, bytes)))
                    .await
                    .map(|_| ())
            })
        }));
    }

    /// Finishes with the thread, archiving it only when somebody said to.
    ///
    /// A thread archived because its session idled out drops off the sidebar,
    /// and the people who were in it have to go hunting for it. Every reason
    /// but a deliberate stop can also be resumed, so the thread is left where
    /// it is and the last notice in it says what happened.
    pub async fn close(&self, reason: EndReason) {
        self.set_busy(false);
        self.state
            .lock()
            .expect("the thread state lock")
            .activity_message_id = None;
        self.outbox.flush().await;
        self.outbox.close();

        if reason != EndReason::Stopped {
            return;
        }
        if let Err(error) = self.transport.set_archived(true).await {
            self.log.warn(
                "could not archive the thread",
                &fields([("detail", LogValue::from(error.to_string()))]),
            );
        }
    }

    fn reset_activity(&self) {
        let state = Arc::clone(&self.state);
        self.outbox.enqueue(Box::new(move || {
            let state = Arc::clone(&state);
            Box::pin(async move {
                let mut state = state.lock().expect("the thread state lock");
                state.activity_message_id = None;
                state.activity_text = String::new();
                Ok(())
            })
        }));
    }
}

impl<T: ThreadTransport> SessionView for ChatThread<T> {
    fn observe<'a>(
        &'a self,
        event: &'a SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), ViewError>> + Send + 'a>> {
        Box::pin(async move {
            match event {
                // A thread has one voice, so a notice is posted like
                // anything else; so is a reply, whose command already sits
                // above it in the thread.
                SessionEvent::Post { text }
                | SessionEvent::Notice { text, .. }
                | SessionEvent::Reply { text, .. } => self.post(text),
                SessionEvent::Diff {
                    path,
                    added,
                    removed,
                    body,
                    ..
                } => self.post_diff(path, *added, *removed, body),
                SessionEvent::Activity { line, .. } => self.append_activity(line),
                SessionEvent::ToolResult { result } => self.note_tool_result(result),
                SessionEvent::Delegation { delegated } => self.note_delegation(delegated),
                SessionEvent::Waiting { text } => {
                    self.set_waiting(text.as_deref()).await;
                }
                SessionEvent::Reaction {
                    message_id,
                    outcome,
                } => self.set_reaction(message_id, *outcome).await,
                SessionEvent::Upload { name, bytes, caption } => {
                    self.upload(name, bytes.clone(), caption);
                }
                SessionEvent::Busy { busy } => self.set_busy(*busy),
                SessionEvent::Close { reason } => self.close(*reason).await,
                // The prompt is already in the thread, posted by the person
                // who wrote it; an aside likewise; reasoning is long and a
                // thread is a conversation; usage is a dashboard thing; and
                // turn boundaries are plain from the messages themselves.
                SessionEvent::Prompt { .. }
                | SessionEvent::Aside { .. }
                | SessionEvent::Thinking { .. }
                | SessionEvent::Usage { .. }
                | SessionEvent::BeginTurn { .. }
                // An attachment is recorded for surfaces that can show one;
                // the upload event above already carried the file.
                | SessionEvent::Attachment { .. } => {}
            }
            Ok(())
        })
    }
}

/// Repeats the typing indicator for as long as the task lives.
async fn typing_loop<T: ThreadTransport>(transport: Arc<T>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(TYPING_REFRESH_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The interval's first tick is immediate, and the ping already went out
    // when the turn started.
    tick.tick().await;
    loop {
        tick.tick().await;
        // A missed refresh is cosmetic; the next tick is seconds away.
        if transport.send_typing().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests;

/// Creates threads on the served channel, one per session.
pub struct ChatThreadFactory {
    channel_id: ChannelId,
    http: Arc<serenity::http::Http>,
    log: Logger,
    /// Whether a thread shows what tools produced. Off unless configured.
    forward_tool_output: bool,
    /// Whether the gateway is up, which every port it builds follows.
    connection: watch::Receiver<bool>,
}

impl ChatThreadFactory {
    /// A factory for the served channel.
    pub fn new(
        channel_id: ChannelId,
        http: Arc<serenity::http::Http>,
        log: Logger,
        forward_tool_output: bool,
        connection: watch::Receiver<bool>,
    ) -> Self {
        Self {
            channel_id,
            http,
            log,
            forward_tool_output,
            connection,
        }
    }

    fn adopt(&self, thread_id: ChannelId) -> ChatThread<SerenityThread> {
        let thread = ChatThread::new(
            Arc::new(SerenityThread::new(thread_id, Arc::clone(&self.http))),
            self.log.clone(),
            self.forward_tool_output,
        );
        thread.follow(self.connection.clone());
        thread
    }

    /// Starts a thread from the message that asked for the session.
    pub async fn create(
        &self,
        starter: MessageId,
        name: &str,
    ) -> Result<(String, ChatThread<SerenityThread>), String> {
        let thread = self
            .channel_id
            .create_thread_from_message(
                &self.http,
                starter,
                serenity::builder::CreateThread::new(name)
                    .auto_archive_duration(serenity::model::channel::AutoArchiveDuration::OneDay),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok((thread.id.get().to_string(), self.adopt(thread.id)))
    }

    /// Opens a thread with no message to hang it on, by posting one first.
    ///
    /// A thread hangs off a message, so one is posted first. That message is
    /// also what tells the channel that work has started somewhere else.
    pub async fn open(
        &self,
        name: &str,
        opener: &str,
    ) -> Result<(String, ChatThread<SerenityThread>), String> {
        let starter = self
            .channel_id
            .send_message(&self.http, plain(opener))
            .await
            .map_err(|error| error.to_string())?;
        self.create(starter.id, name).await
    }

    /// A thread for one that already exists, adopted by id.
    ///
    /// A resumed thread was created by a previous run of the daemon, so it is
    /// fetched rather than read from the cache, which is cold after a
    /// restart. A thread archived by the service's own inactivity is
    /// reopened, since somebody writing in it is asking for exactly that.
    pub async fn port_for(&self, thread_id: ChannelId) -> Option<ChatThread<SerenityThread>> {
        let channel = thread_id.to_channel(&self.http).await.ok()?;
        let Channel::Guild(thread) = channel else {
            return None;
        };
        if !matches!(
            thread.kind,
            ChannelType::PublicThread | ChannelType::PrivateThread | ChannelType::NewsThread
        ) {
            return None;
        }
        if thread
            .thread_metadata
            .as_ref()
            .is_some_and(|metadata| metadata.archived)
            && let Err(error) = thread_id
                .edit_thread(&self.http, EditThread::new().archived(false))
                .await
        {
            self.log.warn(
                "could not reopen a thread",
                &fields([("detail", LogValue::from(error.to_string()))]),
            );
        }
        Some(self.adopt(thread_id))
    }
}
