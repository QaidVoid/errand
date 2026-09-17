//! One live session: a sandboxed agent, a project, and the surfaces watching.
//!
//! Everything a session does passes through here, which is why so little of it
//! is decided here. What may be typed and by whom is in `commands`, where a
//! path may point is in `sandbox::paths`, what a message looks like is in
//! `chat::render`. This is the lifecycle: start it, route what arrives, take a
//! turn, and tear it down exactly once.
//!
//! The session owns its state outright in one task, reached only through a
//! command channel; agent callbacks and scheduler wake-ups are signals on the
//! same channel, so nothing but the task itself touches the state.

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::os::unix::fs::OpenOptionsExt;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::admission::scheduler::{QueueEntry, Scheduler, SubmitOutcome, Ticket};
use crate::agent::client::{AgentClient, AgentHandlers, AgentProcess};
use crate::agent::delegate::{DelegationOutcome, TurnDelegations};
use crate::agent::protocol::{AgentImage, DialogRequest, StreamingBehavior, Usage};
use crate::agent::requests::{DELEGATE_COMMAND, delegate_command_contents, delegate_instructions};
use crate::chat::diff::file_diff;
use crate::chat::render::{
    bytes as byte_count, compaction_line, connection_line, dialog_lines, directory_listing,
    file_view, marker, question_line, tool_line, truncate, usage_summary, warning_line,
};
use crate::config::schema::{Config, GithubConfig};
use crate::config::size::parse_size;
use crate::log::{LogValue, Logger, fields};
use crate::memory::store::{DEFAULT_MEMORY_BUDGET, MemoryStore, memory_instructions, parse_notes};
use crate::provider::ask::{Endpoint, HttpSender};
use crate::sandbox::backend::{SandboxLaunch, SandboxLaunchError};
use crate::sandbox::paths;
use crate::session::attachments::{self, RawAttachment, is_image, receive};
use crate::session::commands::{
    ASIDE, asks_for_pull_request, help_text, is_addressed_to_bot, is_aside, is_command, may_run,
    parse_user_id,
};
use crate::session::delegating::{Delegating, POLL_MS, Reported};
use crate::session::disk::{MIN_CHECK_MS, next_check_ms, tree_bytes, verdict};
use crate::session::event::{
    Delegated, EndReason, NoticeLevel, ReactionOutcome, SessionEvent, SessionUsage, ToolActivity,
    ToolResult,
};
use crate::session::files::{NotAFileError, read_directory, read_file_for_display};
use crate::session::github::{
    ASKED_FILENAME, GH_SHIM_FILENAME, GITCONFIG_FILENAME, REQUEST_FILENAME, SessionLinks,
    TOKEN_VARIABLE, gh_shim_contents, git_config_contents, git_identity_env, review_instructions,
    thread_link, transcript_link,
};
use crate::session::model::{ChosenModel, expand_alias};
use crate::session::pr::{self, PullRequestError};
use crate::session::projects::ProjectSelection;
use crate::session::record::{
    prepare_record_dir, record_dir, withdraw_from_agent_session, withdraw_from_record,
};
use crate::session::redacted::Redacting;
use crate::session::rules::rules_block;

/// A message as a session sees it, whatever surface it arrived from.
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub id: String,
    pub author_id: String,
    /// Display name, when the service gave one.
    pub author_name: Option<String>,
    pub content: String,
    pub attachments: Vec<RawAttachment>,
}

/// Which of the session's own timers fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTimer {
    /// Nothing happened for the configured idle span.
    Idle,
    /// The disk budget is due its next measurement.
    Disk,
    /// An interruption's deadline came due.
    AbortDeadline,
    /// The delegation exchange directory is due a look.
    Delegating,
}

/// Sets the session's timers, so a test fires deadlines by decision.
pub trait Timers: Send + Sync {
    fn set_timeout(&self, action: SessionTimer, ms: u64) -> u64;
    fn clear_timeout(&self, handle: u64);
}

/// The real timers, firing back into the session's channel.
///
/// A cleared timer's sleep still runs out, so each one checks whether it was
/// cleared before it fires.
pub struct SystemTimers {
    sink: mpsc::Sender<Signal>,
    cleared: Arc<Mutex<HashSet<u64>>>,
}

