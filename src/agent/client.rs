//! Drives one agent process: commands out, events in.
//!
//! The client owns the protocol only. It knows nothing about chat, threads, or
//! admission, so it can be exercised against a fake process in tests.
//!
//! Turn completion is taken from `agent_settled`, not from `agent_end`. An
//! `agent_end` may be followed by an automatic retry, so releasing an
//! admission slot on it would let more work into the provider than the cap
//! allows.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::agent::framing::LineFramer;
use crate::agent::protocol::{
    AgentRecord, DialogMethod, DialogRequest, StreamingBehavior, Usage, as_dialog_request,
    is_fire_and_forget, message_role, message_text, starts_thinking, thinking_ended, tool_target,
    usage_of,
};
use crate::log::Logger;

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
#[allow(
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
    /// What the agent reasoned, once a block of it finished.
    pub on_thought: Callback<String>,
    /// What a finished turn cost, when the agent reported it.
    pub on_usage: Callback<Usage>,
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
    /// A confirmation, or its refusal.
    Confirmed(bool),
}

/// A question the agent is blocked on, and the timer that gives up on it.
pub(crate) struct PendingDialog {
    /// What was asked, so an answer can be matched to it.
    pub(crate) request: DialogRequest,
    /// Gives up on the question when nobody answers in time.
    pub(crate) timer: tokio::task::JoinHandle<()>,
}

/// The client's mutable state, shared between the caller and the readers.
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
    /// Why this turn failed, when it did.
    pub(crate) turn_failure: Option<String>,
    /// How much context the model holds, once the agent has said.
    pub(crate) context_window: Option<f64>,
    /// Questions the agent is blocked on, by correlation id.
    pub(crate) dialogs: BTreeMap<String, PendingDialog>,
    /// Commands waiting on a response, by correlation id.
    pub(crate) requests: BTreeMap<String, oneshot::Sender<AgentRecord>>,
    /// Counter behind the correlation ids this client issues.
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

/// Why an assistant message carries no words, when the agent says it failed.
///
/// The agent records a stop reason on the message and an error beside it. A
/// turn that never reached the provider has neither text nor usage, which on
/// its own looks the same as a turn that had nothing to add.
fn message_failure(message: Option<&Value>) -> Option<String> {
    let message = message?;
    if message.get("stopReason").and_then(Value::as_str) != Some("error") {
        return None;
    }
    match message.get("error") {
        Some(Value::String(error)) if !error.trim().is_empty() => Some(error.trim().to_owned()),
        Some(error @ Value::Object(_)) => {
            let from_field = error
                .get("message")
                .and_then(Value::as_str)
                .filter(|detail| !detail.trim().is_empty())
                .map(str::trim)
                .map(str::to_owned);
            Some(from_field.unwrap_or_else(|| "the model provider did not answer".to_owned()))
        }
        _ => Some("the model provider did not answer".to_owned()),
    }
}

fn detail_of(record: &Value) -> String {
    for key in ["error", "message", "reason"] {
        if let Some(Value::String(detail)) = record.get(key)
            && !detail.is_empty()
        {
            return detail.clone();
        }
    }
    record
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned()
}

/// The client's mutable state, shared between the caller and the readers.
struct Shared {
    process: Arc<dyn AgentProcess>,
    state: Arc<Mutex<ClientState>>,
    handlers: Arc<AgentHandlers>,
    log: Logger,
    dialog_timeout_ms: u64,
}

/// Speaks the agent protocol over one process's pipes.
#[derive(Clone)]
pub struct AgentClient {
    inner: Arc<Shared>,
}

