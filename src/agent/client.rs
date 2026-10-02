//! Drives one agent process: commands out, events in.
//!
//! The client owns the protocol only. It knows nothing about chat, threads, or
//! admission, so it can be exercised against a fake process in tests.
//!
//! Turn completion is taken from the turn end, not from the process ending. A
//! process end may be followed by nothing at all, so releasing an admission
//! slot on anything but the turn end would let more work into the provider
//! than the cap allows.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::agent::framing::LineFramer;
use crate::agent::protocol::{
    AcpUpdate, AgentRecord, DialogMethod, DialogRequest, FrameKind, StreamingBehavior, Usage,
    as_permission_ask, ask_option_ids, cancel_ask_frame, cancel_frame, classify_frame,
    classify_update, compact_frame, detail_of, image_block, initialize_frame, is_retryable,
    method_not_found_frame, new_session_frame, prompt_frame, resume_session_frame,
    select_option_frame, set_config_option_frame, stop_failure, text_block, tool_target,
};
use crate::log::Logger;
use crate::log::fields;

/// Where the agent is in its life.
///
/// One value rather than a set of booleans, because the interesting questions
/// are about combinations: whether a command may be sent, and whether a turn
/// was in flight when the process died. Two booleans can hold a state that
/// cannot happen, and then something has to decide what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// Started, not yet answering.
    Starting,
    /// Answering, no turn running.
    Ready,
    /// A turn is running.
    Working,
    /// The process has ended.
    Ended,
}

/// The process the client speaks to. Abstracted so tests need no sandbox.
///
/// `write` is synchronous and queueing: it records or enqueues the bytes and
/// answers at once, matching a write that is fired and not awaited. A failed
/// write surfaces as the process ending, which the client already handles.
pub trait AgentProcess: Send + Sync + 'static {
    /// Writes to the agent's stdin. Fails once the process is gone.
    fn write(&self, bytes: &[u8]) -> std::io::Result<()>;
    /// Reads what the agent produced, up to the buffer's size.
    fn read_stdout<'a>(
        &'a self,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = std::io::Result<usize>> + Send + 'a>>;
    /// Reads what the launcher itself says about failing.
    fn read_stderr<'a>(
        &'a self,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = std::io::Result<usize>> + Send + 'a>>;
    /// Resolves with the exit code once the process ends.
    fn exited(&self) -> Pin<Box<dyn Future<Output = Option<i32>> + Send>>;
}

/// A callback that reports one fact. The unit form reports an event alone.
pub type Callback<A = ()> = Option<Box<dyn Fn(A) + Send + Sync>>;

/// Everything the client reports outward. Every callback is optional.
///
/// The `on_` prefix is what every handler is called in the protocol's own
/// vocabulary; renaming the fields would not make them fields.
#[expect(
    clippy::struct_field_names,
    reason = "the prefix is the protocol's own vocabulary"
)]
#[derive(Default)]
pub struct AgentHandlers {
    /// A turn started.
    pub on_turn_start: Callback,
    /// The assistant finished saying something, reported as it happens so that
    /// it interleaves with tool activity in the order the agent produced it.
    pub on_assistant_text: Callback<String>,
    /// A turn settled, with whether it said anything and why it failed.
    pub on_turn_settled: Callback<(bool, Option<String>)>,
    /// The agent began a tool call.
    pub on_tool_start: Callback<(String, String, Option<String>)>,
    /// A tool call finished, with whether it failed.
    pub on_tool_end: Callback<(String, String, bool, String)>,
    /// The agent is thinking. Reported once per turn.
    pub on_thinking: Callback,
    /// What a finished turn cost, when the agent reported it.
    pub on_usage: Callback<Usage>,
    /// What the agent reasoned, as it arrives.
    pub on_thought: Callback<String>,
    /// The agent reported an error.
    pub on_error: Callback<String>,
    /// The agent refused a command outright, so no turn will follow it.
    pub on_command_rejected: Callback<(String, String)>,
    /// The agent began an automatic retry, which is the rate limit signal.
    pub on_retry: Callback<String>,
    /// The agent is blocked on a dialog.
    pub on_dialog: Callback<DialogRequest>,
    /// A dialog went unanswered long enough that it was cancelled.
    pub on_dialog_timeout: Callback<DialogRequest>,
    /// The agent asked for an interaction a chat thread cannot serve.
    pub on_unsupported_dialog: Callback<String>,
    /// The process ended. Reported exactly once, with whether a turn ran.
    pub on_exit: Callback<(i64, bool)>,
    /// The protocol was violated and the session cannot continue.
    pub on_protocol_violation: Callback<String>,
}

/// What happened to a reply offered for a pending dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerOutcome {
    /// Accepted and sent to the agent.
    Accepted,
    /// Neither an option nor a yes or no; the dialog still stands.
    Unrecognized,
    /// No dialog with that id is pending.
    Unknown,
}

/// How much of a dying process's stderr is kept, in characters.
const STDERR_KEPT: usize = 2_000;

const AFFIRMATIVE: [&str; 5] = ["yes", "y", "true", "ok", "confirm"];
const NEGATIVE: [&str; 5] = ["no", "n", "false", "cancel", "deny"];