impl SystemTimers {
    /// Timers that fire into a session's command channel.
    fn new(sink: mpsc::Sender<Signal>) -> Self {
        Self {
            sink,
            cleared: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

impl Timers for SystemTimers {
    fn set_timeout(&self, action: SessionTimer, ms: u64) -> u64 {
        let sink = self.sink.clone();
        let cleared = Arc::clone(&self.cleared);
        let handle = next_timer_handle();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            if cleared
                .lock()
                .expect("the cleared timer set")
                .contains(&handle)
            {
                return;
            }
            let _ = sink
                .send(Signal::Timer {
                    timer: action,
                    handle,
                })
                .await;
        });
        handle
    }

    fn clear_timeout(&self, handle: u64) {
        self.cleared
            .lock()
            .expect("the cleared timer set")
            .insert(handle);
    }
}

fn next_timer_handle() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Endings a thread is not told about.
///
/// None of them is a failure and none of them is final: the next message
/// picks the session up, and that session says so when it starts.
fn is_quiet_ending(why: EndReason) -> bool {
    matches!(
        why,
        EndReason::Idle | EndReason::Shutdown | EndReason::ThreadArchived
    )
}

/// Tools whose effect is worth showing as a diff.
const EDITING_TOOLS: [&str; 5] = ["edit", "write", "create", "str_replace", "multi_edit"];

/// Largest file uploaded on request. The service refuses much more than this.
const MAX_UPLOAD_BYTES: u64 = 8 * 1024 * 1024;

/// Largest file diffed. Beyond this the change is summarised, not shown.
const MAX_DIFFABLE_BYTES: u64 = 512 * 1024;

/// Tool outputs kept so a delegation can name one, newest first.
const MAX_REMEMBERED_OUTPUTS: usize = 50;

/// Longest file read inline, matching the display reader's own limit.
const MAX_INLINE: u64 = 12 * 1024;

/// The first line of something said, cut for a thread that wants one line.
fn first_line(text: &str) -> String {
    let line = text.split('\n').next().unwrap_or_default();
    if line.chars().count() > 80 {
        let cut: String = line.chars().take(80).collect();
        format!("{cut}...")
    } else {
        line.to_owned()
    }
}

/// Encodes bytes as base64, as the agent protocol carries an image.
fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let block = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let combined =
            (u32::from(block[0]) << 16) | (u32::from(block[1]) << 8) | u32::from(block[2]);
        out.push(ALPHABET[(combined >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(combined >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(combined >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[combined as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

/// Called when the guest list changes, so it can be persisted.
pub type OnGuestsChanged = Arc<dyn Fn(&[String]) + Send + Sync>;

/// Told when the session moves to another model.
pub type OnModelChanged = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// Why a command or a fetch failed, in words worth posting.
fn reason(error: &dyn std::fmt::Display) -> String {
    error.to_string()
}

/// Maps an agent-visible path to its host path.
pub type HostPathOf = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Stops a sandbox, ending everything the agent started.
pub type StopBox = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// What a session needs from the sandbox it runs in.
pub struct RunningBox {
    /// The agent process, driven over the protocol.
    pub process: Arc<dyn AgentProcess>,
    pub to_host_path: HostPathOf,
    pub stop: StopBox,
}

/// Starts one sandbox. The daemon adapts its backend enum into this.
pub type Launcher = Arc<
    dyn Fn(
            SandboxLaunch,
        ) -> Pin<Box<dyn Future<Output = Result<RunningBox, SandboxLaunchError>> + Send>>
        + Send
        + Sync,
>;

/// Opens a pull request. Injected so a session can be driven without one.
pub type OpenPullRequest = Arc<
    dyn Fn(pr::Request) -> Pin<Box<dyn Future<Output = Result<String, PullRequestError>> + Send>>
        + Send
        + Sync,
>;

/// Why a prompt cannot run yet, or nothing when it can.
pub type Unavailable =
    Arc<dyn Fn(&str) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;

/// One attachment fetch, as a boxed future.
pub type FetchBox =
    Pin<Box<dyn Future<Output = Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>> + Send>>;

/// Fetches an attachment. Injected so tests need no network.
pub type FetchAttachment = Arc<dyn Fn(String) -> FetchBox + Send + Sync>;

/// Describes an attached image, for a session whose model cannot see one.
pub type DescribeImages = Arc<
    dyn Fn(Vec<AgentImage>, String) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
        + Send
        + Sync,
>;

/// Everything the manager gives a session at construction.
pub struct SessionOptions {
    pub id: String,
    pub project: ProjectSelection,
    /// Provider and model this session runs on, when the opening message named
    /// them. Absent leaves the configured pair, which is the ordinary case.
    pub chosen: Option<ChosenModel>,
    pub state_dir: String,
    /// What the session reports through, with secrets scrubbed at it.
    pub views: Arc<Redacting>,
    pub launcher: Launcher,
    pub scheduler: Arc<Scheduler>,
    pub config: Config,
    pub log: Logger,
    /// Injected by tests; the session builds real timers when none is
    /// given.
    pub timers: Option<Arc<dyn Timers>>,
    /// The account that started this session and may control it.
    pub owner_id: String,
    /// The owner's display name, when the service gave one.
    pub owner_name: Option<String>,
    /// The turn a resumed session carries on from.
    pub start_turn: Option<u32>,
    pub open_pull_request: Option<OpenPullRequest>,
    /// The thread this session runs in, for linking back to the conversation.
    pub thread_id: Option<String>,
    /// The guild the thread is in, which a thread link needs.
    pub guild_id: Option<String>,
    /// Where the interface is published, when it is.
    pub public_url: Option<String>,
    /// Models this host knows this provider serves, for switching between
    /// them.
    pub available_models: Vec<String>,
    /// Where the provider is reached for a delegated question.
    pub delegate_base_url: Option<String>,
    pub unavailable: Option<Unavailable>,
    /// Accounts that may control any session, not only their own.
    pub operator_ids: Vec<String>,
    /// Accounts the owner has invited to take part in this thread.
    pub guest_ids: Vec<String>,
    /// Called when the guest list changes, so it can be persisted.
    pub on_guests_changed: Option<OnGuestsChanged>,
    /// Told when the session moves to another model, so a restart puts it back
    /// on the one it was working with rather than the configured one.
    pub on_model_changed: Option<OnModelChanged>,
    /// Memory, or none when it is switched off.
    pub memory: Option<Arc<MemoryStore>>,
    pub fetch_attachment: Option<FetchAttachment>,
    pub describe_images: Option<DescribeImages>,
    /// Continue the agent conversation already stored in the state directory.
    pub resume: bool,
    /// Called once the session is finished with, so the manager can forget it.
    pub on_ended: Arc<dyn Fn(EndReason) + Send + Sync>,
}

/// What the session's own delegation sources read.
struct SessionSources {
    project_root: String,
    outputs: Arc<Mutex<Vec<(String, String)>>>,
    project_path: String,
}

impl crate::agent::delegation::Sources for SessionSources {
    fn project_root(&self) -> &str {
        &self.project_root
    }

    // The trait's signature is async; reading a small file needs no await.
    #[allow(clippy::unused_async_trait_impl)]
    async fn read_file(&self, path: &str) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn output_of(&self, call_id: &str) -> Option<String> {
        self.outputs
            .lock()
            .expect("the tool output list")
            .iter()
            .rev()
            .find(|(id, _)| id == call_id)
            .map(|(_, text)| text.clone())
    }

    fn attachment(&self, name: &str) -> Option<String> {
        // Held to the same containment as everything else, so a name that
        // climbs out of the attachments directory reads nothing.
        let path = paths::host_path_under(
            &self.project_path,
            &self.project_path,
            &format!("{}/{}", attachments::ATTACHMENTS_DIR, name),
        )?;
        std::fs::read_to_string(path).ok()
    }
}

/// One thing another task or timer wants the session to do or know.
enum Signal {
    Start {
        first: IncomingMessage,
        reply: oneshot::Sender<bool>,
    },
    Handle {
        message: IncomingMessage,
        reply: oneshot::Sender<()>,
    },
    Stop {
        reason: EndReason,
        reply: oneshot::Sender<()>,
    },
    Withdraw {
        message_id: String,
        reply: oneshot::Sender<bool>,
    },
    Guests {
        reply: oneshot::Sender<Vec<String>>,
    },
    Ended {
        reply: oneshot::Sender<bool>,
    },
    LastActive {
        reply: oneshot::Sender<i64>,
    },
    Timer {
        timer: SessionTimer,
        handle: u64,
    },
    /// A prompt the queue admitted.
    Admitted {
        ticket: Ticket,
        content: String,
        message: IncomingMessage,
        images: Vec<Value>,
    },
    /// A prompt the queue gave up on.
    Expired {
        message_id: String,
        said: String,
    },
    PositionChanged {
        position: usize,
    },
    /// The agent began a turn.
    TurnStart,
    AssistantText(String),
    TurnSettled {
        produced: bool,
        failure: Option<String>,
    },
    ToolStart {
        id: String,
        name: String,
        target: Option<String>,
    },
    ToolEnd {
        id: String,
        name: String,
        failed: bool,
        output: String,
    },
    Thinking,
    Thought(String),
    Usage(Usage),
    Error(String),
    CommandRejected {
        command: String,
        detail: String,
    },
    Retry(String),
    Dialog(DialogRequest),
    DialogTimeout,
    UnsupportedDialog,
    ProtocolViolation(String),
    Exit {
        code: i64,
    },
    DelegationReported(Reported),
}

/// The handle callers hold: a way in, and the facts that never change.
#[derive(Clone)]
pub struct SessionHandle {
    id: String,
    project: ProjectSelection,
    owner_id: String,
    commands: mpsc::Sender<Signal>,
}

impl SessionHandle {
    /// Starts the session's task and returns the way into it.
    pub fn spawn(mut options: SessionOptions) -> Self {
        let (commands, receiver) = mpsc::channel::<Signal>(256);
        let id = options.id.clone();
        let project = options.project.clone();
        let owner_id = options.owner_id.clone();
        let timers: Arc<dyn Timers> = match options.timers.take() {
            Some(injected) => injected,
            None => Arc::new(SystemTimers::new(commands.clone())),
        };
        options.timers = Some(timers);
        let running = Running::new(options, commands.clone());
        tokio::spawn(running.run(receiver));
        Self {
            id,
            project,
            owner_id,
            commands,
        }
    }

    /// The session's stable identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The project this session works in.
    pub fn project(&self) -> &ProjectSelection {
        &self.project
    }

    /// The account that started this session.
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    /// What this session was first asked to do, for naming it.
    pub fn opening(&self) -> String {
        self.project.prompt.trim().to_owned()
    }

    /// Starts the sandbox and waits for the agent to answer.
    ///
    /// Returns false when the session could not start, having already
    /// reported why in the thread and released everything it reserved.
    pub async fn start(&self, first: IncomingMessage) -> bool {
        let (reply, answer) = oneshot::channel();
        if self
            .commands
            .send(Signal::Start { first, reply })
            .await
            .is_err()
        {
            return false;
        }
        answer.await.unwrap_or(false)
    }

    /// Routes a message from the thread.
    pub async fn handle(&self, message: IncomingMessage) {
        let (reply, answer) = oneshot::channel();
        if self
            .commands
            .send(Signal::Handle { message, reply })
            .await
            .is_ok()
        {
            let _ = answer.await;
        }
    }

    /// Ends the session on request, reporting the reason in the thread.
    pub async fn stop(&self, reason: EndReason) {
        let (reply, answer) = oneshot::channel();
        if self
            .commands
            .send(Signal::Stop { reason, reply })
            .await
            .is_ok()
        {
            let _ = answer.await;
        }
    }

    /// Reconciles a message the person who sent it took back.
    pub async fn withdraw(&self, message_id: String) -> bool {
        let (reply, answer) = oneshot::channel();
        if self
            .commands
            .send(Signal::Withdraw { message_id, reply })
            .await
            .is_err()
        {
            return false;
        }
        answer.await.unwrap_or(false)
    }

    /// Everyone invited to take part, for reporting and persistence.
    #[allow(
        dead_code,
        reason = "the daemon persists the guest list through on_guests_changed instead"
    )]
    pub async fn guest_list(&self) -> Vec<String> {
        let (reply, answer) = oneshot::channel();
        if self.commands.send(Signal::Guests { reply }).await.is_err() {
            return Vec::new();
        }
        answer.await.unwrap_or_default()
    }

    /// True once the session has finished, by any path.
    pub async fn is_ended(&self) -> bool {
        let (reply, answer) = oneshot::channel();
        if self.commands.send(Signal::Ended { reply }).await.is_err() {
            return true;
        }
        answer.await.unwrap_or(true)
    }

    /// When this session last did or was told anything.
    pub async fn last_active_at(&self) -> i64 {
        let (reply, answer) = oneshot::channel();
        if self
            .commands
            .send(Signal::LastActive { reply })
            .await
            .is_err()
        {
            return 0;
        }
        answer.await.unwrap_or(0)
    }
}

/// The session's whole state, owned by its one task.
///
/// The flags mirror the original's fields one for one; gathering them into
/// sub-structures would make the port harder to check against it.
#[allow(clippy::struct_excessive_bools)]
struct Running {
    options: SessionOptions,
    commands: mpsc::Sender<Signal>,
    log: Logger,
    views: Arc<Redacting>,
    timers: Arc<dyn Timers>,

    sandbox: Option<RunningBox>,
    client: Option<AgentClient>,
    ticket: Option<Ticket>,
    pending_withdrawals: Vec<String>,
    current_message_id: Option<String>,
    current_author_id: Option<String>,
    idle_timer: Option<u64>,
    disk_timer: Option<u64>,
    delegating_timer: Option<u64>,
    /// Bytes held by the project and state directory when the session started.
    disk_baseline: u64,
    /// True once the session has been told it is close to its budget.
    disk_warned: bool,
    /// What the previous measurement saw, so a write rate can be derived.
    disk_last_written: u64,
    disk_last_at: i64,
    abort_timer: Option<u64>,
    aborting: bool,
    /// An interruption already asked for and not yet answered.
    abort_in_flight: bool,

    /// Who has already been told why they cannot take part.
    explained: HashSet<String>,
    pending_edits: Vec<(String, String)>,
    /// What each tool call produced, so a delegation can name one by its id.
    outputs: Arc<Mutex<Vec<(String, String)>>>,
    /// The delegations of the turn now running, if any.
    turn_delegations: Option<TurnDelegations<SessionSources, HttpSender>>,
    watcher: Option<Delegating>,
    /// What delegation has cost and saved this session, for reporting it.
    delegated_asked: u64,
    delegated_answered: u64,
    delegated_tokens: i64,
    delegated_kept_out: usize,
    guests: HashSet<String>,
    /// Where a `!model` moved this session, which outlives the command.
    switched: Option<(String, String)>,
    /// Speakers whose memory has already been given to the agent this session.
    introduced: HashSet<String>,
    /// Whoever spoke most recently, so a recorded fact is attributed to them.
    last_speaker_id: Option<String>,
    ended: bool,
    last_active: i64,
    /// The command being answered, so its replies can be marked as such.
    replying_to: Option<String>,
    /// Turns opened so far. Zero means nothing has been asked for yet.
    turn: u32,
    usage: SessionUsage,
}

impl Running {
    fn new(options: SessionOptions, commands: mpsc::Sender<Signal>) -> Self {
        let log = options
            .log
            .with(fields([("session", LogValue::from(options.id.as_str()))]));
        let guests = options.guest_ids.iter().cloned().collect();
        Self {
            views: Arc::clone(&options.views),
            timers: options.timers.clone().expect("spawn always sets timers"),
            turn: options.start_turn.unwrap_or(0),
            guests,
            explained: HashSet::new(),
            pending_edits: Vec::new(),
            outputs: Arc::new(Mutex::new(Vec::new())),
            introduced: HashSet::new(),
            delegated_asked: 0,
            delegated_answered: 0,
            delegated_tokens: 0,
            delegated_kept_out: 0,
            switched: None,
            last_speaker_id: None,
            ended: false,
            last_active: crate::log::now_ms(),
            replying_to: None,
            usage: SessionUsage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                total_tokens: 0,
                cost: 0.0,
                context_tokens: 0,
                context_window: None,
                turns: 0,
                model: None,
            },
            aborting: false,
            abort_in_flight: false,
            abort_timer: None,
            sandbox: None,
            client: None,
            ticket: None,
            pending_withdrawals: Vec::new(),
            current_message_id: None,
            current_author_id: None,
            idle_timer: None,
            disk_timer: None,
            delegating_timer: None,
            disk_baseline: 0,
            disk_warned: false,
            disk_last_written: 0,
            disk_last_at: 0,
            turn_delegations: None,
            watcher: None,
            options,
            commands,
            log,
        }
    }

    /// The session's whole life: take one command at a time, in order.
    ///
    /// One arm per signal keeps the dispatch readable; the bodies they call
    /// are the small methods below.
    #[allow(clippy::too_many_lines)]
    async fn run(mut self, mut commands: mpsc::Receiver<Signal>) {
        while let Some(signal) = commands.recv().await {
            match signal {
                Signal::Start { first, reply } => {
                    let started = self.start(first).await;
                    let _ = reply.send(started);
                }
                Signal::Handle { message, reply } => {
                    if !self.ended {
                        self.reset_idle_timer();
                        self.route(message).await;
                    }
                    let _ = reply.send(());
                }
                Signal::Stop { reason, reply } => {
                    self.end_because(
                        reason,
                        &format!("this session ended ({})", end_reason_name(reason)),
                    )
                    .await;
                    let _ = reply.send(());
                }
                Signal::Withdraw { message_id, reply } => {
                    let held = self.withdraw(&message_id).await;
                    let _ = reply.send(held);
                }
                Signal::Guests { reply } => {
                    let _ = reply.send(self.guests.iter().cloned().collect::<Vec<_>>());
                }
                Signal::Ended { reply } => {
                    let _ = reply.send(self.ended);
                }
                Signal::LastActive { reply } => {
                    let _ = reply.send(self.last_active);
                }
                Signal::Timer { timer, handle } => self.on_timer(timer, handle).await,
                Signal::Admitted {
                    ticket,
                    content,
                    message,
                    images,
                } => {
                    let spent = self.check_unavailable().await;
                    if spent.is_none() {
                        self.send_prompt(Some(ticket), &content, &message, &images)
                            .await;
                    } else {
                        self.ticket = Some(ticket);
                        self.current_message_id = Some(message.id);
                        self.views.send(SessionEvent::Waiting { text: None }).await;
                        self.say(&spent.unwrap_or_default()).await;
                        self.settle_turn(ReactionOutcome::Failed).await;
                    }
                }
                Signal::Expired { message_id, said } => {
                    self.views.send(SessionEvent::Waiting { text: None }).await;
                    self.say(&format!(
                        "this message waited longer than the queue allows and was not sent: {said}"
                    ))
                    .await;
                    self.react(&message_id, ReactionOutcome::Failed).await;
                }
                Signal::PositionChanged { position } => {
                    self.views
                        .send(SessionEvent::Waiting {
                            text: Some(format!("waiting for a turn slot, position {position}")),
                        })
                        .await;
                }
                Signal::TurnStart | Signal::Thinking => self.reset_idle_timer(),
                Signal::AssistantText(text) => {
                    self.reset_idle_timer();
                    self.say(&text).await;
                }
                Signal::TurnSettled { produced, failure } => {
                    self.on_settled(produced, failure.as_deref()).await;
                }
                Signal::ToolStart { id, name, target } => {
                    self.reset_idle_timer();
                    self.views
                        .send(SessionEvent::Activity {
                            line: tool_line(&name, target.as_deref()),
                            tool: Some(ToolActivity {
                                id: Some(id.clone()),
                                name: name.clone(),
                                target: target.clone(),
                                failed: None,
                            }),
                        })
                        .await;
                    if EDITING_TOOLS.contains(&name.as_str())
                        && let Some(target) = target
                    {
                        self.snapshot(&target);
                    }
                }
                Signal::ToolEnd {
                    id,
                    name,
                    failed,
                    output,
                } => {
                    self.on_tool_end(&id, &name, failed, &output).await;
                }
                Signal::Thought(text) => {
                    self.views.send(SessionEvent::Thinking { text }).await;
                }
                Signal::Usage(usage) => self.accrue(&usage).await,
                Signal::Error(detail) => {
                    self.say(&format!("the agent reported an error: {detail}"))
                        .await;
                }
                Signal::CommandRejected { command, detail } => {
                    // The agent refused outright, so no turn follows and
                    // nothing else will ever settle this. Reporting and
                    // settling here is what keeps the thread from going quiet
                    // and the admission slot from leaking.
                    self.say(&format!("the agent refused the {command}: {detail}"))
                        .await;
                    self.settle_turn(ReactionOutcome::Failed).await;
                }
                Signal::Retry(detail) => {
                    self.options.scheduler.note_rate_limit();
                    self.views
                        .send(SessionEvent::Notice {
                            text: warning_line(&format!(
                                "waiting on the model provider before continuing: {detail}"
                            )),
                            level: NoticeLevel::Warning,
                        })
                        .await;
                }
                Signal::Dialog(request) => {
                    self.reset_idle_timer();
                    self.say(&question_line(&dialog_lines(&request))).await;
                }
                Signal::DialogTimeout => {
                    self.say(
                        "the question went unanswered for too long and was cancelled; the session is still running",
                    )
                    .await;
                }
                Signal::UnsupportedDialog => {
                    self.say(&warning_line(
                        "the agent asked for a text editor, which a thread cannot provide; it was told to carry on without one",
                    ))
                    .await;
                }
                Signal::ProtocolViolation(detail) => {
                    self.end_because(
                        EndReason::ProtocolViolation,
                        &format!("the agent broke the protocol: {detail}"),
                    )
                    .await;
                }
                Signal::Exit { code } => self.on_exit(code).await,
                Signal::DelegationReported(reported) => self.note_delegation(reported).await,
            }
        }
    }

    /// Drives one fired timer.
    async fn on_timer(&mut self, timer: SessionTimer, _handle: u64) {
        match timer {
            SessionTimer::Idle => {
                self.end_because(
                    EndReason::Idle,
                    "nothing happened for a while, so this session stopped",
                )
                .await;
            }
            SessionTimer::AbortDeadline => {
                if self.abort_in_flight {
                    self.abort_in_flight = false;
                    self.abort_timer = None;
                    self.say(
                        "the agent did not confirm the interruption, so the session was force stopped",
                    )
                    .await;
                    self.end_because(
                        EndReason::Unresponsive,
                        "force stopped after an unconfirmed interruption",
                    )
                    .await;
                }
            }
            SessionTimer::Disk => self.check_disk().await,
            SessionTimer::Delegating => {
                if let Some(watcher) = &self.watcher {
                    let mut borrowed = self.turn_delegations.take();
                    watcher.sweep(borrowed.as_mut()).await;
                    self.turn_delegations = borrowed;
                }
                self.schedule_delegating();
            }
        }
    }

    fn schedule_delegating(&mut self) {
        if self.watcher.is_some() {
            self.delegating_timer =
                Some(self.timers.set_timeout(SessionTimer::Delegating, POLL_MS));
        }
    }

    async fn start(&mut self, first: IncomingMessage) -> bool {
        let github = self.options.config.github.clone();
        self.write_git_config(github.as_ref());
        self.write_agent_bin(github.as_ref());
        let system_prompt_path = self.write_memory_block();

        let mut env = BTreeMap::new();
        env.insert(
            self.options.config.agent.credential_name.clone(),
            self.options.config.agent.credential.clone(),
        );
        if let Some(github) = &github {
            // The GitHub token crosses too. Reading issues and leaving
            // comments is most of working on somebody's repository, and none
            // of it is possible without one. Pull requests are still composed
            // by the daemon.
            env.insert(TOKEN_VARIABLE.to_owned(), github.token.clone());
            for (name, value) in git_identity_env(github) {
                env.insert(name, value);
            }
        }

        let launch = SandboxLaunch {
            session_id: self.options.id.clone(),
            project_path: self.options.project.path.clone(),
            state_dir: self.options.state_dir.clone(),
            env,
            provider: self.provider(),
            model: self.model(),
            providers: self.options.config.agent.providers.clone(),
            system_prompt_path,
            resume: self.options.resume,
        };
        self.sandbox = match (self.options.launcher)(launch).await {
            Ok(sandbox) => Some(sandbox),
            Err(error) => {
                self.say(&format!("could not start this session: {}", reason(&error)))
                    .await;
                self.finish(EndReason::StartupFailed).await;
                return false;
            }
        };

        self.start_disk_watch().await;
        self.start_delegating();

        let client = AgentClient::new(
            self.sandbox
                .as_ref()
                .expect("the sandbox was just launched")
                .process
                .clone(),
            self.build_handlers(),
            self.log.clone(),
            self.options.config.timeouts.question_ms,
            None,
        );
        tokio::spawn({
            let client = client.clone();
            async move {
                client.run().await;
            }
        });
        self.client = Some(client);

        if let Err(error) = self
            .client
            .as_ref()
            .expect("the client was just made")
            .wait_until_ready(self.options.config.timeouts.startup_ms)
            .await
        {
            self.say(&format!(
                "the agent did not become ready within {}ms: {error}",
                self.options.config.timeouts.startup_ms
            ))
            .await;
            self.finish(EndReason::StartupFailed).await;
            return false;
        }

        // Which sandbox confines a session is not named in the thread. It
        // tells a reader nothing they can act on, and tells anyone else what
        // to probe. The operator sees it at startup, in the log, where it
        // belongs.
        let opening = if self.options.resume {
            format!("resumed, continuing in {}", self.options.project.name)
        } else {
            format!("ready, working in {}", self.options.project.name)
        };

        // Offered once, when the thread is new. A thread shows the
        // conversation and the interface shows the work behind it, and
        // somebody reading on a phone has no other way to find the second
        // from the first.
        let transcript = self.session_links().transcript;
        self.views
            .send(SessionEvent::Notice {
                text: match transcript {
                    None => connection_line(&opening),
                    Some(link) => format!("{}\n{link}", connection_line(&opening)),
                },
                level: NoticeLevel::Started,
            })
            .await;
        self.reset_idle_timer();
        self.route(first).await;
        true
    }

    async fn route(&mut self, message: IncomingMessage) {
        let content = message.content.trim().to_owned();
        let mut words = content.split_whitespace();
        let word = words.next().unwrap_or_default().to_owned();
        let rest = words.collect::<Vec<_>>().join(" ");

        // First, so that an aside which happens to read like a command still
        // runs nothing. Deciding this later would make the marker
        // unreliable, which is the opposite of what it is for.
        if is_aside(&content) {
            self.note_aside(&content, &message).await;
            return;
        }

        if is_command(&content) {
            self.run_command(&word, &rest, message).await;
            return;
        }

        // Left alone entirely. It is another bot's command, where anything
        // from this one is noise in somebody else's exchange, or a person
        // typing to the room, where marking it failed says their message was
        // wrong when it was simply not addressed here.
        if is_addressed_to_bot(&content) {
            return;
        }

        let pending = self.client.as_ref().and_then(AgentClient::pending_dialog);
        if let Some(dialog) = pending {
            self.answer_dialog(&dialog, &content, &message).await;
            return;
        }

        // A message carrying only a file still says something: that a file
        // arrived. Discarding it for having no text is why one used to vanish.
        let attached = self.take_attachments(&message).await;
        if content.is_empty() && attached.note.is_empty() {
            return;
        }

        self.submit_prompt(&content, message, false, attached).await;
    }

    /// Says something in the session.
    ///
    /// While a command is being answered this marks what it writes as that
    /// command's reply. Everywhere else it is ordinary output. Routing it in
    /// one place is what keeps every reply marked without each of them having
    /// to remember to say so.
    async fn say(&self, text: &str) {
        match &self.replying_to {
            Some(command) => {
                self.views
                    .send(SessionEvent::Reply {
                        text: text.to_owned(),
                        command: command.clone(),
                    })
                    .await;
            }
            None => {
                self.views
                    .send(SessionEvent::Post {
                        text: text.to_owned(),
                    })
                    .await;
            }
        }
    }

    async fn react(&self, message_id: &str, outcome: ReactionOutcome) {
        self.views
            .send(SessionEvent::Reaction {
                message_id: message_id.to_owned(),
                outcome,
            })
            .await;
    }

    /// Takes what was attached and says what the agent should know about it.
    async fn take_attachments(&self, message: &IncomingMessage) -> Attached {
        if message.attachments.is_empty() {
            return Attached::default();
        }

        let fetch: FetchAttachment = self
            .options
            .fetch_attachment
            .clone()
            .unwrap_or_else(|| Arc::new(default_fetch_attached));
        let outcome = receive(
            &message.attachments,
            &self.options.project.path,
            attachments::Limits {
                max_bytes: self.options.config.output.max_attachment_bytes,
                max_count: self.options.config.output.max_attachments_per_message,
            },
            move |url: String| {
                let fetch = Arc::clone(&fetch);
                Box::pin(async move { fetch(url).await })
                    as Pin<
                        Box<
                            dyn Future<
                                    Output = Result<
                                        Vec<u8>,
                                        Box<dyn std::error::Error + Send + Sync>,
                                    >,
                                > + Send,
                        >,
                    >
            },
        )
        .await;

        for refusal in &outcome.refused {
            self.say(&warning_line(&format!(
                "`{}` was not taken: {}",
                refusal.name, refusal.reason
            )))
            .await;
        }

        if outcome.taken.is_empty() {
            return Attached::default();
        }

        let listed = outcome
            .taken
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>()
            .join(", ");
        let note = format!("Files attached to this message, saved in the project at: {listed}");

        // Handed over to look at as well as saved, so the agent has both the
        // picture and the path.
        let images = outcome
            .taken
            .iter()
            .filter(|file| is_image(file.content_type.as_deref(), &file.path))
            .map(|file| AgentImage {
                r#type: "image".to_owned(),
                data: encode_base64(&file.bytes),
                mime_type: file
                    .content_type
                    .clone()
                    .unwrap_or_else(|| "image/png".to_owned()),
            })
            .collect::<Vec<_>>();

        let describe = self.options.describe_images.clone();
        if images.is_empty() || describe.is_none() {
            return Attached { note, images };
        }

        // Injected only when this session's model cannot be shown an image,
        // so reaching here means handing them over would fail the turn.
        let describe = describe.expect("checked above");
        match describe(images.clone(), message.content.clone()).await {
            Ok(described) => Attached {
                note: format!("{note}\n\n{described}"),
                images: Vec::new(),
            },
            Err(error) => {
                self.log.warn(
                    "could not describe an attached image",
                    &fields([("detail", LogValue::from(error.clone()))]),
                );
                self.say(&warning_line(&error)).await;
                Attached {
                    note,
                    images: Vec::new(),
                }
            }
        }
    }

    async fn answer_dialog(
        &mut self,
        dialog: &DialogRequest,
        content: &str,
        message: &IncomingMessage,
    ) {
        let outcome = self
            .client
            .as_ref()
            .map_or(crate::agent::client::AnswerOutcome::Unknown, |client| {
                client.answer_dialog(&dialog.id, content)
            });
        if outcome == crate::agent::client::AnswerOutcome::Accepted {
            // Recorded like any other prompt. It is what the agent was
            // waiting for, and a transcript without it shows a question that
            // answered itself.
            self.note_prompt(&display_name(message), content, Some(&message.id))
                .await;
            self.react(&message.id, ReactionOutcome::Accepted).await;
            return;
        }
        self.say(&question_line(&format!(
            "that did not answer the question. {}",
            dialog_lines(dialog)
        )))
        .await;
    }

    async fn submit_prompt(
        &mut self,
        content: &str,
        message: IncomingMessage,
        hold: bool,
        attached: Attached,
    ) {
        if content.is_empty() && attached.note.is_empty() {
            return;
        }

        if asks_for_pull_request(content) {
            self.note_pull_request_asked(&message);
        }

        // What arrived is said as part of the prompt, so the agent knows a
        // file is there and where to read it.
        let said = if attached.note.is_empty() {
            content.to_owned()
        } else {
            format!("{content}\n\n{}", attached.note).trim().to_owned()
        };

        // A session is the owner's: their project, their model spend, their
        // turn in the queue. Taking part is something they invite you to.
        if !self.may_take_part(&message.author_id) {
            self.refuse(&message, &self.not_invited()).await;
            return;
        }

        // Checked before the turn is queued or the running one redirected, so
        // a spent window is answered with when to come back rather than with
        // a turn that starts and then fails against the provider.
        if let Some(spent) = self.check_unavailable().await {
            self.say(&spent).await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        }

        // Saying something to a working agent redirects what it is doing,
        // which is the point of saying it now rather than waiting. It is the
        // same turn, so it neither opens one nor reserves a slot.
        if self.ticket.is_some() && !hold {
            let images = attached.images.iter().map(image_value).collect::<Vec<_>>();
            self.redirect(&said, message, images).await;
            return;
        }

        // Opened before the prompt is noted, so the prompt is the first thing
        // in the turn it starts rather than the last thing in the one before
        // it.
        self.turn += 1;
        self.turn_delegations = self.new_delegations();
        self.views
            .send(SessionEvent::BeginTurn { turn: self.turn })
            .await;

        self.react(&message.id, ReactionOutcome::Accepted).await;
        self.note_prompt(&display_name(&message), &said, Some(&message.id))
            .await;

        let with_context = self.introduce(&message, &said);
        self.last_speaker_id = Some(message.author_id.clone());
        let image_values = attached.images.iter().map(image_value).collect::<Vec<_>>();

        let outcome = self.options.scheduler.submit(QueueEntry {
            session_id: self.options.id.clone(),
            on_admitted: {
                let commands = self.commands.clone();
                let content = with_context.clone();
                let message = message.clone();
                let images = image_values.clone();
                Box::new(move |ticket| {
                    let _ = commands.try_send(Signal::Admitted {
                        ticket,
                        content,
                        message,
                        images,
                    });
                })
            },
            on_expired: {
                let commands = self.commands.clone();
                let message_id = message.id.clone();
                let said = first_line(&said);
                Box::new(move || {
                    let _ = commands.try_send(Signal::Expired { message_id, said });
                })
            },
            on_position_changed: {
                let commands = self.commands.clone();
                Some(Box::new(move |position| {
                    let _ = commands.try_send(Signal::PositionChanged { position });
                }))
            },
        });

        match outcome {
            SubmitOutcome::Admitted { ticket } => {
                self.send_prompt(Some(ticket), &with_context, &message, &image_values)
                    .await;
            }
            SubmitOutcome::Rejected { reason } => {
                self.say(&reason).await;
                self.react(&message.id, ReactionOutcome::Failed).await;
            }
            SubmitOutcome::Queued { position } => {
                self.views
                    .send(SessionEvent::Waiting {
                        text: Some(format!("waiting for a turn slot, position {position}")),
                    })
                    .await;
            }
        }
    }

    async fn check_unavailable(&self) -> Option<String> {
        let unavailable = self.options.unavailable.as_ref()?;
        unavailable(&self.provider()).await
    }

    async fn send_prompt(
        &mut self,
        ticket: Option<Ticket>,
        content: &str,
        message: &IncomingMessage,
        images: &[Value],
    ) {
        self.ticket = ticket;
        self.current_message_id = Some(message.id.clone());
        self.current_author_id = Some(message.author_id.clone());
        self.views.send(SessionEvent::Waiting { text: None }).await;
        self.views.send(SessionEvent::Busy { busy: true }).await;

        let sent = self.client.as_ref().is_some_and(|client| {
            client.prompt(
                content,
                (!images.is_empty()).then(|| images.to_vec()),
                Some(StreamingBehavior::FollowUp),
            )
        });
        if !sent {
            self.say("the agent is not accepting prompts; this session has ended")
                .await;
            self.settle_turn(ReactionOutcome::Failed).await;
            self.finish(EndReason::Crashed).await;
        }
    }

    /// Notes something said to the people in the thread rather than to the
    /// agent.
    async fn note_aside(&mut self, content: &str, message: &IncomingMessage) {
        let said = content
            .trim_start()
            .strip_prefix(ASIDE)
            .unwrap_or_default()
            .trim()
            .to_owned();
        self.note_aside_recorded(&display_name(message), &said, Some(&message.id))
            .await;
        self.react(&message.id, ReactionOutcome::Succeeded).await;
    }

    async fn note_aside_recorded(&self, author: &str, text: &str, id: Option<&str>) {
        self.views
            .send(SessionEvent::Aside {
                author: author.to_owned(),
                text: text.to_owned(),
                id: id.map(str::to_owned),
                withdrawn: false,
            })
            .await;
    }

    /// Redirects the turn already running.
    async fn redirect(&mut self, content: &str, message: IncomingMessage, images: Vec<Value>) {
        self.reset_idle_timer();
        self.last_speaker_id = Some(message.author_id.clone());

        self.react(&message.id, ReactionOutcome::Accepted).await;
        self.note_prompt(&display_name(&message), content, Some(&message.id))
            .await;

        let introduced = self.introduce(&message, content);
        let sent = self.client.as_ref().is_some_and(|client| {
            client.steer(&introduced, (!images.is_empty()).then(|| images.clone()))
        });
        if !sent {
            self.say("the agent is not accepting anything further; this session has ended")
                .await;
            self.react(&message.id, ReactionOutcome::Failed).await;
        }
    }

    /// Reconciles a message the person who sent it took back.
    ///
    /// Held until the turn settles when one is running, so nothing writes to
    /// the agent's conversation while the agent is appending to it.
    async fn withdraw(&mut self, message_id: &str) -> bool {
        if self.ticket.is_some() {
            self.pending_withdrawals.push(message_id.to_owned());
            return true;
        }
        self.apply_withdrawal(message_id).await
    }

    // Keeping the signature async keeps every caller's await uniform.
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    async fn apply_withdrawal(&mut self, message_id: &str) -> bool {
        let said = match withdraw_from_record(&self.options.state_dir, message_id) {
            Ok(Some(said)) => said,
            Ok(None) => return false,
            Err(error) => {
                self.log.warn(
                    "a withdrawal could not be written",
                    &fields([("detail", LogValue::from(error.to_string()))]),
                );
                return false;
            }
        };
        let _ = withdraw_from_agent_session(&self.options.state_dir, &said);

        // Told, not corrected: `steer` is the one command that reaches a
        // running agent without asking it for another turn, and being
        // answered about a withdrawal is the opposite of taking something
        // back.
        if let Some(client) = &self.client {
            client.steer(
                "A message you were sent has been withdrawn by the person who sent it. \
                 Disregard it; do not act on it further and do not reply about it.",
                None,
            );
        }
        true
    }

    /// Applies what a running turn held back.
    async fn drain_withdrawals(&mut self) {
        let pending = std::mem::take(&mut self.pending_withdrawals);
        for message_id in pending {
            self.apply_withdrawal(&message_id).await;
        }
    }

    /// Releases the turn's admission slot and sets the outcome reaction.
    ///
    /// Every path that ends a turn goes through here, so the slot is released
    /// exactly once and the scheduler's view never drifts from reality.
    async fn settle_turn(&mut self, outcome: ReactionOutcome) {
        let ticket = self.ticket.take();
        self.views.send(SessionEvent::Busy { busy: false }).await;
        // Now that the agent is not writing to its own conversation, anything
        // a running turn held back can be applied to it.
        self.drain_withdrawals().await;

        if let Some(ticket) = ticket
            && !self.options.scheduler.release(&ticket)
        {
            self.log.warn(
                "a turn slot was already released",
                &fields([("outcome", LogValue::from(outcome.as_str()))]),
            );
        }

        let message_id = self.current_message_id.take();
        self.current_author_id = None;
        if let Some(message_id) = message_id {
            self.react(&message_id, outcome).await;
        }
    }

    /// Adds a turn's usage to the running total.
    ///
    /// The agent reports each turn's own cost. Context is the latest turn's
    /// input rather than a sum, because it is what the model is carrying now.
    // The agent reports token counts as whole numbers in float fields.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    async fn accrue(&mut self, usage: &Usage) {
        let context_window = self
            .client
            .as_ref()
            .and_then(AgentClient::context_window)
            .map(|window| window as u64);
        let total = &mut self.usage;
        total.input += usage.input as u64;
        total.output += usage.output as u64;
        total.cache_read += usage.cache_read as u64;
        total.cache_write += usage.cache_write as u64;
        total.total_tokens += usage.total_tokens as u64;
        total.cost += usage.cost;
        total.context_tokens = (usage.input + usage.cache_read) as u64;
        total.context_window = context_window.or(total.context_window);
        total.turns += 1;
        if let Some(model) = &usage.model {
            total.model = Some(model.clone());
        }
        let current = self.usage.clone();
        self.views
            .send(SessionEvent::Usage { usage: current })
            .await;
    }

    fn build_handlers(&self) -> AgentHandlers {
        let commands = self.commands.clone();
        let mut handlers = AgentHandlers::default();
        macro_rules! on {
            ($field:ident, $type:ty, $make:expr) => {{
                let commands = commands.clone();
                handlers.$field = Some(Box::new(move |input: $type| {
                    let _ = commands.try_send($make(input));
                }));
            }};
        }
        on!(on_turn_start, (), |(): ()| Signal::TurnStart);
        on!(on_thinking, (), |(): ()| Signal::Thinking);
        on!(on_assistant_text, String, Signal::AssistantText);
        on!(on_turn_settled, (bool, Option<String>), |(
            produced,
            failure,
        )| {
            Signal::TurnSettled { produced, failure }
        });
        on!(on_tool_start, (String, String, Option<String>), |(
            id,
            name,
            target,
        )| {
            Signal::ToolStart { id, name, target }
        });
        on!(on_tool_end, (String, String, bool, String), |(
            id,
            name,
            failed,
            output,
        )| {
            Signal::ToolEnd {
                id,
                name,
                failed,
                output,
            }
        });
        on!(on_thought, String, Signal::Thought);
        on!(on_usage, Usage, Signal::Usage);
        on!(on_error, String, Signal::Error);
        on!(on_command_rejected, (String, String), |(
            command,
            detail,
        )| {
            Signal::CommandRejected { command, detail }
        });
        on!(on_retry, String, Signal::Retry);
        on!(on_dialog, DialogRequest, Signal::Dialog);
        on!(on_dialog_timeout, DialogRequest, |_| Signal::DialogTimeout);
        on!(on_unsupported_dialog, String, |_| Signal::UnsupportedDialog);
        on!(on_protocol_violation, String, Signal::ProtocolViolation);
        on!(on_exit, (i64, bool), |(code, _during)| Signal::Exit {
            code
        });
        handlers
    }

    async fn on_settled(&mut self, produced: bool, failure: Option<&str>) {
        self.options.scheduler.note_success();
        self.harvest_memory();
        self.open_requested_pull_request().await;
        if self.ended {
            // A pull request failure never ends a session; a protocol
            // violation during the settle could have.
            return;
        }

        // The agent's own words were posted as it produced them. This closes
        // the turn and pings whoever asked, which is the point of driving it
        // remotely.
        //
        // The turn's own author is cleared when a turn settles, so a settle
        // that arrives without one falls back to whoever last spoke rather
        // than to nobody. A queued message must not lose its ping.
        let who = self
            .current_author_id
            .clone()
            .or_else(|| self.last_speaker_id.clone())
            .unwrap_or_else(|| self.options.owner_id.clone());
        let mention = format!("<@{who}> ");
        let spent = if self.usage.turns > 0 {
            // Token totals sit far below f64's exact range.
            #[allow(clippy::cast_precision_loss)]
            let shown = crate::chat::render::Usage {
                input: self.usage.input as f64,
                cache_read: self.usage.cache_read as f64,
                total_tokens: self.usage.total_tokens as f64,
                cost: self.usage.cost,
                context_tokens: self.usage.context_tokens as f64,
                context_window: self
                    .usage
                    .context_window
                    .map_or(0.0, |window| window as f64),
            };
            format!(" {}", usage_summary(&shown))
        } else {
            String::new()
        };

        // A turn that failed is not a turn that had nothing to say. Saying so
        // is the difference between "the model was brief" and "the request
        // never reached it", which otherwise look identical from the thread.
        let ending = match (produced, failure) {
            (true, _) => format!("{}{spent}", marker("done")),
            (false, None) => {
                format!(
                    "{} the turn finished without producing any output{spent}",
                    marker("done")
                )
            }
            (false, Some(failure)) => {
                format!("{} the turn failed: {failure}{spent}", marker("failed"))
            }
        };
        // A warning rather than a done: the session is still alive and the
        // prompt can be sent again, which is not what "done" invites.
        self.views
            .send(SessionEvent::Notice {
                text: format!("{mention}{ending}"),
                level: if failure.is_none() {
                    NoticeLevel::Done
                } else {
                    NoticeLevel::Warning
                },
            })
            .await;

        let interrupted = self.aborting;
        self.settle_turn(if interrupted {
            ReactionOutcome::Interrupted
        } else {
            ReactionOutcome::Succeeded
        })
        .await;
        self.turn_delegations = None;
        self.aborting = false;
        if let Some(handle) = self.abort_timer.take() {
            self.timers.clear_timeout(handle);
        }
        self.abort_in_flight = false;
        self.reset_idle_timer();
    }

    async fn on_tool_end(&mut self, id: &str, name: &str, failed: bool, output: &str) {
        if !failed && EDITING_TOOLS.contains(&name) {
            self.report_edit(name, id).await;
        }

        // Kept whole rather than truncated as the thread shows it: a
        // delegation about a log is worth nothing if it is asked about the
        // first page of one.
        if !id.is_empty() {
            let mut outputs = self.outputs.lock().expect("the tool output list");
            if let Some(existing) = outputs.iter_mut().find(|(kept, _)| kept == id) {
                existing.1.replace_range(.., output);
            } else {
                outputs.push((id.to_owned(), output.to_owned()));
            }
            while outputs.len() > MAX_REMEMBERED_OUTPUTS {
                outputs.remove(0);
            }
        }

        // Reported whatever the thread is configured to forward, because a
        // surface that can fold output away has no reason to be spared it.
        self.views
            .send(SessionEvent::ToolResult {
                result: ToolResult {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    failed,
                    output: truncate(output, self.options.config.output.max_tool_output_chars),
                },
            })
            .await;
        // The result was reported once, above. Each surface decides whether
        // to show it, so it is not also posted here as though the agent had
        // said it, which would show it twice wherever it is already attached
        // to the call it came from.
        if failed && !self.options.config.output.forward_tool_output {
            self.views
                .send(SessionEvent::Activity {
                    line: tool_line(name, Some("failed")),
                    tool: Some(ToolActivity {
                        id: Some(id.to_owned()),
                        name: name.to_owned(),
                        target: None,
                        failed: Some(true),
                    }),
                })
                .await;
        }
    }

    /// What the agent's dying words say went wrong, when they say anything.
    fn diagnose(words: &str) -> Option<&'static str> {
        let lower = words.to_lowercase();
        if lower.contains("enospc") || lower.contains("no space left on device") {
            return Some("the host it runs on has run out of disk space");
        }
        if lower.contains("enomem")
            || lower.contains("out of memory")
            || lower.contains("cannot allocate memory")
        {
            return Some("the host it runs on has run out of memory");
        }
        if lower.contains("eacces") || lower.contains("permission denied") {
            return Some("it was refused permission to something it needs");
        }
        None
    }

    async fn on_exit(&mut self, code: i64) {
        if self.ended {
            return;
        }

        let words = self
            .client
            .as_ref()
            .map_or_else(String::new, AgentClient::dying_words);
        if let Some(named) = Self::diagnose(&words) {
            self.end_because(
                EndReason::ResourceLimit,
                &format!("this session stopped because {named}"),
            )
            .await;
            return;
        }

        // 137 is SIGKILL, which is how a container killed for exceeding a
        // limit ends. Naming the limit beats reporting a generic crash.
        if code == 137 {
            let sandbox = &self.options.config.sandbox;
            self.end_because(
                EndReason::ResourceLimit,
                &format!(
                    "the session was terminated for exceeding a configured resource limit (memory {}, cpus {}, pids {})",
                    sandbox.memory, sandbox.cpus, sandbox.pids
                ),
            )
            .await;
            return;
        }

        // Whatever it last said, so a reader has something to act on rather
        // than a number.
        let said = words
            .split('\n')
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(first_line)
            .unwrap_or_default();
        let detail = if said.is_empty() {
            format!("this session ended unexpectedly with exit code {code}")
        } else {
            format!("this session ended unexpectedly with exit code {code}: {said}")
        };
        self.end_because(EndReason::Crashed, &detail).await;
    }

    /// Writes the git configuration into the agent's own home.
    ///
    /// Into the home rather than the project: a configuration in the working
    /// tree is one the agent could commit by accident, and it would follow
    /// the code into whatever it opens a pull request against.
    fn write_git_config(&self, github: Option<&GithubConfig>) {
        let Some(github) = github else { return };
        let home = std::path::Path::new(&self.options.state_dir).join("home");
        let written = std::fs::create_dir_all(&home).and_then(|()| {
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(home.join(GITCONFIG_FILENAME))
                .and_then(|mut file| {
                    use std::io::Write;
                    file.write_all(git_config_contents(github).as_bytes())
                })
        });
        if let Err(error) = written {
            self.log.warn(
                "could not write the git configuration",
                &fields([("detail", LogValue::from(error.to_string()))]),
            );
        }
    }

    /// Puts the wrappers a session runs in place of the real program.
    fn write_agent_bin(&self, github: Option<&GithubConfig>) {
        let bin = std::path::Path::new(&self.options.state_dir)
            .join("home")
            .join("bin");
        let delegate = self.options.config.agent.delegate.as_ref();
        let written = std::fs::create_dir_all(&bin).and_then(|()| {
            if let Some(delegate) = delegate {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o755)
                    .open(bin.join(DELEGATE_COMMAND))
                    .and_then(|mut file| {
                        use std::io::Write;
                        file.write_all(delegate_command_contents(delegate.deadline_ms).as_bytes())
                    })?;
            }
            if let Some(_github) = github {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o755)
                    .open(bin.join(GH_SHIM_FILENAME))
                    .and_then(|mut file| {
                        use std::io::Write;
                        file.write_all(gh_shim_contents().as_bytes())
                    })?;
            }
            Ok(())
        });
        if let Err(error) = written {
            self.log.warn(
                "could not write the agent's wrappers",
                &fields([("detail", LogValue::from(error.to_string()))]),
            );
        }
    }

