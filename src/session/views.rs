//! One session, presented through any number of surfaces at once.
//!
//! A session reports what it is doing by sending [`SessionEvent`]s here, and
//! the fan-out delivers each one to every attached view, so a chat thread and
//! a browser are two views of the same session rather than two sessions.
//!
//! It also keeps a bounded record of what was reported, so a view that
//! attaches to a session already in progress is shown what it missed instead
//! of an empty pane. The record holds what was said, not the state around it:
//! state is kept separately and applied as it currently stands, because
//! replaying every queue position a session ever had would be noise rather
//! than history.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use futures_util::future::join_all;

use crate::log::{LogValue, Logger, fields};
use crate::session::event::{ReactionOutcome, SessionEvent, SessionUsage};

/// How much of a session's output is kept for a view that attaches later.
pub const DEFAULT_TRANSCRIPT_LIMIT: usize = 400;

/// What stands in for a withdrawn turn, so a reader is not left guessing.
pub const WITHDRAWN_NOTE: &str = "[withdrawn by the person who sent it]";

/// What to show for a recorded prompt or aside.
///
/// A withdrawn entry keeps its place and loses its words. Showing the empty
/// text would read as somebody having said nothing, which is a different and
/// untrue thing.
pub fn shown_text(withdrawn: bool, text: &str) -> &str {
    if withdrawn { WITHDRAWN_NOTE } else { text }
}

/// One recorded thing and the turn it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    /// The turn, when one was being recorded. None for anything recorded
    /// before turns were kept, which is why a transcript written by an older
    /// daemon still reads.
    pub turn: Option<u32>,
    pub entry: SessionEvent,
}

/// Where output is written down so it outlives the session.
///
/// A trait rather than the transcript itself, so this module knows nothing
/// about files and a test needs none.
pub trait Recorder: Send + Sync {
    fn append(&self, entry: &SessionEvent, turn: u32);
}

/// What a failed delivery said.
pub type ViewError = Box<dyn std::error::Error + Send + Sync>;

/// One surface showing one session.
///
/// The session sends events; a view matches exhaustively and shows what each
/// one means for its surface. A delivery may fail, and a failing view is
/// skipped: the session carries on without it.
pub trait SessionView: Send + Sync {
    fn observe<'a>(
        &'a self,
        event: &'a SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), ViewError>> + Send + 'a>>;
}

/// The state a view needs in order to look right the moment it attaches.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewState {
    pub busy: bool,
    pub waiting: Option<String>,
    pub ended: bool,
    pub usage: Option<SessionUsage>,
}

/// The fan-out's whole state, guarded by one lock.
#[derive(Default)]
struct Inner {
    views: Vec<Arc<dyn SessionView>>,
    held: Vec<Held>,
    reactions: Vec<(String, ReactionOutcome)>,
    busy: bool,
    waiting: Option<String>,
    ended: bool,
    dropped: usize,
    usage: Option<SessionUsage>,
    /// The turn being recorded. Zero is everything before the first prompt,
    /// which is where a session's opening notices live.
    turn: u32,
}

impl Inner {
    fn state(&self) -> ViewState {
        ViewState {
            busy: self.busy,
            waiting: self.waiting.clone(),
            ended: self.ended,
            usage: self.usage.clone(),
        }
    }
}

/// What a replay needs, taken under one lock.
struct Snapshot {
    dropped: usize,
    held: Vec<Held>,
    reactions: Vec<(String, ReactionOutcome)>,
    state: ViewState,
    turn: u32,
}

/// What a failure was about, named as the event is.
fn label_of(event: &SessionEvent) -> &'static str {
    match event {
        SessionEvent::Post { .. } => "post",
        SessionEvent::Prompt { .. } => "prompt",
        SessionEvent::Aside { .. } => "aside",
        SessionEvent::Notice { .. } => "notice",
        SessionEvent::Thinking { .. } => "thinking",
        SessionEvent::Reply { .. } => "reply",
        SessionEvent::ToolResult { .. } => "toolResult",
        SessionEvent::Activity { .. } => "activity",
        SessionEvent::Delegation { .. } => "delegation",
        SessionEvent::Diff { .. } => "diff",
        SessionEvent::Attachment { .. } => "attachment",
        SessionEvent::Upload { .. } => "upload",
        SessionEvent::Usage { .. } => "usage",
        SessionEvent::Waiting { .. } => "waiting",
        SessionEvent::Reaction { .. } => "reaction",
        SessionEvent::Busy { .. } => "busy",
        SessionEvent::BeginTurn { .. } => "turn",
        SessionEvent::Close { .. } => "close",
    }
}