/// The reply to a dialog request, in the shapes the protocol carries.
enum DialogReply {
    /// A choice or free text.
    Value(String),
    /// A confirmation, or its refusal. No confirm dialogs arrive over this
    /// protocol, but the validation stays beside the select and input checks
    /// it was written with.
    #[allow(
        dead_code,
        reason = "no confirm dialogs arrive over ACP; kept with the validation it belongs to"
    )]
    Confirmed(bool),
}

/// What one outbound request waits on: an answer for the caller, or nothing
/// because the answer only feeds the turn.
pub(crate) enum Awaited {
    /// A caller waits on this channel for the answer.
    Rpc(oneshot::Sender<AgentRecord>),
    /// A prompt whose answer carries the turn's stop reason.
    Prompt,
}

/// A question the agent is blocked on, and the timer that gives up on it.
pub(crate) struct PendingDialog {
    /// What was asked, so an answer can be matched to it.
    pub(crate) request: DialogRequest,
    /// The offered option ids, in the same order as the request's names.
    pub(crate) option_ids: Vec<String>,
    /// Gives up on the question when nobody answers in time.
    pub(crate) timer: tokio::task::JoinHandle<()>,
}

/// The client's mutable state, shared between the caller and the readers.
///
/// The flags are independent one-bit facts read on different paths: whether
/// the exit, the thinking, and the settle were already reported, and whether
/// the turn said anything. An enum would tangle axes that never change
/// together.
#[expect(clippy::struct_excessive_bools)]
pub(crate) struct ClientState {
    /// Splits the agent's stream into whole records.
    pub(crate) framer: LineFramer,
    /// Where the process is between starting and gone.
    pub(crate) lifecycle: AgentState,
    /// Whether the exit has already been reported, so it is reported once.
    pub(crate) exit_reported: bool,
    /// Whether this turn has already said the agent is thinking.
    pub(crate) thinking_reported: bool,
    /// The last thing the assistant said, for a turn that ends without more.
    pub(crate) last_words: String,
    /// Whether this turn said anything at all.
    pub(crate) produced_text: bool,
    /// Whether this turn already settled, so a late turn end cannot settle
    /// it twice.
    pub(crate) turn_done: bool,
    /// Why this turn failed, when it did.
    pub(crate) turn_failure: Option<String>,
    /// How much context the model holds, once the agent has said.
    pub(crate) context_window: Option<f64>,
    /// The agent's session, once `session/new` has answered.
    pub(crate) session_id: Option<String>,
    /// Tool call titles by id, for ends that name no title.
    pub(crate) tool_titles: BTreeMap<String, String>,
    /// The last compaction counts, for the compact answer.
    pub(crate) pending_compaction: Option<(f64, f64)>,
    /// Questions the agent is blocked on, by request id.
    pub(crate) dialogs: BTreeMap<String, PendingDialog>,
    /// Requests waiting on an answer, by frame id.
    pub(crate) requests: BTreeMap<u64, Awaited>,
    /// Counter behind the frame ids this client issues.
    pub(crate) next_request_id: u64,
}

/// Turns a reply into a protocol response, or nothing when the reply does not
/// answer the question that was asked.
fn build_response(request: &DialogRequest, reply: &str) -> Option<DialogReply> {
    let trimmed = reply.trim();

    if request.method == DialogMethod::Confirm {
        let lowered = trimmed.to_lowercase();
        if AFFIRMATIVE.contains(&lowered.as_str()) {
            return Some(DialogReply::Confirmed(true));
        }
        if NEGATIVE.contains(&lowered.as_str()) {
            return Some(DialogReply::Confirmed(false));
        }
        return None;
    }

    if request.method == DialogMethod::Select {
        let options = request.options.as_deref().unwrap_or(&[]);
        if let Ok(index) = trimmed.parse::<usize>()
            && (1..=options.len()).contains(&index)
        {
            return Some(DialogReply::Value(options[index - 1].clone()));
        }
        let matched = options
            .iter()
            .find(|option| option.to_lowercase() == trimmed.to_lowercase())?;
        return Some(DialogReply::Value(matched.clone()));
    }

    if trimmed.is_empty() {
        return None;
    }
    Some(DialogReply::Value(trimmed.to_owned()))
}

/// The client's mutable state, shared between the caller and the readers.
struct Shared {
    process: Arc<dyn AgentProcess>,
    state: Arc<Mutex<ClientState>>,
    handlers: Arc<AgentHandlers>,
    log: Logger,
    dialog_timeout_ms: u64,
    cwd: String,
}

/// Speaks the agent protocol over one process's pipes.
#[derive(Clone)]
pub struct AgentClient {
    inner: Arc<Shared>,
}