    /// Who a pull request from this session is on behalf of.
    fn requested_by(&self) -> String {
        if let Some(asked) = self.who_asked()
            && asked.id != self.options.owner_id
        {
            return self
                .display_name_of(&asked.id)
                .or_else(|| asked.name.clone())
                .unwrap_or(asked.id);
        }
        self.display_name_of(&self.options.owner_id)
            .or_else(|| self.options.owner_name.clone())
            .unwrap_or_else(|| self.options.owner_id.clone())
    }

    fn display_name_of(&self, user_id: &str) -> Option<String> {
        self.options
            .memory
            .as_ref()?
            .display_name(user_id)
            .ok()
            .flatten()
    }

    /// Whoever asked for a pull request, which is not always whose session it
    /// is.
    fn who_asked(&self) -> Option<Asked> {
        let contents = std::fs::read_to_string(
            std::path::Path::new(&record_dir(&self.options.state_dir)).join(ASKED_FILENAME),
        )
        .ok()?;
        let mut lines = contents.split('\n');
        let id = lines.next()?.trim();
        if id.is_empty() {
            return None;
        }
        Some(Asked {
            id: id.to_owned(),
            name: lines
                .next()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        })
    }

    /// Where this session can be read back, for a pull request to point at.
    fn session_links(&self) -> SessionLinks {
        SessionLinks {
            thread: self
                .options
                .thread_id
                .as_ref()
                .zip(self.options.guild_id.as_ref())
                .map(|(thread, guild)| thread_link(guild, thread)),
            transcript: self
                .options
                .public_url
                .as_ref()
                .map(|url| transcript_link(url, &self.options.id)),
        }
    }