/// Replaces a message's outcome, keeping the place it was first set in.
fn set_reaction(
    reactions: &mut Vec<(String, ReactionOutcome)>,
    message_id: &str,
    outcome: ReactionOutcome,
) {
    for (existing, value) in reactions.iter_mut() {
        if existing == message_id {
            *value = outcome;
            return;
        }
    }
    reactions.push((message_id.to_owned(), outcome));
}

/// Delivers a session's output to every attached view.
pub struct ViewFanOut {
    log: Logger,
    limit: usize,
    recorder: Option<Arc<dyn Recorder>>,
    inner: Mutex<Inner>,
}

impl ViewFanOut {
    /// Creates a fan-out keeping the default amount of history.
    pub fn new(log: Logger) -> Self {
        Self::with_recorder(log, DEFAULT_TRANSCRIPT_LIMIT, None)
    }

    /// Creates a fan-out with its own limit and somewhere to write output
    /// down, so it survives the session.
    pub fn with_recorder(log: Logger, limit: usize, recorder: Option<Arc<dyn Recorder>>) -> Self {
        Self {
            log,
            limit,
            recorder,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Seeds the record from a stored transcript, for a resumed session.
    ///
    /// Replaces what is held rather than adding to it, so restoring twice
    /// cannot double a session's history.
    pub fn restore(&self, entries: &[Held], dropped: usize) {
        let mut inner = self.inner.lock().expect("the view fan-out lock");
        let kept_from = entries.len().saturating_sub(self.limit);
        inner.held = entries[kept_from..]
            .iter()
            .filter(|held| !matches!(held.entry, SessionEvent::Usage { .. }))
            .cloned()
            .collect();
        inner.dropped = dropped + kept_from;

        // State, not history: the last one stands and none of them is replayed
        // as an event, or a reader would watch the cost climb through every
        // turn the session ever ran.
        inner.usage = entries.iter().rev().find_map(|held| match &held.entry {
            SessionEvent::Usage { usage } => Some(usage.clone()),
            _ => None,
        });

        // Counted over everything restored rather than over what is kept:
        // usage is dropped from the record, and a turn whose only surviving
        // entry was its cost would otherwise be forgotten and its number
        // handed out twice.
        inner.turn = entries
            .iter()
            .map(|held| held.turn.unwrap_or(0))
            .max()
            .unwrap_or(0);
    }

    /// The turn the recorded history has reached.
    ///
    /// Read when a session resumes, so its own counter carries on rather than
    /// restarting at one and labelling a new exchange with a number the stored
    /// history already used.
    pub fn current_turn(&self) -> u32 {
        self.inner.lock().expect("the view fan-out lock").turn
    }

    /// How many views are currently attached.
    pub fn size(&self) -> usize {
        self.inner
            .lock()
            .expect("the view fan-out lock")
            .views
            .len()
    }

    /// The session's current state, for a view that has just attached.
    pub fn state(&self) -> ViewState {
        self.inner.lock().expect("the view fan-out lock").state()
    }

    /// Everything kept for replay, oldest first.
    pub fn history(&self) -> Vec<SessionEvent> {
        let inner = self.inner.lock().expect("the view fan-out lock");
        inner.held.iter().map(|held| held.entry.clone()).collect()
    }

    /// Everything kept for replay, with the turn each belongs to.
    pub fn held(&self) -> Vec<Held> {
        self.inner
            .lock()
            .expect("the view fan-out lock")
            .held
            .clone()
    }

    /// How much was dropped from the record, which a replay must admit to.
    pub fn dropped_count(&self) -> usize {
        self.inner.lock().expect("the view fan-out lock").dropped
    }

    /// Sends one event to every attached view, and keeps what replay needs.
    pub async fn send(&self, event: SessionEvent) {
        // Recorded and the audience read under one lock, so a view attaching
        // now either sees the event in its replay or live, never both and
        // never neither.
        let views = {
            let mut inner = self.inner.lock().expect("the view fan-out lock");
            self.record(&mut inner, &event);
            inner.views.clone()
        };
        self.deliver(&views, &event).await;
    }

    /// Attaches a view and shows it what it missed.
    pub async fn attach(self: Arc<Self>, view: Arc<dyn SessionView>) -> Attached {
        let snapshot = {
            let mut inner = self.inner.lock().expect("the view fan-out lock");
            if !inner
                .views
                .iter()
                .any(|attached| Arc::ptr_eq(attached, &view))
            {
                inner.views.push(Arc::clone(&view));
            }
            Snapshot {
                dropped: inner.dropped,
                held: inner.held.clone(),
                reactions: inner.reactions.clone(),
                state: inner.state(),
                turn: inner.turn,
            }
        };

        if let Err(error) = Self::replay_to(&view, &snapshot).await {
            self.log.warn(
                "replaying to a new view failed",
                &fields([("detail", error.to_string().into())]),
            );
        }
        Attached { fan: self, view }
    }

    /// Detaches a view without affecting the session or the other views.
    pub fn detach(&self, view: &Arc<dyn SessionView>) {
        self.inner
            .lock()
            .expect("the view fan-out lock")
            .views
            .retain(|attached| !Arc::ptr_eq(attached, view));
    }

    /// Records what replay keeps, and tracks the state a new view needs.
    ///
    /// Replies are not kept: a command's answer belongs to whoever ran it, in
    /// the moment they ran it, and is not part of the conversation with the
    /// agent. Usage is written down but not held, because only the latest
    /// total is meaningful.
    fn record(&self, inner: &mut Inner, event: &SessionEvent) {
        match event {
            SessionEvent::BeginTurn { turn } => {
                inner.turn = *turn;
                return;
            }
            SessionEvent::Waiting { text } => {
                inner.waiting.clone_from(text);
                return;
            }
            SessionEvent::Reaction {
                message_id,
                outcome,
            } => {
                set_reaction(&mut inner.reactions, message_id, *outcome);
                return;
            }
            SessionEvent::Busy { busy } => {
                inner.busy = *busy;
                return;
            }
            SessionEvent::Close { .. } => {
                inner.ended = true;
                inner.busy = false;
                return;
            }
            SessionEvent::Reply { .. } => return,
            SessionEvent::Usage { usage } => {
                inner.usage = Some(usage.clone());
                if let Some(recorder) = &self.recorder {
                    recorder.append(event, inner.turn);
                }
                return;
            }
            _ => {}
        }

        let entry = match event {
            SessionEvent::Upload { name, bytes, .. } => SessionEvent::Attachment {
                name: name.clone(),
                size: bytes.len() as u64,
            },
            other => other.clone(),
        };
        if let Some(recorder) = &self.recorder {
            recorder.append(&entry, inner.turn);
        }
        inner.held.push(Held {
            turn: Some(inner.turn),
            entry,
        });
        let excess = inner.held.len().saturating_sub(self.limit);
        if excess > 0 {
            inner.held.drain(..excess);
            inner.dropped += excess;
        }
    }

    /// Delivers to every view, and keeps going when one of them fails.
    ///
    /// A browser that closed mid-turn must not stop the chat thread from being
    /// told what happened, so a failing view is logged and skipped rather than
    /// allowed to propagate into the session.
    async fn deliver(&self, views: &[Arc<dyn SessionView>], event: &SessionEvent) {
        let results = join_all(views.iter().map(|view| view.observe(event))).await;
        for result in results {
            if let Err(error) = result {
                self.log.warn(
                    "a view failed and was skipped",
                    &fields([
                        ("what", LogValue::from(label_of(event))),
                        ("detail", LogValue::from(error.to_string())),
                    ]),
                );
            }
        }
    }

    /// Shows a view everything it missed, then where things stand now.
    ///
    /// The first failure stops the replay: a view that cannot be told the
    /// beginning cannot be trusted with the end either.
    async fn replay_to(view: &Arc<dyn SessionView>, snapshot: &Snapshot) -> Result<(), ViewError> {
        if snapshot.dropped > 0 {
            view.observe(&SessionEvent::Post {
                text: format!("[{} earlier line(s) not kept]", snapshot.dropped),
            })
            .await?;
        }

        // The turn is re-announced at each boundary rather than passed with
        // every call, so a view stamps what it draws without every event
        // growing an argument that only one surface uses.
        let mut announced: Option<u32> = None;
        for held in &snapshot.held {
            if let Some(turn) = held.turn
                && announced != Some(turn)
            {
                announced = Some(turn);
                view.observe(&SessionEvent::BeginTurn { turn }).await?;
            }

            match &held.entry {
                SessionEvent::Post { .. }
                | SessionEvent::Notice { .. }
                | SessionEvent::Thinking { .. }
                | SessionEvent::Reply { .. }
                | SessionEvent::ToolResult { .. }
                | SessionEvent::Activity { .. }
                | SessionEvent::Delegation { .. }
                | SessionEvent::Diff { .. } => view.observe(&held.entry).await?,
                SessionEvent::Prompt {
                    author,
                    text,
                    withdrawn,
                    ..
                }
                | SessionEvent::Aside {
                    author,
                    text,
                    withdrawn,
                    ..
                } => {
                    view.observe(&SessionEvent::Prompt {
                        author: author.clone(),
                        text: shown_text(*withdrawn, text).to_owned(),
                        id: None,
                        withdrawn: false,
                    })
                    .await?;
                }
                SessionEvent::Attachment { name, size } => {
                    view.observe(&SessionEvent::Post {
                        text: format!("[attached {name}, {size} bytes]"),
                    })
                    .await?;
                }
                // Usage is state, carried below rather than replayed. The rest
                // is never recorded, so it is never held.
                SessionEvent::Upload { .. }
                | SessionEvent::Usage { .. }
                | SessionEvent::Waiting { .. }
                | SessionEvent::Reaction { .. }
                | SessionEvent::Busy { .. }
                | SessionEvent::BeginTurn { .. }
                | SessionEvent::Close { .. } => {}
            }
        }

        // Anything the view is told after this belongs to the turn in
        // progress, which is not the one the last replayed entry belonged to.
        if announced.is_some() && announced != Some(snapshot.turn) {
            view.observe(&SessionEvent::BeginTurn {
                turn: snapshot.turn,
            })
            .await?;
        }

        for (message_id, outcome) in &snapshot.reactions {
            view.observe(&SessionEvent::Reaction {
                message_id: message_id.clone(),
                outcome: *outcome,
            })
            .await?;
        }

        if let Some(usage) = &snapshot.state.usage {
            view.observe(&SessionEvent::Usage {
                usage: usage.clone(),
            })
            .await?;
        }
        if let Some(text) = &snapshot.state.waiting {
            view.observe(&SessionEvent::Waiting {
                text: Some(text.clone()),
            })
            .await?;
        }
        // Only when it is true: a view that has just attached already assumes
        // a session is not working, so saying so is noise.
        if snapshot.state.busy {
            view.observe(&SessionEvent::Busy { busy: true }).await?;
        }
        if snapshot.state.ended {
            view.observe(&SessionEvent::Post {
                text: "[this session has ended]".to_owned(),
            })
            .await?;
        }
        Ok(())
    }
}

/// One view's attachment to a session.
///
/// Ending it takes the view out of the fan-out and nothing else: detaching
/// never ends the session, however many views are left.
pub struct Attached {
    fan: Arc<ViewFanOut>,
    view: Arc<dyn SessionView>,
}

impl Attached {
    /// Detaches the view without affecting the session or the other views.
    pub fn detach(self) {
        self.fan.detach(&self.view);
    }
}

#[cfg(test)]
#[path = "views/tests.rs"]
mod tests;