impl AgentClient {
    /// A client over `process`, reporting through `handlers`.
    ///
    /// `cwd` is the working directory the agent's session opens in: the
    /// project as the agent sees it, which the sandbox decides.
    pub fn new(
        process: Arc<dyn AgentProcess>,
        handlers: AgentHandlers,
        log: Logger,
        dialog_timeout_ms: u64,
        max_record_bytes: Option<usize>,
        cwd: &str,
    ) -> Self {
        Self {
            inner: Arc::new(Shared {
                process,
                state: Arc::new(Mutex::new(ClientState {
                    framer: LineFramer::new(max_record_bytes.unwrap_or(8 * 1024 * 1024)),
                    lifecycle: AgentState::Starting,
                    exit_reported: false,
                    thinking_reported: false,
                    last_words: String::new(),
                    produced_text: false,
                    turn_done: false,
                    turn_failure: None,
                    context_window: None,
                    session_id: None,
                    tool_titles: BTreeMap::new(),
                    pending_compaction: None,
                    dialogs: BTreeMap::new(),
                    requests: BTreeMap::new(),
                    next_request_id: 1,
                })),
                handlers: Arc::new(handlers),
                log,
                dialog_timeout_ms,
                cwd: cwd.to_owned(),
            }),
        }
    }

    /// The last thing the agent said on stderr before it went.
    ///
    /// Kept because an exit code on its own explains nothing: a process killed
    /// for filling the disk and one that hit a bug both exit with 1, and only
    /// this says which. Bounded, since a crash can print a great deal.
    pub fn dying_words(&self) -> String {
        self.inner
            .state
            .lock()
            .expect("the client lock")
            .last_words
            .clone()
    }