    /// Writes what the agent should know before it starts, as its system
    /// prompt. Returns the path, or none when there is nothing to say and it
    /// would cost context for no benefit.
    fn write_memory_block(&self) -> Option<String> {
        let memory = self.options.memory.as_ref()?;

        let notes_path = std::path::Path::new(&self.options.state_dir)
            .join(crate::memory::store::NOTES_FILENAME);
        let project_notes_path = std::path::Path::new(&self.options.state_dir)
            .join(crate::memory::store::PROJECT_NOTES_FILENAME);

        let about = [
            memory
                .render(&self.options.owner_id, DEFAULT_MEMORY_BUDGET)
                .unwrap_or_default(),
            memory
                .render_project(&self.options.project.name, DEFAULT_MEMORY_BUDGET)
                .unwrap_or_default(),
        ]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

        // Only when the agent can actually push. Telling it how to attribute
        // a pull request it has no credential to open is instruction for its
        // own sake.
        let attribution = self
            .options
            .config
            .github
            .as_ref()
            .map(|github| review_instructions(github, &self.requested_by(), &self.session_links()))
            .unwrap_or_default();

        let delegating = self
            .options
            .config
            .agent
            .delegate
            .as_ref()
            .map(|delegate| delegate_instructions(&delegate.model, delegate.per_turn as usize))
            .unwrap_or_default();

        let contents = format!(
            "{}{}{}{attribution}{delegating}",
            self.house_rules().unwrap_or_default(),
            about,
            memory_instructions(
                &format!(
                    "{}/{}",
                    crate::sandbox::backend::STATE_PATH,
                    crate::memory::store::NOTES_FILENAME
                ),
                &format!(
                    "{}/{}",
                    crate::sandbox::backend::STATE_PATH,
                    crate::memory::store::PROJECT_NOTES_FILENAME
                ),
            )
        );

        let path = std::path::Path::new(&self.options.state_dir)
            .join(crate::memory::store::BLOCK_FILENAME);
        // Written before the sandbox is launched, so nothing else has had
        // reason to create the directory yet. The notes files are created
        // empty so the agent appends to a file it can see exists.
        let written = std::fs::create_dir_all(&self.options.state_dir)
            .and_then(|()| std::fs::write(&path, format!("{contents}\n")))
            .and_then(|()| {
                for notes in [&notes_path, &project_notes_path] {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(notes)?;
                }
                Ok(())
            });
        if let Err(error) = written {
            self.log.warn(
                "could not write the memory block",
                &fields([("detail", LogValue::from(error.to_string()))]),
            );
            return None;
        }
        Some(path.display().to_string())
    }