impl AgentClient {
    /// A client over `process`, reporting through `handlers`.
    pub fn new(
        process: Arc<dyn AgentProcess>,
        handlers: AgentHandlers,
        log: Logger,
        dialog_timeout_ms: u64,
        max_record_bytes: Option<usize>,
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
                    turn_failure: None,
                    context_window: None,
                    dialogs: BTreeMap::new(),
                    requests: BTreeMap::new(),
                    next_request_id: 1,
                })),
                handlers: Arc::new(handlers),
                log,
                dialog_timeout_ms,
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
        };
        let stderr = tokio::spawn(async move { error_reader.read_stderr().await });
        self.inner.read_stdout().await;
        let _ = stderr.await;
        let code = self.inner.process.exited().await.unwrap_or(-1);
        self.end(i64::from(code));
    }

    /// Waits until the agent answers, which is what readiness means here.
    pub async fn wait_until_ready(&self, timeout_ms: u64) -> Result<AgentRecord, String> {
        let answer = self
            .request(json!({ "type": "get_state" }), timeout_ms)
            .await?;
        let window = answer
            .get("data")
            .and_then(|data| data.get("model"))
            .and_then(|model| model.get("contextWindow"))
            .and_then(Value::as_f64);
        if window.is_some_and(|window| window > 0.0) {
            self.inner
                .state
                .lock()
                .expect("the client lock")
                .context_window = window;
        }
        let mut state = self.inner.state.lock().expect("the client lock");
        if state.lifecycle == AgentState::Starting {
            state.lifecycle = AgentState::Ready;
        }
        Ok(answer)
    }

    /// Sends a prompt, queueing it behind a running turn when one exists.
    pub fn prompt(
        &self,
        message: &str,
        images: Option<Vec<Value>>,
        behavior: Option<StreamingBehavior>,
    ) -> bool {
        let mut command = json!({ "type": "prompt", "message": message });
        if let Some(images) = images {
            command["images"] = Value::Array(images);
        }
        if let Some(behavior) = behavior {
            command["streamingBehavior"] = json!(behavior.as_wire());
        }
        self.send(&command)
    }

    /// Redirects the turn that is already running.
    pub fn steer(&self, message: &str, images: Option<Vec<Value>>) -> bool {
        let mut command = json!({ "type": "steer", "message": message });
        if let Some(images) = images {
            command["images"] = Value::Array(images);
        }
        self.send(&command)
    }

    /// Switches the model the session runs on, from this turn onward.
    ///
    /// The conversation is kept: what was said stays said, and the next turn
    /// is answered by the model named here. That is the point of switching
    /// rather than starting again.
    pub fn set_model(&self, provider: &str, model_id: &str) -> bool {
        self.send(&json!({ "type": "set_model", "provider": provider, "modelId": model_id }))
    }

    /// Asks the agent to stop the running turn.
    pub fn abort(&self) -> bool {
        self.send(&json!({ "type": "abort" }))
    }

    /// Asks the agent to summarise the conversation so far.
    ///
    /// Sent as a request rather than fired off, because the useful part is the
    /// answer: how much context it freed, which is the only way to tell a
    /// compaction that did something from one that did not.
    pub async fn compact(&self, timeout_ms: u64) -> Result<AgentRecord, String> {
        self.request(json!({ "type": "compact" }), timeout_ms).await
    }

    /// Answers a dialog the agent is blocked on.
    pub fn respond_to_dialog(&self, response: &Value) -> bool {
        self.send(response)
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
    /// ask for.
    pub fn answer_dialog(&self, id: &str, reply: &str) -> AnswerOutcome {
        let response = {
            let mut state = self.inner.state.lock().expect("the client lock");
            let Some(entry) = state.dialogs.get(id) else {
                return AnswerOutcome::Unknown;
            };
            let Some(built) = build_response(&entry.request, reply) else {
                return AnswerOutcome::Unrecognized;
            };
            entry.timer.abort();
            state.dialogs.remove(id);
            Some(built)
        };
        let Some(response) = response else {
            return AnswerOutcome::Unknown;
        };
        let answer = match response {
            DialogReply::Value(value) => json!({
                "type": "extension_ui_response",
                "id": id,
                "value": value,
            }),
            DialogReply::Confirmed(confirmed) => json!({
                "type": "extension_ui_response",
                "id": id,
                "confirmed": confirmed,
            }),
        };
        self.send(&answer);
        AnswerOutcome::Accepted
    }

    /// Cancels every pending dialog, so nothing is left waiting when a session
    /// ends or the daemon shuts down.
    pub fn cancel_dialogs(&self) {
        let ids: Vec<String> = {
            let mut state = self.inner.state.lock().expect("the client lock");
            let ids: Vec<String> = state.dialogs.keys().cloned().collect();
            state.dialogs.clear();
            ids
        };
        for id in ids {
            self.respond_to_dialog(&json!({
                "type": "extension_ui_response",
                "id": id,
                "cancelled": true,
            }));
        }
    }

    /// Sends a command carrying a correlation id and waits for its response.
    ///
    /// Used to establish readiness: the agent is ready exactly when it
    /// answers, which is more reliable than watching for an event or sleeping.
    pub async fn request(&self, command: Value, timeout_ms: u64) -> Result<AgentRecord, String> {
        let (sender, receiver) = oneshot::channel();
        let id = {
            let mut state = self.inner.state.lock().expect("the client lock");
            let id = format!("rq-{}", state.next_request_id);
            state.next_request_id += 1;
            state.requests.insert(id.clone(), sender);
            id
        };

        let mut sent = command.clone();
        sent["id"] = json!(id);
        if !self.send(&sent) {
            self.inner
                .state
                .lock()
                .expect("the client lock")
                .requests
                .remove(&id);
            return Err("the agent is not accepting commands".to_owned());
        }

        let kind = command
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("command")
            .to_owned();
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
                &crate::log::fields([(
                    "command",
                    command
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                )]),
            );
            return false;
        }
        let mut line = command.to_string();
        line.push('\n');
        // Fire and forget: a failed write surfaces as the process ending,
        // which the client already handles.
        match self.inner.process.write(line.as_bytes()) {
            Ok(()) => true,
            Err(error) => {
                self.inner.state.lock().expect("the client lock").lifecycle = AgentState::Ended;
                self.inner.log.warn(
                    "writing to the agent failed",
                    &crate::log::fields([("detail", error.to_string().into())]),
                );
                false
            }
        }
    }
}