    /// Where the agent is in its life.
    #[allow(
        dead_code,
        reason = "read by this module's tests, which assert on state the daemon never asks for"
    )]
    pub fn state(&self) -> AgentState {
        self.inner.state.lock().expect("the client lock").lifecycle
    }

    /// False once the process has ended or a write has failed.
    #[allow(
        dead_code,
        reason = "read by this module's tests, which assert on state the daemon never asks for"
    )]
    pub fn is_alive(&self) -> bool {
        self.state() != AgentState::Ended
    }

    /// True while a turn is running.
    #[allow(
        dead_code,
        reason = "read by this module's tests, which assert on state the daemon never asks for"
    )]
    pub fn is_working(&self) -> bool {
        self.state() == AgentState::Working
    }

    /// How many tokens the model can hold, once the agent has said.
    pub fn context_window(&self) -> Option<f64> {
        self.inner
            .state
            .lock()
            .expect("the client lock")
            .context_window
    }

    /// Begins reading both streams. Resolves when the process has ended.
    pub async fn run(&self) {
        let error_reader = Shared {
            process: Arc::clone(&self.inner.process),
            state: Arc::clone(&self.inner.state),
            handlers: Arc::clone(&self.inner.handlers),
            log: self.inner.log.clone(),
            dialog_timeout_ms: self.inner.dialog_timeout_ms,
            cwd: self.inner.cwd.clone(),
        };
        let stderr = tokio::spawn(async move { error_reader.read_stderr().await });
        self.inner.read_stdout().await;
        let _ = stderr.await;
        let code = self.inner.process.exited().await.unwrap_or(-1);
        self.end(i64::from(code));
    }

    /// Waits until the agent answers, which is what readiness means here.
    ///
    /// The handshake opens the session the whole client then speaks on: every
    /// later command names it, so nothing is sent before this resolves.
    pub async fn wait_until_ready(&self, timeout_ms: u64) -> Result<AgentRecord, String> {
        self.open_session(None, timeout_ms).await
    }

    /// Waits until the agent answers and reopens its recorded session, so a
    /// revived session carries on where its process left off. Falls back to
    /// a fresh session when the agent no longer holds the recording.
    pub async fn resume_until_ready(
        &self,
        kage_session_id: &str,
        timeout_ms: u64,
    ) -> Result<AgentRecord, String> {
        self.open_session(Some(kage_session_id), timeout_ms).await
    }

    /// The agent's session id, once the handshake has opened one.
    pub fn agent_session_id(&self) -> Option<String> {
        self.session()
    }

    /// Runs the handshake and opens (or reopens) the agent's session.
    async fn open_session(
        &self,
        resume: Option<&str>,
        timeout_ms: u64,
    ) -> Result<AgentRecord, String> {
        let init = self
            .request_frame(initialize_frame(self.take_id()), timeout_ms, "initialize")
            .await?;
        if init.get("error").is_some() {
            return Err(detail_of(&init));
        }
        let cwd = self.inner.cwd.clone();
        let id = self.take_id();
        let (frame, kind) = match resume {
            Some(previous) => (resume_session_frame(id, previous, &cwd), "session/resume"),
            None => (new_session_frame(id, &cwd), "session/new"),
        };
        let mut opened = self.request_frame(frame, timeout_ms, kind).await?;
        if opened.get("error").is_some() && resume.is_some() {
            let retry = self
                .request_frame(
                    new_session_frame(self.take_id(), &cwd),
                    timeout_ms,
                    "session/new",
                )
                .await?;
            if retry.get("error").is_some() {
                return Err(detail_of(&retry));
            }
            opened = retry;
        } else if opened.get("error").is_some() {
            return Err(detail_of(&opened));
        }
        let Some(session_id) = opened
            .get("result")
            .and_then(|result| result.get("sessionId"))
            .and_then(Value::as_str)
        else {
            return Err("the agent opened no session".to_owned());
        };
        let mut state = self.inner.state.lock().expect("the client lock");
        state.session_id = Some(session_id.to_owned());
        if state.lifecycle == AgentState::Starting {
            state.lifecycle = AgentState::Ready;
        }
        Ok(opened)
    }

    /// Sends a prompt, steering the running turn when one exists.
    ///
    /// Steering and queueing are the agent's choice on delivery: `Steer`
    /// joins the running turn, anything else waits behind it.
    pub fn prompt(
        &self,
        message: &str,
        images: Option<Vec<Value>>,
        behavior: Option<StreamingBehavior>,
    ) -> bool {
        let Some(session_id) = self.session() else {
            return false;
        };
        let mut blocks = vec![text_block(message)];
        for image in images.unwrap_or_default() {
            let data = image
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mime_type = image
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream");
            blocks.push(image_block(data, mime_type));
        }
        let delivery = behavior.and_then(StreamingBehavior::delivery);
        let id = self.take_id();
        let frame = prompt_frame(id, &session_id, &blocks, delivery);
        if !self.send(&frame) {
            return false;
        }
        self.inner
            .state
            .lock()
            .expect("the client lock")
            .requests
            .insert(id, Awaited::Prompt);
        true
    }

    /// Redirects the turn that is already running.
    pub fn steer(&self, message: &str, images: Option<Vec<Value>>) -> bool {
        self.prompt(message, images, Some(StreamingBehavior::Steer))
    }

    /// Switches the model the session runs on, from this turn onward.
    ///
    /// The conversation is kept: what was said stays said, and the next turn
    /// is answered by the model named here. That is the point of switching
    /// rather than starting again.
    ///
    /// The id is provider-qualified. The agent matches a model by exactly the
    /// id it lists, so a `:level` written onto the name finds nothing: the
    /// level is [`set_thinking_level`](Self::set_thinking_level), sent after
    /// this.
    ///
    /// Waits for the agent's answer, because it refuses a model it has no key
    /// for, and a switch it refused must not be reported as made.
    pub async fn set_model(
        &self,
        provider: &str,
        model_id: &str,
        timeout_ms: u64,
    ) -> Result<(), String> {
        let Some(session_id) = self.session() else {
            return Err("the agent is not ready".to_owned());
        };
        let value = format!("{provider}/{model_id}");
        let answer = self
            .request_frame(
                set_config_option_frame(self.take_id(), &session_id, "model", &value),
                timeout_ms,
                "session/set_config_option",
            )
            .await?;
        if answer.get("error").is_some() {
            return Err(detail_of(&answer));
        }
        Ok(())
    }

    /// Sets how hard the model thinks, named without its colon.
    ///
    /// Sent after a switch rather than with it: switching already moves the
    /// level to what the new model can do, and this says what was asked for
    /// instead. Nothing is sent when nobody asked, leaving the agent's own
    /// choice for that model alone.
    pub fn set_thinking_level(&self, level: &str) -> bool {
        let Some(session_id) = self.session() else {
            return false;
        };
        let id = self.take_id();
        self.send(&set_config_option_frame(id, &session_id, "thinking", level))
    }

    /// Asks the agent to stop the running turn.
    pub fn abort(&self) -> bool {
        let Some(session_id) = self.session() else {
            return true;
        };
        self.send(&cancel_frame(&session_id))
    }

    /// Asks the agent to summarise the conversation so far.
    ///
    /// Sent as a request rather than fired off, because the useful part is
    /// the answer: how much context it freed, which is the only way to tell a
    /// compaction that did something from one that did not. The counts arrive
    /// on the compaction update ahead of the answer.
    pub async fn compact(&self, timeout_ms: u64) -> Result<AgentRecord, String> {
        let Some(session_id) = self.session() else {
            return Err("the agent is not ready".to_owned());
        };
        let answer = self
            .request_frame(
                compact_frame(self.take_id(), &session_id),
                timeout_ms,
                "_kage/session/compact",
            )
            .await?;
        if answer.get("error").is_some() {
            return Err(detail_of(&answer));
        }
        let counts = self
            .inner
            .state
            .lock()
            .expect("the client lock")
            .pending_compaction
            .take();
        match counts {
            Some((before, after)) => Ok(json!({
                "success": true,
                "data": {
                    "tokensBefore": before,
                    "estimatedTokensAfter": after,
                },
            })),
            None => Ok(json!({ "success": true })),
        }
    }

    /// The dialog currently blocking the agent, when there is one.
    pub fn pending_dialog(&self) -> Option<DialogRequest> {
        let state = self.inner.state.lock().expect("the client lock");
        state
            .dialogs
            .values()
            .next()
            .map(|entry| entry.request.clone())
    }

    /// Offers a reply as the answer to a pending dialog.
    ///
    /// An unrecognized reply leaves the dialog pending, so the caller can
    /// repeat the question rather than sending the agent something it did not
    /// ask for. The chosen name answers with its option id.
    pub fn answer_dialog(&self, id: &str, reply: &str) -> AnswerOutcome {
        let response = {
            let mut state = self.inner.state.lock().expect("the client lock");
            let Some(entry) = state.dialogs.get(id) else {
                return AnswerOutcome::Unknown;
            };
            let Some(built) = build_response(&entry.request, reply) else {
                return AnswerOutcome::Unrecognized;
            };
            let names = entry.request.options.clone().unwrap_or_default();
            let chosen = match &built {
                DialogReply::Value(value) => value.clone(),
                // Permission asks are always selects, so a confirmation
                // cannot name an option; leaving the dialog standing is the
                // honest answer.
                DialogReply::Confirmed(_) => return AnswerOutcome::Unrecognized,
            };
            let option_id = names
                .iter()
                .position(|name| name == &chosen)
                .and_then(|index| entry.option_ids.get(index))
                .cloned();
            let Some(option_id) = option_id else {
                return AnswerOutcome::Unrecognized;
            };
            let ask_id: u64 = id.parse().unwrap_or(0);
            entry.timer.abort();
            state.dialogs.remove(id);
            Some(select_option_frame(ask_id, &option_id))
        };
        let Some(response) = response else {
            return AnswerOutcome::Unknown;
        };
        self.send(&response);
        AnswerOutcome::Accepted
    }

    /// Cancels every pending dialog, so nothing is left waiting when a session
    /// ends or the daemon shuts down.
    pub fn cancel_dialogs(&self) {
        let ids: Vec<(String, u64)> = {
            let mut state = self.inner.state.lock().expect("the client lock");
            let ids: Vec<(String, u64)> = state
                .dialogs
                .keys()
                .filter_map(|id| id.parse::<u64>().ok().map(|ask| (id.clone(), ask)))
                .collect();
            state.dialogs.clear();
            ids
        };
        for (_, ask_id) in ids {
            self.inner.write_frame(&cancel_ask_frame(ask_id));
        }
    }

    /// Sends a frame carrying a fresh id and waits for its answer.
    ///
    /// Used to establish readiness and to run commands whose answer is the
    /// outcome: the agent answers exactly what was asked, which is more
    /// reliable than watching for an event or sleeping.
    async fn request_frame(
        &self,
        frame: Value,
        timeout_ms: u64,
        kind: &str,
    ) -> Result<AgentRecord, String> {
        let Some(id) = frame.get("id").and_then(Value::as_u64) else {
            return Err("a request without an id answers nothing".to_owned());
        };
        let (sender, receiver) = oneshot::channel();
        self.inner
            .state
            .lock()
            .expect("the client lock")
            .requests
            .insert(id, Awaited::Rpc(sender));
        if !self.send(&frame) {
            self.inner
                .state
                .lock()
                .expect("the client lock")
                .requests
                .remove(&id);
            return Err("the agent is not accepting commands".to_owned());
        }

        let outcome =
            tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), receiver).await;
        if let Ok(Ok(record)) = outcome {
            return Ok(record);
        }
        self.inner
            .state
            .lock()
            .expect("the client lock")
            .requests
            .remove(&id);
        Err(format!(
            "the agent did not answer {kind} within {timeout_ms}ms"
        ))
    }

    /// Sends a command carrying a correlation id and waits for its response.
    ///
    /// The id is stamped here, before the waiter is filed, so an answer can
    /// never arrive first.
    #[allow(
        dead_code,
        reason = "driven by this module's tests, which assert on request and answer pairing the daemon never asks for directly"
    )]
    pub async fn request(&self, command: Value, timeout_ms: u64) -> Result<AgentRecord, String> {
        let kind = command
            .get("method")
            .and_then(Value::as_str)
            .or_else(|| command.get("type").and_then(Value::as_str))
            .unwrap_or("command")
            .to_owned();
        let mut frame = command.clone();
        frame["id"] = json!(self.take_id());
        if frame.get("jsonrpc").is_none() {
            frame["jsonrpc"] = json!("2.0");
        }
        self.request_frame(frame, timeout_ms, &kind).await
    }

    fn end(&self, code: i64) {
        let during_turn = {
            let mut state = self.inner.state.lock().expect("the client lock");
            let during_turn = state.lifecycle == AgentState::Working;
            state.lifecycle = AgentState::Ended;
            if state.exit_reported {
                return;
            }
            state.exit_reported = true;
            during_turn
        };
        self.inner
            .fail_pending_requests(&format!("the agent exited with code {code}"));
        self.cancel_dialogs();
        if let Some(on_exit) = &self.inner.handlers.on_exit {
            on_exit((code, during_turn));
        }
    }

    fn send(&self, command: &Value) -> bool {
        let lifecycle = self.inner.state.lock().expect("the client lock").lifecycle;
        if lifecycle == AgentState::Ended {
            self.inner.log.warn(
                "dropped a command for an agent that has ended",
                &fields([(
                    "command",
                    command
                        .get("method")
                        .and_then(Value::as_str)
                        .or_else(|| command.get("type").and_then(Value::as_str))
                        .unwrap_or("")
                        .into(),
                )]),
            );
            return false;
        }
        let mut line = command.to_string();
        self.inner.log.trace(
            "sent to the agent",
            &fields([(
                "type",
                command
                    .get("method")
                    .and_then(Value::as_str)
                    .or_else(|| command.get("type").and_then(Value::as_str))
                    .unwrap_or("")
                    .into(),
            )]),
        );
        self.inner
            .log
            .wire("to the agent", &fields([("line", line.as_str().into())]));
        line.push('\n');
        // Fire and forget: a failed write surfaces as the process ending,
        // which the client already handles.
        match self.inner.process.write(line.as_bytes()) {
            Ok(()) => true,
            Err(error) => {
                self.inner.state.lock().expect("the client lock").lifecycle = AgentState::Ended;
                self.inner.log.warn(
                    "writing to the agent failed",
                    &fields([("detail", error.to_string().into())]),
                );
                false
            }
        }
    }

    /// The agent's session, once the handshake has opened it.
    fn session(&self) -> Option<String> {
        self.inner
            .state
            .lock()
            .expect("the client lock")
            .session_id
            .clone()
    }

    /// The next outbound frame id.
    fn take_id(&self) -> u64 {
        let mut state = self.inner.state.lock().expect("the client lock");
        let id = state.next_request_id;
        state.next_request_id += 1;
        id
    }
}