    /// The operator's standing instructions, when a file of them is
    /// configured.
    fn house_rules(&self) -> Option<String> {
        let path = self.options.config.agent.rules_path.as_ref()?;
        match std::fs::read_to_string(path).map(|text| rules_block(&text).unwrap_or_default()) {
            Ok(block) => Some(block),
            Err(error) => {
                self.log.warn(
                    "the house rules could not be read",
                    &fields([
                        ("path", LogValue::from(path.as_str())),
                        ("detail", LogValue::from(error.to_string())),
                    ]),
                );
                None
            }
        }
    }

    /// Prefixes a speaker's memory to their first message in this session.
    fn introduce(&mut self, message: &IncomingMessage, content: &str) -> String {
        let Some(memory) = &self.options.memory else {
            return content.to_owned();
        };
        if self.introduced.contains(&message.author_id) {
            return content.to_owned();
        }

        self.introduced.insert(message.author_id.clone());
        if let Some(name) = &message.author_name {
            let _ = memory.remember_user(&message.author_id, name, crate::log::now_ms());
        }

        // The owner was already introduced through the system prompt.
        if message.author_id == self.options.owner_id {
            return content.to_owned();
        }

        let block = memory
            .render_for_speaker(&message.author_id, DEFAULT_MEMORY_BUDGET)
            .unwrap_or_default();
        if block.is_empty() {
            content.to_owned()
        } else {
            format!("{block}\n\n{content}")
        }
    }