impl Shared {
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
            self.log.warn(
                "agent stderr",
                &crate::log::fields([("detail", text.clone().into())]),
            );
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

        let record: AgentRecord = match serde_json::from_str(line) {
            Ok(record) => record,
            Err(error) => {
                self.log.warn(
                    "agent sent an unparseable line",
                    &crate::log::fields([
                        ("detail", error.to_string().into()),
                        ("length", line.len().into()),
                    ]),
                );
                return;
            }
        };

        if self.dispatch_dialog(&record) {
            return;
        }
        if self.dispatch_response(&record) {
            return;
        }
        self.dispatch_event(&record);
    }

    /// Returns true when the record was a dialog and has been handled.
    fn dispatch_dialog(&self, record: &AgentRecord) -> bool {
        let Some(dialog) = as_dialog_request(record) else {
            return record.get("type").and_then(Value::as_str) == Some("extension_ui_request")
                && is_fire_and_forget(record.get("method").and_then(Value::as_str));
        };

        if dialog.method == DialogMethod::Editor {
            if let Some(on_unsupported) = &self.handlers.on_unsupported_dialog {
                on_unsupported(dialog.method.as_str().to_owned());
            }
            let cancelled = json!({
                "type": "extension_ui_response",
                "id": dialog.id,
                "cancelled": true,
            });
            let _ = self.process.write(cancelled.to_string().as_bytes());
            return true;
        }

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
                timer,
            },
        );
        if let Some(on_dialog) = &self.handlers.on_dialog {
            on_dialog(dialog.clone());
        }
        true
    }

    /// Returns true when the record was a response and has been handled.
    fn dispatch_response(&self, record: &AgentRecord) -> bool {
        if record.get("type").and_then(Value::as_str) != Some("response") {
            return false;
        }

        let id = record.get("id").and_then(Value::as_str).map(str::to_owned);
        if let Some(id) = &id {
            let waiting = self
                .state
                .lock()
                .expect("the client lock")
                .requests
                .remove(id);
            if let Some(reply) = waiting {
                let _ = reply.send(record.clone());
                return true;
            }
        }

        // A command sent without a correlation id, such as a prompt, still
        // gets a response, and a rejection there is the whole outcome of that
        // command. Dropping it leaves the thread waiting on a turn that will
        // never run.
        if record.get("success") == Some(&Value::Bool(false))
            && let Some(on_rejected) = &self.handlers.on_command_rejected
        {
            on_rejected((
                record
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("command")
                    .to_owned(),
                detail_of(record),
            ));
        }
        true
    }

    // One table of the protocol's event kinds; splitting it would scatter
    // the dispatch the tests read as a whole.
    #[allow(clippy::too_many_lines)]
    fn dispatch_event(&self, record: &AgentRecord) {
        let kind = record.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "agent_start" => {
                let mut state = self.state.lock().expect("the client lock");
                state.lifecycle = AgentState::Working;
                state.thinking_reported = false;
                state.produced_text = false;
                state.turn_failure = None;
                drop(state);
                if let Some(on_turn_start) = &self.handlers.on_turn_start {
                    on_turn_start(());
                }
            }

            "turn_end" => {
                if let Some(usage) = usage_of(record)
                    && let Some(on_usage) = &self.handlers.on_usage
                {
                    on_usage(usage);
                }
            }

            "agent_settled" => {
                let (produced, failure) = {
                    let mut state = self.state.lock().expect("the client lock");
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

            "message_update" => {
                let reported = {
                    let mut state = self.state.lock().expect("the client lock");
                    if !state.thinking_reported && starts_thinking(record) {
                        state.thinking_reported = true;
                        true
                    } else {
                        false
                    }
                };
                if reported {
                    if let Some(on_thinking) = &self.handlers.on_thinking {
                        on_thinking(());
                    }
                    return;
                }
                if let Some(thought) = thinking_ended(record)
                    && !thought.trim().is_empty()
                    && let Some(on_thought) = &self.handlers.on_thought
                {
                    on_thought(thought);
                }
            }

            "message_end" => {
                // Only the assistant's own words. The agent also emits
                // message_end for the user's message, and collecting that
                // would echo the prompt back into the thread it came from.
                if message_role(record.get("message")) != Some("assistant".to_owned()) {
                    return;
                }
                // Read before the empty check below drops the message: a turn
                // that ended in an error arrives with nothing in it, and is
                // otherwise indistinguishable from a model that chose to say
                // nothing.
                let failure = message_failure(record.get("message"));
                if let Some(failure) = failure {
                    self.state.lock().expect("the client lock").turn_failure = Some(failure);
                }
                let text = message_text(record.get("message"));
                let text = text.trim().to_owned();
                if text.is_empty() {
                    return;
                }
                // Reported now rather than accumulated: an agent that speaks,
                // runs a tool, then speaks again must read in that order.
                self.state.lock().expect("the client lock").produced_text = true;
                if let Some(on_text) = &self.handlers.on_assistant_text {
                    on_text(text);
                }
            }

            "tool_execution_start" => {
                if let Some(on_tool_start) = &self.handlers.on_tool_start {
                    on_tool_start((
                        record
                            .get("toolCallId")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        record
                            .get("toolName")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_owned(),
                        tool_target(record.get("args")),
                    ));
                }
            }

            "tool_execution_end" => {
                if let Some(on_tool_end) = &self.handlers.on_tool_end {
                    on_tool_end((
                        record
                            .get("toolCallId")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        record
                            .get("toolName")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_owned(),
                        record.get("isError") == Some(&Value::Bool(true)),
                        // The agent reports a tool's output under `result`, as
                        // content parts. Reading the record itself finds
                        // nothing and silently yields an empty output.
                        format!(
                            "{}{}",
                            message_text(record.get("result")),
                            message_text(Some(record))
                        ),
                    ));
                }
            }

            "auto_retry_start" => {
                if let Some(on_retry) = &self.handlers.on_retry {
                    on_retry(detail_of(record));
                }
            }

            "extension_error" => {
                if let Some(on_error) = &self.handlers.on_error {
                    on_error(detail_of(record));
                }
            }

            _ => {}
        }
    }

    fn fail_pending_requests(&self, reason: &str) {
        let waiting: Vec<oneshot::Sender<AgentRecord>> = {
            let mut state = self.state.lock().expect("the client lock");
            let ids: Vec<String> = state.requests.keys().cloned().collect();
            ids.iter()
                .filter_map(|id| state.requests.remove(id))
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
    let cancelled = json!({
        "type": "extension_ui_response",
        "id": request.id,
        "cancelled": true,
    });
    let _ = process.write(cancelled.to_string().as_bytes());
    if let Some(on_timeout) = &handlers.on_dialog_timeout {
        on_timeout(request);
    }
}

#[cfg(test)]
mod tests;