impl Shared {
    /// Writes one frame to the agent, newline-terminated, outside the
    /// lifecycle gate: answers to the agent's own requests must go out even
    /// while the client is ending.
    fn write_frame(&self, frame: &Value) {
        let mut line = frame.to_string();
        line.push('\n');
        let _ = self.process.write(line.as_bytes());
    }

    async fn read_stdout(&self) {
        let mut buffer = vec![0_u8; 65_536];
        loop {
            let read = match self.process.read_stdout(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let records = {
                let mut state = self.state.lock().expect("the client lock");
                match state.framer.push(&buffer[..read]) {
                    Ok(records) => records,
                    Err(error) => {
                        state.lifecycle = AgentState::Ended;
                        drop(state);
                        if let Some(on_violation) = &self.handlers.on_protocol_violation {
                            on_violation(error.to_string());
                        }
                        return;
                    }
                }
            };
            for record in records {
                self.dispatch(&record);
            }
        }
    }

    async fn read_stderr(&self) {
        let mut buffer = vec![0_u8; 16_384];
        loop {
            let read = match self.process.read_stderr(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(read) => read,
            };
            let text = String::from_utf8_lossy(&buffer[..read])
                .trim_end()
                .to_owned();
            if text.is_empty() {
                continue;
            }
            self.log
                .warn("agent stderr", &fields([("detail", text.clone().into())]));
            let mut state = self.state.lock().expect("the client lock");
            state.last_words = format!("{}\n{text}", state.last_words);
            let kept_from = state
                .last_words
                .char_indices()
                .rev()
                .nth(STDERR_KEPT)
                .map(|(at, _)| at);
            if let Some(at) = kept_from {
                let trimmed = state.last_words[at..].trim_start().to_owned();
                state.last_words = trimmed;
            }
        }
    }

    fn dispatch(&self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        self.log
            .wire("from the agent", &fields([("line", line.into())]));

        let record: AgentRecord = match serde_json::from_str(line) {
            Ok(record) => record,
            Err(error) => {
                self.log.warn(
                    "agent sent an unparseable line",
                    &fields([
                        ("detail", error.to_string().into()),
                        ("length", line.len().into()),
                    ]),
                );
                return;
            }
        };

        self.log.trace(
            "heard from the agent",
            &fields([(
                "type",
                record
                    .get("method")
                    .and_then(Value::as_str)
                    .or_else(|| record.get("type").and_then(Value::as_str))
                    .unwrap_or("")
                    .into(),
            )]),
        );
        match classify_frame(&record) {
            FrameKind::Request { id, method } => self.dispatch_request(id, &method, &record),
            FrameKind::Notification { method } => self.dispatch_notification(&method, &record),
            FrameKind::Success { id } => self.dispatch_success(id, &record),
            FrameKind::Failure { id } => self.dispatch_failure(id, &record),
            FrameKind::Unknown => {}
        }
    }

    /// Answers a server request: a permission ask becomes a dialog, anything
    /// the thread cannot serve is refused at once, and unknown methods are
    /// refused too so the agent never waits on this client.
    fn dispatch_request(&self, id: u64, method: &str, record: &AgentRecord) {
        if method != "session/request_permission" {
            self.write_frame(&method_not_found_frame(id, method));
            return;
        }
        let params = record.get("params").unwrap_or(&Value::Null);
        let Some(dialog) = as_permission_ask(id, params) else {
            self.write_frame(&cancel_ask_frame(id));
            if let Some(on_unsupported) = &self.handlers.on_unsupported_dialog {
                on_unsupported(DialogMethod::Editor.as_str().to_owned());
            }
            return;
        };

        let timer = spawn_dialog_timer(
            Arc::clone(&self.state),
            Arc::clone(&self.handlers),
            Arc::clone(&self.process),
            self.dialog_timeout_ms,
            dialog.clone(),
        );
        self.state.lock().expect("the client lock").dialogs.insert(
            dialog.id.clone(),
            PendingDialog {
                request: dialog.clone(),
                option_ids: ask_option_ids(params),
                timer,
            },
        );
        if let Some(on_dialog) = &self.handlers.on_dialog {
            on_dialog(dialog.clone());
        }
    }

    /// Handles a server notification: turn progress, or an ask withdrawn by
    /// the agent.
    fn dispatch_notification(&self, method: &str, record: &AgentRecord) {
        if method == "$/cancel_request" {
            let request_id = record
                .get("params")
                .and_then(|params| params.get("requestId"))
                .and_then(Value::as_u64)
                .map(|id| id.to_string());
            if let Some(id) = request_id {
                self.withdraw_dialog(&id);
            }
            return;
        }
        if method != "session/update" {
            return;
        }
        let update = record.get("params").and_then(|params| params.get("update"));
        if let Some(update) = update.map(classify_update) {
            self.apply_update(update);
        }
    }

    /// Applies one turn update to the state and the handlers.
    fn apply_update(&self, update: AcpUpdate) {
        match update {
            AcpUpdate::AgentText(text) => self.apply_text(text),
            AcpUpdate::ThoughtText(text) => self.apply_thought(text),
            AcpUpdate::ToolStart {
                id,
                title,
                raw_input,
            } => self.apply_tool_start(&id, &title, raw_input.as_ref()),
            AcpUpdate::ToolEnd {
                id,
                title,
                failed,
                output,
            } => self.apply_tool_end(id, title, failed, output),
            AcpUpdate::UsageInfo { usage, size } => self.apply_usage(usage, size),
            AcpUpdate::TurnStart => self.apply_turn_start(),
            AcpUpdate::TurnEnd => self.settle_turn(),
            AcpUpdate::CompactionInfo { before, after } => {
                self.state
                    .lock()
                    .expect("the client lock")
                    .pending_compaction = Some((before, after));
            }
            AcpUpdate::Notice(text) => {
                if let Some(on_error) = &self.handlers.on_error {
                    on_error(text);
                }
            }
            AcpUpdate::Ignored => {}
        }
    }

    /// Reports assistant text as it arrives, noting the turn said something.
    fn apply_text(&self, text: String) {
        if !text.trim().is_empty() {
            self.state.lock().expect("the client lock").produced_text = true;
        }
        if !text.is_empty()
            && let Some(on_text) = &self.handlers.on_assistant_text
        {
            on_text(text);
        }
    }

    /// Reports thinking once per turn, then each thought as it arrives.
    fn apply_thought(&self, text: String) {
        let report = {
            let mut state = self.state.lock().expect("the client lock");
            if state.thinking_reported {
                false
            } else {
                state.thinking_reported = true;
                true
            }
        };
        if report && let Some(on_thinking) = &self.handlers.on_thinking {
            on_thinking(());
        }
        if !text.trim().is_empty()
            && let Some(on_thought) = &self.handlers.on_thought
        {
            on_thought(text);
        }
    }

    /// Reports a tool call start, remembering its title for the end.
    fn apply_tool_start(&self, id: &str, title: &str, raw_input: Option<&Value>) {
        self.state
            .lock()
            .expect("the client lock")
            .tool_titles
            .insert(id.to_owned(), title.to_owned());
        if let Some(on_tool_start) = &self.handlers.on_tool_start {
            on_tool_start((id.to_owned(), title.to_owned(), tool_target(raw_input)));
        }
    }

    /// Reports a tool call end under its remembered title.
    fn apply_tool_end(&self, id: String, title: Option<String>, failed: bool, output: String) {
        let held = {
            let mut state = self.state.lock().expect("the client lock");
            state.tool_titles.remove(&id)
        };
        if let Some(on_tool_end) = &self.handlers.on_tool_end {
            on_tool_end((
                id,
                held.or(title).unwrap_or_else(|| "tool".to_owned()),
                failed,
                output,
            ));
        }
    }

    /// Reports usage and the window size it arrived with.
    fn apply_usage(&self, usage: Usage, size: f64) {
        if size > 0.0 {
            self.inner_context_window(size);
        }
        if let Some(on_usage) = &self.handlers.on_usage {
            on_usage(usage);
        }
    }

    /// Opens a turn, clearing the last one's outcome.
    fn apply_turn_start(&self) {
        let mut state = self.state.lock().expect("the client lock");
        state.lifecycle = AgentState::Working;
        state.thinking_reported = false;
        state.produced_text = false;
        state.turn_done = false;
        state.turn_failure = None;
        drop(state);
        if let Some(on_turn_start) = &self.handlers.on_turn_start {
            on_turn_start(());
        }
    }

    /// Records the window size the usage update last reported.
    fn inner_context_window(&self, size: f64) {
        self.state.lock().expect("the client lock").context_window = Some(size);
    }

    /// Settles the running turn the way the turn end means it: once, so a
    /// failure answer and a late turn end cannot settle it twice.
    fn settle_turn(&self) {
        let (produced, failure) = {
            let mut state = self.state.lock().expect("the client lock");
            if state.turn_done {
                return;
            }
            state.turn_done = true;
            if state.lifecycle == AgentState::Working {
                state.lifecycle = AgentState::Ready;
            }
            let produced = state.produced_text;
            let failure = state.turn_failure.take();
            state.produced_text = false;
            (produced, failure)
        };
        if let Some(on_settled) = &self.handlers.on_turn_settled {
            on_settled((produced, failure));
        }
    }

    /// Drops a dialog the agent withdrew, telling the caller it timed out so
    /// nothing is left holding a question with no answer coming.
    fn withdraw_dialog(&self, id: &str) {
        let request = {
            let mut state = self.state.lock().expect("the client lock");
            let Some(entry) = state.dialogs.remove(id) else {
                return;
            };
            entry.timer.abort();
            entry.request
        };
        if let Some(on_timeout) = &self.handlers.on_dialog_timeout {
            on_timeout(request);
        }
    }

    /// Routes an answer to the request that waits on it, or records what a
    /// prompt's answer says about the turn it ran.
    fn dispatch_success(&self, id: u64, record: &AgentRecord) {
        let waiting = self
            .state
            .lock()
            .expect("the client lock")
            .requests
            .remove(&id);
        match waiting {
            Some(Awaited::Rpc(reply)) => {
                let _ = reply.send(record.clone());
            }
            Some(Awaited::Prompt) => {
                let result = record.get("result").unwrap_or(&Value::Null);
                self.record_turn_failure(stop_failure(result));
            }
            None => {}
        }
    }

    /// Routes a refusal: to the waiter when one exists; for a prompt, the run
    /// is over, so a busy provider both backs the admission off and fails
    /// the turn, while anything else is the whole outcome of the command and
    /// the session settles it. A refusal naming nothing waiting is reported
    /// the same way, since it answers nothing that can still use it. Every
    /// prompt path marks the turn done, so a late turn end cannot settle it
    /// again.
    fn dispatch_failure(&self, id: u64, record: &AgentRecord) {
        let waiting = self
            .state
            .lock()
            .expect("the client lock")
            .requests
            .remove(&id);
        match waiting {
            Some(Awaited::Rpc(reply)) => {
                let _ = reply.send(record.clone());
            }
            Some(Awaited::Prompt) => {
                let detail = detail_of(record);
                if is_retryable(&detail) {
                    if let Some(on_retry) = &self.handlers.on_retry {
                        on_retry(detail.clone());
                    }
                    self.record_turn_failure(Some(detail));
                    self.settle_turn();
                } else {
                    self.suppress_turn_end();
                    if let Some(on_rejected) = &self.handlers.on_command_rejected {
                        on_rejected(("prompt".to_owned(), detail));
                    }
                }
            }
            None => {
                if let Some(on_rejected) = &self.handlers.on_command_rejected {
                    on_rejected(("command".to_owned(), detail_of(record)));
                }
            }
        }
    }

    /// Marks the turn done without settling it: the session settles a
    /// refused command itself, and a later turn end must not settle it again.
    fn suppress_turn_end(&self) {
        self.state.lock().expect("the client lock").turn_done = true;
    }

    /// Records what the prompt's answer says about the turn it ran.
    fn record_turn_failure(&self, failure: Option<String>) {
        self.state.lock().expect("the client lock").turn_failure = failure;
    }

    fn fail_pending_requests(&self, reason: &str) {
        let waiting: Vec<oneshot::Sender<AgentRecord>> = {
            let mut state = self.state.lock().expect("the client lock");
            let ids: Vec<u64> = state.requests.keys().copied().collect();
            ids.iter()
                .filter_map(|id| match state.requests.remove(id) {
                    Some(Awaited::Rpc(reply)) => Some(reply),
                    Some(Awaited::Prompt) | None => None,
                })
                .collect()
        };
        for reply in waiting {
            let _ = reply.send(json!({ "failed": reason }));
        }
    }
}

/// Ends a dialog nobody answered: cancelled with the agent, and the caller
/// told. An aborted task never reaches this, which is what answering does.
pub(crate) fn spawn_dialog_timer(
    state: Arc<Mutex<ClientState>>,
    handlers: Arc<AgentHandlers>,
    process: Arc<dyn AgentProcess>,
    timeout_ms: u64,
    request: DialogRequest,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_dialog_timer(
        state, handlers, process, timeout_ms, request,
    ))
}

/// Ends a dialog nobody answered.
async fn run_dialog_timer(
    state: Arc<Mutex<ClientState>>,
    handlers: Arc<AgentHandlers>,
    process: Arc<dyn AgentProcess>,
    timeout_ms: u64,
    request: DialogRequest,
) {
    tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
    let was_pending = state
        .lock()
        .expect("the client lock")
        .dialogs
        .remove(&request.id)
        .is_some();
    if !was_pending {
        return;
    }
    let ask_id: u64 = request.id.parse().unwrap_or(0);
    let mut cancelled = cancel_ask_frame(ask_id).to_string();
    cancelled.push('\n');
    let _ = process.write(cancelled.as_bytes());
    if let Some(on_timeout) = &handlers.on_dialog_timeout {
        on_timeout(request);
    }
}

#[cfg(test)]
mod tests;