    /// Records a file's contents before an edit, so the change can be shown.
    fn snapshot(&mut self, agent_path: &str) {
        let Some(host) = self.host_path(agent_path) else {
            return;
        };
        match std::fs::metadata(&host) {
            Ok(meta) if meta.is_file() && meta.len() <= MAX_DIFFABLE_BYTES => {
                if let Ok(text) = std::fs::read_to_string(&host) {
                    self.remember_edit(agent_path, text);
                }
            }
            // A file that does not exist yet is an empty one for diffing
            // purposes.
            _ => self.remember_edit(agent_path, String::new()),
        }
    }

    fn remember_edit(&mut self, agent_path: &str, before: String) {
        if let Some(existing) = self
            .pending_edits
            .iter_mut()
            .find(|(path, _)| path == agent_path)
        {
            existing.1 = before;
        } else {
            self.pending_edits.push((agent_path.to_owned(), before));
        }
    }

    /// Posts what an edit changed, once the tool has finished.
    async fn report_edit(&mut self, tool_name: &str, call: &str) {
        if !self.options.config.output.post_diffs {
            self.pending_edits.clear();
            return;
        }

        let edits = std::mem::take(&mut self.pending_edits);
        for (agent_path, before) in edits {
            let Some(host) = self.host_path(&agent_path) else {
                continue;
            };
            let Ok(meta) = std::fs::metadata(&host) else {
                continue;
            };
            if !meta.is_file() || meta.len() > MAX_DIFFABLE_BYTES {
                continue;
            }
            let Ok(after) = std::fs::read_to_string(&host) else {
                continue;
            };

            let diff = file_diff(&before, &after);
            if diff.empty {
                continue;
            }
            self.log.info(
                "posting an edit",
                &fields([
                    ("tool", LogValue::from(tool_name)),
                    ("path", LogValue::from(agent_path.as_str())),
                ]),
            );
            self.views
                .send(SessionEvent::Diff {
                    path: self.display_path(&agent_path),
                    added: diff.added as u64,
                    removed: diff.removed as u64,
                    body: diff.body,
                    cause: Some(call.to_owned()),
                })
                .await;
        }
    }

    fn host_path(&self, agent_path: &str) -> Option<String> {
        self.sandbox
            .as_ref()
            .and_then(|sandbox| (sandbox.to_host_path)(agent_path))
    }

    /// A path as a reader would recognise it, relative to the project.
    fn display_path(&self, agent_path: &str) -> String {
        let Some(host) = self.host_path(agent_path) else {
            return agent_path.to_owned();
        };
        let root = &self.options.project.path;
        match host.strip_prefix(root) {
            Some(rest) => rest.trim_start_matches('/').to_owned(),
            None => host,
        }
    }

    /// Lists a directory or shows a file, without involving the agent.
    async fn read_path(&mut self, request: &str) {
        let wanted = request.trim();
        let wanted = if wanted.is_empty() { "." } else { wanted };
        let Some(host) = self.host_path(wanted) else {
            self.say(&format!("`{wanted}` is not inside this session's project"))
                .await;
            return;
        };

        let display = {
            let shown = self.display_path(wanted);
            if shown.is_empty() {
                ".".to_owned()
            } else {
                shown
            }
        };
        let result = std::fs::metadata(&host).map(|meta| meta.is_dir());
        match result {
            Ok(true) => {
                // A directory is listed whichever was asked for: `!cat` on
                // one is a mistake worth answering rather than an error worth
                // reporting.
                match read_directory(&host, &display) {
                    Ok(entries) => {
                        self.say(&directory_listing(&entries, &display)).await;
                    }
                    Err(error) => {
                        self.say(&format!("could not read `{wanted}`: {error}"))
                            .await;
                    }
                }
            }
            Ok(false) => match read_file_for_display(&host, &display, MAX_INLINE) {
                Ok(contents) => {
                    let shown = file_view(&contents);
                    self.say(&shown).await;
                }
                Err(NotAFileError(path)) => {
                    self.say(&format!("could not read `{wanted}`: {path} is not a file"))
                        .await;
                }
            },
            Err(error) => {
                self.say(&format!("could not read `{wanted}`: {error}"))
                    .await;
            }
        }
    }

    /// Uploads a file from the project on request.
    async fn upload_file(&mut self, request: &str) {
        let wanted = request.trim();
        if wanted.is_empty() {
            self.say("say which file, as `!file <path>`").await;
            return;
        }

        let Some(host) = self.host_path(wanted) else {
            self.say(&format!("`{wanted}` is not inside this session's project"))
                .await;
            return;
        };

        match std::fs::metadata(&host) {
            Ok(meta) if !meta.is_file() => {
                self.say(&format!("`{wanted}` is not a file")).await;
            }
            Ok(meta) if meta.len() > MAX_UPLOAD_BYTES => {
                #[allow(
                    clippy::cast_precision_loss,
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss
                )]
                let kilobytes = (meta.len() as f64 / 1024.0).round() as u64;
                self.say(&format!(
                    "`{wanted}` is {kilobytes} KB, larger than the upload limit"
                ))
                .await;
            }
            Ok(meta) => match std::fs::read(&host) {
                Ok(bytes) => {
                    let name = host.rsplit('/').next().unwrap_or("file").to_owned();
                    self.views
                        .send(SessionEvent::Upload {
                            name,
                            bytes,
                            caption: format!(
                                "`{}` {} bytes",
                                self.display_path(wanted),
                                meta.len()
                            ),
                        })
                        .await;
                }
                Err(error) => {
                    self.say(&format!("could not read `{wanted}`: {error}"))
                        .await;
                }
            },
            Err(error) => {
                self.say(&format!("could not read `{wanted}`: {error}"))
                    .await;
            }
        }
    }

    /// Reads anything the agent wrote to its notes files and stores it.
    fn harvest_memory(&mut self) {
        let Some(memory) = self.options.memory.clone() else {
            return;
        };

        // Attributed to whoever spoke this turn, not to the session's owner:
        // in a shared thread the facts being recorded are about the person
        // talking.
        let about = self
            .last_speaker_id
            .clone()
            .unwrap_or_else(|| self.options.owner_id.clone());
        let stored = self.harvest(&memory, crate::memory::store::NOTES_FILENAME, true, &about)
            + self.harvest(
                &memory,
                crate::memory::store::PROJECT_NOTES_FILENAME,
                false,
                &self.options.project.name,
            );

        if stored > 0 {
            self.log.info(
                "recorded facts from a turn",
                &fields([
                    #[allow(clippy::cast_possible_wrap)]
                    ("stored", LogValue::from(stored as i64)),
                    ("about", LogValue::from(about.as_str())),
                ]),
            );
        }
    }

    /// Reads one notes file and stores what it holds.
    ///
    /// The file is emptied rather than deleted, so the agent's next append
    /// lands in a file it already knows exists and no line is ever ingested
    /// twice.
    fn harvest(
        &self,
        memory: &MemoryStore,
        filename: &str,
        user_scope: bool,
        subject: &str,
    ) -> usize {
        let Ok(contents) =
            std::fs::read_to_string(std::path::Path::new(&self.options.state_dir).join(filename))
        else {
            return 0;
        };

        let mut stored = 0;
        for fact in parse_notes(&contents) {
            let scope = if user_scope {
                crate::memory::store::Scope::User
            } else {
                crate::memory::store::Scope::Project
            };
            if memory
                .remember(
                    scope,
                    subject,
                    &fact,
                    &self.options.id,
                    crate::log::now_ms(),
                )
                .unwrap_or(false)
            {
                stored += 1;
            }
        }
        // A fact offered again is deduplicated, so a failure to empty the
        // file is not worth failing on.
        let _ = std::fs::write(
            std::path::Path::new(&self.options.state_dir).join(filename),
            "",
        );
        stored
    }

    /// True when this account may change what the session is doing.
    fn may_control(&self, author_id: &str) -> bool {
        author_id == self.options.owner_id
            || self.options.operator_ids.contains(&author_id.to_owned())
    }

    /// True when this account may prompt the agent and read the project.
    fn may_take_part(&self, author_id: &str) -> bool {
        self.may_control(author_id) || self.guests.contains(author_id)
    }

    fn not_invited(&self) -> String {
        format!(
            "<@{}> has not invited you to this thread; they can with `!allow`",
            self.options.owner_id
        )
    }

    /// Notes that somebody asked for something.
    async fn note_prompt(&self, author: &str, text: &str, id: Option<&str>) {
        self.views
            .send(SessionEvent::Prompt {
                author: author.to_owned(),
                text: text.to_owned(),
                id: id.map(str::to_owned),
                withdrawn: false,
            })
            .await;
    }

    /// Builds the delegations one turn may make, when a model is configured.
    ///
    /// The endpoint is the provider's own, so the cheaper model is reached
    /// with the same credential over the same connection as the session's.
    fn new_delegations(&self) -> Option<TurnDelegations<SessionSources, HttpSender>> {
        let delegate = self.options.config.agent.delegate.as_ref()?;
        let base_url = delegate
            .base_url
            .clone()
            .or_else(|| self.options.delegate_base_url.clone())?;
        let scheduler = Arc::clone(&self.options.scheduler);
        Some(TurnDelegations::new(
            &self.options.id,
            Endpoint {
                base_url,
                model: delegate.model.clone(),
                credential: self.options.config.agent.credential.clone(),
            },
            scheduler,
            Arc::new(SessionSources {
                project_root: self.options.project.path.clone(),
                outputs: Arc::clone(&self.outputs),
                project_path: self.options.project.path.clone(),
            }),
            delegate.deadline_ms,
            delegate.per_turn as usize,
            Arc::new(HttpSender),
        ))
    }

    async fn run_command(&mut self, word: &str, rest: &str, message: IncomingMessage) {
        self.replying_to = Some(word.to_owned());
        self.answer_command(word, rest, &message).await;
        self.replying_to = None;
    }

    /// Who or what a memory command was aimed at.
    fn memory_subject(&self, rest: &str, asked_by: &str) -> Option<(bool, String, String)> {
        let trimmed = rest.trim();
        if trimmed.is_empty() {
            return Some((true, asked_by.to_owned(), format!("<@{asked_by}>")));
        }
        if trimmed.to_lowercase() == "project" {
            return Some((
                false,
                self.options.project.name.clone(),
                format!("`{}`", self.options.project.name),
            ));
        }
        parse_user_id(trimmed).map(|id| (true, id.clone(), format!("<@{id}>")))
    }

    /// Reads back what the agent is told before it answers.
    async fn report_facts(&mut self, rest: &str, message: &IncomingMessage) {
        let Some(memory) = self.options.memory.as_ref() else {
            self.say("nothing is remembered on this host").await;
            return;
        };
        let Some((user_scope, subject, label)) = self.memory_subject(rest, &message.author_id)
        else {
            self.refuse(
                message,
                "say who, as `!facts @somebody`, or `!facts project`",
            )
            .await;
            return;
        };

        let scope = if user_scope {
            crate::memory::store::Scope::User
        } else {
            crate::memory::store::Scope::Project
        };
        let facts = memory
            .facts_for(scope, &subject, i64::MAX)
            .unwrap_or_default();
        if facts.is_empty() {
            self.say(&format!("nothing is remembered about {label}"))
                .await;
            return;
        }
        // Numbered the way they are held, so what a person reads back is what
        // the agent was given, newest first.
        let lines = facts
            .iter()
            .map(|fact| format!("- {}", fact.fact))
            .collect::<Vec<_>>()
            .join("\n");
        self.say(&format!("remembered about {label}:\n{lines}"))
            .await;
    }

    /// Drops what is remembered, which changes every later session too.
    async fn forget_facts(&mut self, rest: &str, message: &IncomingMessage) {
        let Some(memory) = self.options.memory.as_ref() else {
            self.say("nothing is remembered on this host").await;
            return;
        };
        if rest.trim().is_empty() {
            self.refuse(
                message,
                "say who, as `!forget @somebody`, or `!forget project`",
            )
            .await;
            return;
        }
        let Some((user_scope, subject, label)) = self.memory_subject(rest, &message.author_id)
        else {
            self.refuse(
                message,
                "say who, as `!forget @somebody`, or `!forget project`",
            )
            .await;
            return;
        };

        let scope = if user_scope {
            crate::memory::store::Scope::User
        } else {
            crate::memory::store::Scope::Project
        };
        let gone = memory.forget(scope, &subject).unwrap_or(0);
        self.react(&message.id, ReactionOutcome::Accepted).await;
        self.say(&if gone == 0 {
            format!("nothing was remembered about {label}")
        } else {
            format!(
                "forgot {gone} fact{} about {label}",
                if gone == 1 { "" } else { "s" }
            )
        })
        .await;
    }

    /// The provider this session runs on: what was asked for, else
    /// configured.
    fn provider(&self) -> String {
        self.switched
            .as_ref()
            .map(|(provider, _)| provider.clone())
            .or_else(|| {
                self.options
                    .chosen
                    .as_ref()
                    .and_then(|chosen| chosen.provider.clone())
            })
            .unwrap_or_else(|| self.options.config.agent.provider.clone())
    }

    /// The model this session runs on, on that provider.
    fn model(&self) -> Option<String> {
        self.switched
            .as_ref()
            .map(|(_, model)| model.clone())
            .or_else(|| {
                self.options
                    .chosen
                    .as_ref()
                    .map(|chosen| chosen.model.clone())
            })
            .or_else(|| self.options.config.agent.model.clone())
    }

    // One arm per command keeps the switch readable, as the original's
    // switch does.
    #[allow(clippy::too_many_lines)]
    async fn answer_command(&mut self, word: &str, rest: &str, message: &IncomingMessage) {
        let access = crate::session::commands::COMMANDS
            .iter()
            .find(|(name, _)| *name == word)
            .map_or(
                crate::session::commands::CommandAccess::Owner,
                |(_, meta)| meta.access,
            );
        let standing = crate::session::commands::Standing {
            is_owner: self.may_control(&message.author_id),
            is_guest: self.guests.contains(&message.author_id),
        };
        if !may_run(access, standing) {
            let why = if access == crate::session::commands::CommandAccess::Owner {
                format!(
                    "only <@{}>, who started this session, can use {word}",
                    self.options.owner_id
                )
            } else {
                self.not_invited()
            };
            self.refuse(message, &why).await;
            return;
        }

        match word {
            "!stop" => {
                self.react(&message.id, ReactionOutcome::Accepted).await;
                self.note_command(message, "!stop").await;
                self.end_because(
                    EndReason::Stopped,
                    &format!(
                        "this session ended ({})",
                        end_reason_name(EndReason::Stopped)
                    ),
                )
                .await;
            }
            "!interrupt" => {
                if self.ticket.is_none() {
                    self.say("there is nothing running to interrupt").await;
                    return;
                }
                // Asking twice is asking for the same thing. Each ask used to
                // start its own wait, and the first of them to run out force
                // stopped the session, so hurrying it along was what ended it.
                if self.abort_in_flight {
                    self.react(&message.id, ReactionOutcome::Accepted).await;
                    self.say(
                        "already interrupting; waiting for the agent to confirm. `!stop` ends the session",
                    )
                    .await;
                    return;
                }
                self.aborting = true;
                self.react(&message.id, ReactionOutcome::Accepted).await;
                self.note_command(message, "!interrupt").await;
                self.abort();
            }
            "!allow" | "!deny" => {
                let target = parse_user_id(rest);
                let Some(target) = target else {
                    self.say(&format!("say who, as `{word} @user`")).await;
                    return;
                };
                if target == self.options.owner_id {
                    self.say("the owner already takes part in their own thread")
                        .await;
                    return;
                }

                if word == "!allow" {
                    self.guests.insert(target.clone());
                } else {
                    self.guests.remove(&target);
                }
                if let Some(changed) = &self.options.on_guests_changed {
                    let list = self.guests.iter().cloned().collect::<Vec<_>>();
                    changed(&list);
                }

                self.react(&message.id, ReactionOutcome::Accepted).await;
                self.say(&if word == "!allow" {
                    format!("<@{target}> can now prompt this session and read its project")
                } else {
                    format!("<@{target}> can no longer take part in this thread")
                })
                .await;
            }
            "!guests" => {
                let guests = self.guests.iter().cloned().collect::<Vec<_>>();
                self.say(&if guests.is_empty() {
                    format!(
                        "only <@{}> takes part in this thread",
                        self.options.owner_id
                    )
                } else {
                    format!(
                        "taking part: <@{}> and {}",
                        self.options.owner_id,
                        guests
                            .iter()
                            .map(|id| format!("<@{id}>"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .await;
            }
            "!facts" => self.report_facts(rest, message).await,
            "!forget" => self.forget_facts(rest, message).await,
            "!pwd" => {
                self.say(&format!(
                    "`{}` at `{}`",
                    self.options.project.name, self.options.project.path
                ))
                .await;
            }
            "!ls" | "!cat" => self.read_path(rest).await,
            "!file" => {
                self.react(&message.id, ReactionOutcome::Accepted).await;
                self.upload_file(rest).await;
            }
            "!pr" => {
                self.note_pull_request_asked(message);
                self.open_pull_request_command(rest, message).await;
            }
            "!compact" => self.compact_conversation(message).await,
            "!model" => self.switch_model(rest, message).await,
            "!help" => self.say(&help_text()).await,
            "!then" => {
                if rest.trim().is_empty() {
                    self.say("say what to hold, as `!then <instruction>`").await;
                    return;
                }
                // With nothing running there is nothing to wait for, so it
                // starts a turn rather than being refused for asking at the
                // wrong moment.
                self.submit_prompt(rest, message.clone(), true, Attached::default())
                    .await;
            }
            "!steer" => {
                if rest.trim().is_empty() {
                    self.say("say what to steer towards, as `!steer <instruction>`")
                        .await;
                    return;
                }
                if self.ticket.is_none() {
                    self.say("there is no running turn to steer; send it as an ordinary message")
                        .await;
                    return;
                }
                self.note_command(message, &format!("!steer {rest}")).await;
                if let Some(client) = &self.client {
                    client.steer(rest, None);
                }
                self.react(&message.id, ReactionOutcome::Accepted).await;
            }
            _ => {
                let said = self.describe_status();
                self.say(&said).await;
            }
        }
    }

    /// Shows which models this session can run on, or moves it to one.
    async fn switch_model(&mut self, rest: &str, message: &IncomingMessage) {
        // A short name is what somebody types here too, so it stands for the
        // same model it would have at the start of a session.
        let wanted = expand_alias(rest.trim(), &self.options.config.agent.aliases);
        let available = &self.options.available_models;

        if wanted.is_empty() {
            let running = self
                .usage
                .model
                .clone()
                .or_else(|| self.options.config.agent.model.clone())
                .unwrap_or_else(|| "the provider default".to_owned());
            self.say(&if available.is_empty() {
                format!("this session runs on `{running}`; the host lists no others to switch to")
            } else {
                let mut lines = vec![format!(
                    "this session runs on `{running}`. Switch with `!model <name>`:"
                )];
                lines.extend(available.iter().map(|model| format!("  {model}")));
                lines.join("\n")
            })
            .await;
            return;
        }

        if self.ticket.is_some() {
            self.say("a turn is running; wait for it, or stop it with `!interrupt`")
                .await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        }

        // Refused rather than passed through, so a typo becomes a message
        // here instead of a turn that fails against the provider later.
        if !available.is_empty() && !available.contains(&wanted) {
            self.say(&format!(
                "this host does not list a model called `{wanted}`"
            ))
            .await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        }

        let sent = self
            .client
            .as_ref()
            .is_some_and(|client| client.set_model(&self.provider(), &wanted));
        if !sent {
            self.say("the agent is not accepting anything further; this session has ended")
                .await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        }

        let provider = self.provider();
        self.switched = Some((provider.clone(), wanted.clone()));
        if let Some(changed) = &self.options.on_model_changed {
            changed(&provider, &wanted);
        }

        self.note_command(message, &format!("!model {wanted}"))
            .await;
        self.say(&connection_line(&format!(
            "this session now runs on `{wanted}`, keeping what was said"
        )))
        .await;
        self.react(&message.id, ReactionOutcome::Accepted).await;
    }

    // Token totals sit far below f64's exact range.
    #[allow(clippy::cast_precision_loss)]
    fn describe_status(&self) -> String {
        let mut lines = vec![
            format!("project: {}", self.options.project.name),
            format!(
                "state: {}",
                if self.ticket.is_some() {
                    "running a turn"
                } else {
                    "idle"
                }
            ),
            format!(
                "turns in flight across all sessions: {}",
                self.options.scheduler.turns_in_flight()
            ),
            format!("prompts waiting: {}", self.options.scheduler.queue_length()),
        ];

        if self.delegated_asked > 0 {
            lines.push(format!(
                "delegated: {} of {} asked, {} token(s) spent, {} kept out of this conversation",
                self.delegated_answered,
                self.delegated_asked,
                self.delegated_tokens,
                byte_count(self.delegated_kept_out as f64),
            ));
        }
        lines.join("\n")
    }

    /// Aborts the running turn, force stopping if the agent will not confirm.
    fn abort(&mut self) {
        self.abort_in_flight = true;
        if let Some(client) = &self.client {
            client.abort();
        }
        self.abort_timer = Some(self.timers.set_timeout(
            SessionTimer::AbortDeadline,
            self.options.config.timeouts.abort_ms,
        ));
    }

    /// Begins answering the delegations the agent asks for.
    fn start_delegating(&mut self) {
        if self.options.config.agent.delegate.is_none() {
            return;
        }

        let watcher = Delegating::new(
            std::path::Path::new(&self.options.state_dir),
            self.log.clone(),
            {
                let commands = self.commands.clone();
                Arc::new(move |outcome: Reported| {
                    let _ = commands.try_send(Signal::DelegationReported(outcome));
                })
            },
        );
        self.watcher = Some(watcher);
        self.schedule_delegating();
    }

    /// Reports a delegation and keeps a running total of what it bought.
    async fn note_delegation(&mut self, reported: Reported) {
        self.delegated_asked += 1;

        if let DelegationOutcome::Refused(refused) = &reported.outcome {
            self.views
                .send(SessionEvent::Delegation {
                    delegated: Delegated {
                        question: reported.asked,
                        refused: Some(refused.refused.clone()),
                        ..Default::default()
                    },
                })
                .await;
            return;
        }

        let DelegationOutcome::Ready(answer) = &reported.outcome else {
            return;
        };
        self.delegated_answered += 1;
        self.delegated_tokens += answer.tokens.unwrap_or(0);
        self.delegated_kept_out += answer.kept_out;

        self.views
            .send(SessionEvent::Delegation {
                delegated: Delegated {
                    question: reported.asked,
                    model: Some(answer.model.clone()),
                    describes: Some(answer.describes.clone()),
                    answer: Some(answer.text.clone()),
                    tokens: answer.tokens,
                    kept_out: Some(answer.kept_out),
                    ..Default::default()
                },
            })
            .await;
    }

    /// Records what the project already held, then watches how much the
    /// session adds to it.
    async fn start_disk_watch(&mut self) {
        let Some(budget) = parse_size(&self.options.config.sandbox.disk) else {
            return;
        };
        if budget == 0 {
            return;
        }

        self.disk_baseline = self.measure_disk().await;
        if self.ended {
            return;
        }
        self.disk_last_at = crate::log::now_ms();
        // The first interval is short on purpose: nothing has been observed
        // yet, so there is no rate to pace against, and waiting the
        // configured interval is exactly the window a fast writer would use
        // to pass the budget.
        self.disk_timer = Some(self.timers.set_timeout(SessionTimer::Disk, MIN_CHECK_MS));
    }

    // Keeping the signature async keeps every caller's await uniform.
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    async fn measure_disk(&self) -> u64 {
        let project = tree_bytes(&self.options.project.path).unwrap_or(0);
        let state = tree_bytes(&self.options.state_dir).unwrap_or(0);
        project + state
    }

    /// Ends the session once it has written more than its budget.
    ///
    /// This is a measurement rather than a boundary: the agent can exceed the
    /// budget between two checks, and nothing here can stop it mid-write.
    /// What it does guarantee is that a session filling a disk stops rather
    /// than continuing until the disk is full.
    // Byte counts sit far below f64's exact range, and the log fields hold
    // them as milliseconds-epoch-sized integers.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_wrap)]
    async fn check_disk(&mut self) {
        if self.ended {
            return;
        }

        let budget = parse_size(&self.options.config.sandbox.disk).unwrap_or(0);
        let written = self.measure_disk().await.saturating_sub(self.disk_baseline);
        if self.ended {
            return;
        }

        match verdict(written, budget) {
            crate::session::disk::Verdict::Over => {
                self.log.warn(
                    "session stopped for writing past its disk budget",
                    &fields([
                        ("written", LogValue::from(written as i64)),
                        ("budget", LogValue::from(budget as i64)),
                    ]),
                );
                self.end_because(
                    EndReason::ResourceLimit,
                    &format!(
                        "this session stopped after writing {}, past its {} budget",
                        byte_count(written as f64),
                        byte_count(budget as f64)
                    ),
                )
                .await;
                return;
            }
            crate::session::disk::Verdict::Close if !self.disk_warned => {
                self.disk_warned = true;
                self.views
                    .send(SessionEvent::Notice {
                        text: format!(
                            "this session has written {} of its {} budget, and ends if it passes it",
                            byte_count(written as f64),
                            byte_count(budget as f64)
                        ),
                        level: NoticeLevel::Warning,
                    })
                    .await;
            }
            _ => {}
        }

        let now = crate::log::now_ms();
        let next = next_check_ms(
            written,
            self.disk_last_written,
            budget,
            (now - self.disk_last_at).max(0).cast_unsigned(),
            self.options.config.sandbox.disk_check_ms,
        );
        self.disk_last_written = written;
        self.disk_last_at = now;
        self.disk_timer = Some(self.timers.set_timeout(SessionTimer::Disk, next));
    }

    /// Records a command that changed the agent's course.
    async fn note_command(&self, message: &IncomingMessage, text: &str) {
        self.note_prompt(&display_name(message), text, None).await;
    }

    /// Records that a pull request was asked for, and by whom, for a resume.
    fn note_pull_request_asked(&self, message: &IncomingMessage) {
        let directory = prepare_record_dir(&self.options.state_dir);
        let written = std::fs::write(
            std::path::Path::new(&directory).join(ASKED_FILENAME),
            format!(
                "{}\n{}\n",
                message.author_id,
                message.author_name.clone().unwrap_or_default()
            ),
        );
        if let Err(error) = written {
            self.log.warn(
                "could not record that a pull request was asked for",
                &fields([("detail", LogValue::from(error.to_string()))]),
            );
        }
    }

    /// Opens the pull request the agent asked for, if it asked for one.
    async fn open_requested_pull_request(&mut self) {
        let path = std::path::Path::new(&self.options.state_dir).join(REQUEST_FILENAME);
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return;
        };

        if let Err(error) = std::fs::remove_file(&path) {
            self.log.warn(
                "could not clear the pull request request",
                &fields([("detail", LogValue::from(error.to_string()))]),
            );
        }

        if self.who_asked().is_none() {
            self.log
                .warn("ignored a pull request nobody asked for", &fields([]));
            self.say(
                "a pull request was asked for by the agent, not by anyone here, so it was ignored. Ask for one and it will go through",
            )
            .await;
            return;
        }

        let mut lines = contents.split('\n');
        let title = lines.next().unwrap_or_default().trim().to_owned();
        let repository = lines
            .map(|line| line.trim().strip_prefix("repository:").map(str::trim))
            .find_map(|name| name.filter(|name| !name.is_empty()))
            .map(str::to_owned);

        if title.is_empty() {
            self.say("a pull request was asked for without a title, so none was opened")
                .await;
            return;
        }
        self.open_pull_request_now(&title, repository.as_deref())
            .await;
    }

    /// Opens one on request, reporting the outcome on the message that asked.
    async fn open_pull_request_command(&mut self, title: &str, message: &IncomingMessage) {
        if self.options.config.github.is_none() {
            self.say("no GitHub identity is configured, so there is nowhere to open one")
                .await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        }
        if title.trim().is_empty() {
            self.say("say what to call it, as `!pr <title>`").await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        }

        self.react(&message.id, ReactionOutcome::Accepted).await;
        let opened = self.open_pull_request_now(title.trim(), None).await;
        self.react(
            &message.id,
            if opened {
                ReactionOutcome::Succeeded
            } else {
                ReactionOutcome::Failed
            },
        )
        .await;
    }

    /// Opens one, reporting either the address or why it did not.
    async fn open_pull_request_now(&mut self, title: &str, repository: Option<&str>) -> bool {
        let (Some(github), Some(open)) = (
            self.options.config.github.clone(),
            self.options.open_pull_request.clone(),
        ) else {
            self.say("no GitHub identity is configured, so there is nowhere to open one")
                .await;
            return false;
        };

        match open(pr::Request {
            github,
            project_path: self.options.project.path.clone(),
            repository: repository.map(str::to_owned),
            title: title.to_owned(),
            requested_by: self.requested_by(),
            links: self.session_links(),
        })
        .await
        {
            Ok(url) => {
                self.say(&connection_line(&format!("opened {url}"))).await;
                true
            }
            Err(error) => {
                self.log.warn(
                    "could not open a pull request",
                    &fields([("detail", LogValue::from(error.to_string()))]),
                );
                self.say(&reason(&error)).await;
                false
            }
        }
    }

    /// Summarises the conversation so far, freeing context to carry on in.
    ///
    /// Refused while a turn is running: compacting underneath a turn would
    /// change the conversation the agent is part way through answering about.
    async fn compact_conversation(&mut self, message: &IncomingMessage) {
        if self.ticket.is_some() {
            self.say("a turn is running; wait for it, or stop it with `!interrupt`")
                .await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        }

        let Some(client) = self.client.clone() else {
            self.say("this session has no agent to compact").await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        };

        self.react(&message.id, ReactionOutcome::Accepted).await;
        self.note_command(message, "!compact").await;
        match client
            .compact(self.options.config.timeouts.question_ms)
            .await
        {
            Ok(answer) => {
                self.say(&compaction_line(&answer)).await;
                self.react(&message.id, ReactionOutcome::Succeeded).await;
            }
            Err(error) => {
                self.log.warn(
                    "compaction failed",
                    &fields([("detail", LogValue::from(error.clone()))]),
                );
                self.say(&format!("compaction did not finish: {error}"))
                    .await;
                self.react(&message.id, ReactionOutcome::Failed).await;
            }
        }
    }

    /// Turns a message down, explaining the first time and reacting every
    /// time.
    ///
    /// Answered as a reply rather than said: a refusal is addressed to the
    /// person who tripped it, not to the session. Posting it would record it
    /// and show it in an interface as though the agent had said it, which is
    /// both untrue and noise in a conversation the refused message never
    /// joined.
    async fn refuse(&mut self, message: &IncomingMessage, why: &str) {
        if !self.explained.contains(&message.author_id) {
            self.explained.insert(message.author_id.clone());
            self.views
                .send(SessionEvent::Reply {
                    text: why.to_owned(),
                    command: "refused".to_owned(),
                })
                .await;
        }
        self.react(&message.id, ReactionOutcome::Failed).await;
    }

    fn reset_idle_timer(&mut self) {
        self.last_active = crate::log::now_ms();
        if let Some(handle) = self.idle_timer.take() {
            self.timers.clear_timeout(handle);
        }
        self.idle_timer = Some(
            self.timers
                .set_timeout(SessionTimer::Idle, self.options.config.timeouts.idle_ms),
        );
    }

    /// Ends the session, saying so only when there is something to say.
    ///
    /// A session that idles out, or that goes down with the daemon, is picked
    /// up again by the next message in its thread, and the resumed session
    /// announces itself. Announcing the pause as well would be a message in
    /// every thread that says nothing a reader has to act on.
    ///
    /// A failure is different: it stopped part way through something, and
    /// somebody should know why. And a thread archived from outside is left
    /// alone, because posting into it would open it again, which is the
    /// opposite of what whoever archived it asked for.
    async fn end_because(&mut self, why: EndReason, detail: &str) {
        if self.ended {
            return;
        }

        if !is_quiet_ending(why) {
            self.views
                .send(SessionEvent::Notice {
                    text: connection_line(detail),
                    level: NoticeLevel::Ended,
                })
                .await;
        }
        self.finish(why).await;
    }

    /// Tears everything down exactly once: pending dialogs, the turn slot,
    /// the queued prompts, the sandbox, and the session reservation.
    async fn finish(&mut self, why: EndReason) {
        if self.ended {
            return;
        }
        self.ended = true;

        for timer in [
            self.idle_timer.take(),
            self.disk_timer.take(),
            self.delegating_timer.take(),
            self.abort_timer.take(),
        ]
        .into_iter()
        .flatten()
        {
            self.timers.clear_timeout(timer);
        }

        if let Some(client) = self.client.as_ref() {
            client.cancel_dialogs();
        }
        let interrupted = matches!(why, EndReason::Stopped | EndReason::Unresponsive);
        self.settle_turn(if interrupted {
            ReactionOutcome::Interrupted
        } else {
            ReactionOutcome::Failed
        })
        .await;
        self.options.scheduler.cancel_session(&self.options.id);

        if let Some(sandbox) = &self.sandbox {
            let _ = (sandbox.stop)().await;
        }

        self.options.scheduler.release_session();
        self.views.send(SessionEvent::Waiting { text: None }).await;
        self.views.send(SessionEvent::Close { reason: why }).await;
        self.log.info(
            "session ended",
            &fields([("reason", LogValue::from(end_reason_name(why)))]),
        );
        (self.options.on_ended)(why);
    }
}

/// An attachment note and the images to hand over with it.
#[derive(Default)]
struct Attached {
    note: String,
    images: Vec<AgentImage>,
}

/// An image as the prompt carries it.
fn image_value(image: &AgentImage) -> Value {
    json!({
        "type": image.r#type,
        "data": image.data,
        "mimeType": image.mime_type,
    })
}

/// Whoever asked for a pull request.
struct Asked {
    id: String,
    name: Option<String>,
}

/// The name a message is shown under.
fn display_name(message: &IncomingMessage) -> String {
    message
        .author_name
        .clone()
        .unwrap_or_else(|| message.author_id.clone())
}

/// The name an end reason is written with.
fn end_reason_name(why: EndReason) -> &'static str {
    match why {
        EndReason::Stopped => "stopped",
        EndReason::Unresponsive => "unresponsive",
        EndReason::Idle => "idle",
        EndReason::Crashed => "crashed",
        EndReason::ResourceLimit => "resource limit",
        EndReason::StartupFailed => "startup failed",
        EndReason::Shutdown => "shutdown",
        EndReason::ThreadArchived => "thread archived",
        EndReason::ProtocolViolation => "protocol violation",
    }
}

/// Fetches an attachment the way the chat service handed it over.
fn default_fetch_attached(url: String) -> FetchBox {
    Box::pin(async move {
        let ok = default_fetch(&url).await?;
        Ok(ok)
    })
}

/// Fetches an attachment the way the chat service handed it over.
async fn default_fetch(url: &str) -> Result<Vec<u8>, String> {
    let response = reqwest::get(url).await.map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "the chat service answered {}",
            response.status().as_u16()
        ));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "session/tests.rs"]
mod tests;
