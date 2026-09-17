//! Session registry and lifecycle across the whole daemon.
//!
//! Owns the thread-to-session binding and guarantees it is one to one: a
//! thread belongs to exactly one session for its lifetime and is never
//! reused, so a message can only ever reach the session it was written to.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use crate::admission::scheduler::Scheduler;
use crate::chat::render::thread_name;
use crate::config::redact::secret_values;
use crate::config::schema::Config;
use crate::log::{LogValue, Logger, fields};
use crate::memory::store::MemoryStore;
use crate::sandbox::backend::{
    CapabilityReport, SandboxLaunch, SandboxLaunchError, SandboxUnavailableError,
};
use crate::session::event::EndReason;
use crate::session::ids::{TOKEN_LENGTH, session_id, session_token};
use crate::session::model::{ChosenModel, expand_alias, resolve_model, select_model};
use crate::session::pr;
use crate::session::projects::{ProjectSelection, ensure_project_directory, select_project};
use crate::session::record::{prepare_record_dir, record_dir, withdraw_from_record};
use crate::session::redacted::Redacting;
use crate::session::registry::{ThreadRecord, ThreadRegistry};
use crate::session::rules::house_rules_text;
pub use crate::session::session::Unavailable;
use crate::session::session::{
    DescribeImages, IncomingMessage, Launcher, OnGuestsChanged, OnModelChanged, OpenPullRequest,
    RunningBox, SessionHandle, SessionOptions,
};
use crate::session::transcript::{TRANSCRIPT_FILENAME, Transcript};
use crate::session::views::{Attached, DEFAULT_TRANSCRIPT_LIMIT, Held, SessionView, ViewFanOut};

/// How a request to start a session turned out.
pub enum StartOutcome {
    Started { session: SessionHandle },
    Refused { reason: String },
}

impl StartOutcome {
    /// Whether a session came of it.
    pub fn is_started(&self) -> bool {
        matches!(self, StartOutcome::Started { .. })
    }

    /// The refusal's words, for a test's sake.
    #[allow(
        dead_code,
        reason = "read by this module's tests, which assert on state the daemon never asks for"
    )]
    pub fn refused_reason(&self) -> &str {
        match self {
            StartOutcome::Started { .. } => "",
            StartOutcome::Refused { reason } => reason,
        }
    }

    /// The session, when there is one.
    #[allow(
        dead_code,
        reason = "read by this module's tests, which assert on state the daemon never asks for"
    )]
    pub fn session(&self) -> Option<&SessionHandle> {
        match self {
            StartOutcome::Started { session } => Some(session),
            StartOutcome::Refused { .. } => None,
        }
    }
}

/// A thread creation, as a boxed future.
pub type MadeThread = Pin<Box<dyn Future<Output = Result<CreatedThread, String>> + Send>>;
/// A thread view lookup, as a boxed future.
pub type FoundView = Pin<Box<dyn Future<Output = Option<Arc<dyn SessionView>>> + Send>>;

/// One thread made for a session: where it lives, and the view to show it.
pub struct CreatedThread {
    pub id: String,
    pub view: Arc<dyn SessionView>,
}

/// What the manager needs in order to create a session's thread.
pub trait ThreadFactory: Send + Sync {
    /// Creates the thread for a session.
    ///
    /// Fails when the chat service refuses, in which case no sandbox is
    /// started: an agent nobody can see or stop is worse than no agent.
    fn create(self: Arc<Self>, message: IncomingMessage, name: String) -> MadeThread;

    /// Creates a thread with no message to hang it on, by posting one first.
    ///
    /// A session started from the interface still gets a thread, so a turn
    /// finishing still reaches a phone wherever the work began.
    fn open(self: Arc<Self>, name: String, opener: String) -> MadeThread;

    /// A view for a thread that already exists, so a session can be resumed
    /// into it after a restart. None when the thread cannot be reached.
    fn port_for(self: Arc<Self>, thread_id: String) -> FoundView;

    /// Forgets a thread once its session has ended.
    fn release(&self, _thread_id: &str) {}
}

/// What the manager needs from the sandbox layer.
pub trait SandboxPool: Send + Sync {
    /// Checks that this backend can run here and reports what it can enforce.
    fn probe(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<CapabilityReport, SandboxUnavailableError>> + Send + '_>>;

    /// Starts one session's sandbox.
    ///
    /// Takes the pool by value so the returned future is `Send` on its own,
    /// which is what a session's launcher holds.
    fn launch(
        self: Arc<Self>,
        launch: SandboxLaunch,
    ) -> Pin<Box<dyn Future<Output = Result<RunningBox, SandboxLaunchError>> + Send>>;

    /// Names the sandboxes a previous run left behind.
    fn list_orphans(&self) -> Pin<Box<dyn Future<Output = Vec<String>> + Send + '_>>;

    /// Removes the named sandboxes, returning how many went.
    fn remove_orphans<'a>(
        &'a self,
        names: &'a [String],
    ) -> Pin<Box<dyn Future<Output = usize> + Send + 'a>>;
}

/// Options for the session manager.
pub struct ManagerOptions {
    pub config: Config,
    pub sandbox: Arc<dyn SandboxPool>,
    pub scheduler: Arc<Scheduler>,
    pub threads: Arc<dyn ThreadFactory>,
    pub registry: Arc<Mutex<ThreadRegistry>>,
    pub log: Logger,
    /// Injected so identifiers are predictable in tests.
    pub make_id: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    /// Why a prompt cannot run yet, or none when it can.
    ///
    /// Held here rather than by a session so one answer serves every
    /// session, and so a refusal happens before a thread is opened.
    pub unavailable: Option<Unavailable>,
    /// Who may control any session, when it is not the configured list.
    pub operator_ids: Option<Vec<String>>,
    /// Memory, or none when it is switched off.
    pub memory: Option<Arc<MemoryStore>>,
    /// Describes an image for a session whose model cannot be shown one.
    pub describe_images: Option<DescribeImages>,
    /// Where the interface is published, when it is.
    pub public_url: Option<String>,
    /// Models this host knows the provider serves, for `!model`.
    pub available_models: Vec<String>,
    /// Where a delegated question is sent, read from the host's model store.
    pub delegate_base_url: Option<String>,
    /// Injected so record timestamps are predictable in tests.
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
}

/// The parts of a session's options that do not depend on which session it
/// is, gathered once.
struct Shared {
    scheduler: Arc<Scheduler>,
    config: Config,
    log: Logger,
    operator_ids: Vec<String>,
    memory: Option<Arc<MemoryStore>>,
    describe_images: Option<DescribeImages>,
    public_url: Option<String>,
    available_models: Vec<String>,
    delegate_base_url: Option<String>,
    unavailable: Option<Unavailable>,
    launcher: Launcher,
    guild_id: Arc<Mutex<Option<String>>>,
    registry: Arc<Mutex<ThreadRegistry>>,
    threads: Arc<dyn ThreadFactory>,
}

/// The book a session's ending has to reach, shared with its task.
struct ManagerState {
    by_thread: Mutex<HashMap<String, SessionHandle>>,
    ended_threads: Mutex<HashSet<String>>,
    views: Mutex<HashMap<String, Arc<ViewFanOut>>>,
}

/// Owns every live session and the mapping from threads to them.
pub struct SessionManager {
    options: ManagerOptions,
    shared: Arc<Shared>,
    state: Arc<ManagerState>,
    secrets: Vec<String>,
}

impl SessionManager {
    /// Builds a manager over the given connections and stores.
    pub fn new(options: ManagerOptions) -> Self {
        let secrets = secret_values(&options.config);
        let shared = Arc::new(Shared {
            scheduler: Arc::clone(&options.scheduler),
            config: options.config.clone(),
            log: options.log.clone(),
            operator_ids: options
                .operator_ids
                .clone()
                .unwrap_or_else(|| options.config.chat.operator_user_ids.clone()),
            memory: options.memory.clone(),
            describe_images: options.describe_images.clone(),
            public_url: options.public_url.clone(),
            available_models: options.available_models.clone(),
            delegate_base_url: options.delegate_base_url.clone(),
            unavailable: options.unavailable.clone(),
            launcher: {
                let sandbox = Arc::clone(&options.sandbox);
                Arc::new(move |launch: SandboxLaunch| Arc::clone(&sandbox).launch(launch))
            },
            guild_id: Arc::new(Mutex::new(None)),
            registry: Arc::clone(&options.registry),
            threads: Arc::clone(&options.threads),
        });
        Self {
            options,
            shared,
            state: Arc::new(ManagerState {
                by_thread: Mutex::new(HashMap::new()),
                ended_threads: Mutex::new(HashSet::new()),
                views: Mutex::new(HashMap::new()),
            }),
            secrets,
        }
    }

    /// Records the guild, once the gateway has resolved it.
    pub fn set_guild(&self, guild_id: String) {
        *self.shared.guild_id.lock().expect("the guild lock") = Some(guild_id);
    }

    /// Why a prompt on `provider` cannot run yet, or none when it can.
    pub async fn unavailable(&self, provider: &str) -> Option<String> {
        match &self.options.unavailable {
            Some(unavailable) => unavailable(provider).await,
            None => None,
        }
    }

    /// Every live session.
    pub fn sessions(&self) -> Vec<SessionHandle> {
        self.state
            .by_thread
            .lock()
            .expect("the thread map lock")
            .values()
            .cloned()
            .collect()
    }

    /// The session bound to a thread, if it is still live.
    pub fn for_thread(&self, thread_id: &str) -> Option<SessionHandle> {
        self.state
            .by_thread
            .lock()
            .expect("the thread map lock")
            .get(thread_id)
            .cloned()
    }

    /// The session with this identifier, if it is live.
    pub fn for_session(&self, session_id: &str) -> Option<SessionHandle> {
        self.sessions()
            .into_iter()
            .find(|session| session.id() == session_id)
    }

    /// Whether a turn is running for the named live session.
    ///
    /// The state lives on the session's fan-out, which the manager already
    /// holds, so the interface can list sessions without each one having to
    /// answer a question mid-turn.
    pub fn is_busy(&self, session_id: &str) -> bool {
        let views = self.state.views.lock().expect("the views map lock");
        views
            .get(session_id)
            .is_some_and(|fan_out| fan_out.state().busy)
    }

    /// True when the thread once held a session that has since ended.
    pub fn is_finished_thread(&self, thread_id: &str) -> bool {
        self.state
            .ended_threads
            .lock()
            .expect("the ended set lock")
            .contains(thread_id)
    }

    /// Removes sandboxes left behind by a previous run.
    ///
    /// Runs before the gateway accepts anything, so a crashed daemon cannot
    /// leave containers running against a project while a new daemon starts
    /// more.
    pub async fn sweep_orphans(&self) -> usize {
        let orphans = self.options.sandbox.list_orphans().await;
        if orphans.is_empty() {
            return 0;
        }
        let removed = self.options.sandbox.remove_orphans(&orphans).await;
        self.options.log.info(
            "removed sandboxes left by a previous run",
            &fields([
                ("found", LogValue::from(orphans.len())),
                ("removed", LogValue::from(removed)),
            ]),
        );
        removed
    }

    /// Starts a session for a message in the served channel.
    ///
    /// Capacity is reserved before the thread is created and released again
    /// on every failure path, so a refused or failed start cannot consume a
    /// slot.
    pub async fn start(&self, message: IncomingMessage) -> StartOutcome {
        let token = self.next_id();

        // A named session reaches the same directory every time the name is
        // used. An unnamed one works in a directory of its own, named after
        // the session.
        let project = select_project(&message.content, &self.options.config.project_root, &token);
        let id = session_id(&project, &token);
        self.launch(id, project, message, ThreadKind::Created).await
    }

    /// Starts a session that no message created.
    ///
    /// Used by the interface. The thread is opened rather than hung off an
    /// existing message, so a session started at a keyboard is still
    /// announced in the channel and still notifies a phone when it finishes.
    pub async fn start_detached(&self, request: DetachedRequest) -> StartOutcome {
        let token = self.next_id();
        let named = if request.project.trim().is_empty() {
            String::new()
        } else {
            format!("{}: ", request.project.trim())
        };
        let message = IncomingMessage {
            id: format!("web-{token}"),
            author_id: request.owner_id.clone(),
            author_name: request.owner_name.clone(),
            content: request.prompt.clone(),
            attachments: Vec::new(),
        };

        let project = select_project(
            &format!("{named}{}", request.prompt),
            &self.options.config.project_root,
            &token,
        );
        self.launch(
            session_id(&project, &token),
            project,
            message,
            ThreadKind::Opened,
        )
        .await
    }

    fn next_id(&self) -> String {
        match &self.options.make_id {
            Some(make) => make(),
            None => session_token(TOKEN_LENGTH),
        }
    }

    // One arm per step, in the order the original refuses and reserves.
    #[allow(clippy::too_many_lines)]
    async fn launch(
        &self,
        id: String,
        mut project: ProjectSelection,
        message: IncomingMessage,
        kind: ThreadKind,
    ) -> StartOutcome {
        // The opening message may name the model, because by the time there
        // is a thread to type `!model` in, the session has already started on
        // another. Read before the window is checked: which provider this
        // runs on decides whose window matters, and another provider's being
        // spent is not a reason to refuse work this one can do.
        let asked = select_model(&project.prompt);
        let known: Vec<&str> = [self.options.config.agent.provider.as_str()]
            .into_iter()
            .chain(
                self.options
                    .config
                    .agent
                    .providers
                    .keys()
                    .map(String::as_str),
            )
            .collect();
        let chosen = asked.value.as_ref().map(|value| {
            resolve_model(
                &expand_alias(value, &self.options.config.agent.aliases),
                &known,
            )
        });
        project.prompt = asked.prompt;

        // Before anything is reserved or created, so a window that is already
        // spent does not open a thread and start a sandbox only to fail on
        // its first turn.
        let provider_for_window = chosen
            .as_ref()
            .and_then(|chosen| chosen.provider.clone())
            .unwrap_or_else(|| self.options.config.agent.provider.clone());
        if let Some(spent) = self.unavailable(&provider_for_window).await {
            return StartOutcome::Refused { reason: spent };
        }

        // Refused before anything is reserved or created, so a project only
        // ever has one agent writing to it. Two would be editing one working
        // tree with neither able to see the other's changes.
        if let Some(busy) = self
            .sessions()
            .into_iter()
            .find(|other| other.project().path == project.path)
        {
            return StartOutcome::Refused {
                reason: format!(
                    "{} already has a live session ({}); continue there, or stop it first",
                    project.name,
                    busy.id()
                ),
            };
        }

        if self.options.scheduler.reserve_session().is_none() {
            return StartOutcome::Refused {
                reason: self.options.scheduler.session_refused_reason(),
            };
        }

        let state_dir = std::path::Path::new(&self.options.config.state_dir)
            .join(&id)
            .display()
            .to_string();

        // The agent stores its own state under a home inside the session's
        // state directory, so it is writable under a mapped user id.
        let home = std::path::Path::new(&state_dir).join("home");
        let thread = match std::fs::create_dir_all(&home)
            .map_err(|error| error.to_string())
            .and_then(|()| {
                ensure_project_directory(&project, &self.options.config.project_root)
                    .map_err(|error| error.to_string())
            }) {
            Err(error) => Err(error),
            Ok(()) => match kind {
                ThreadKind::Created => {
                    Arc::clone(&self.shared.threads)
                        .create(message.clone(), thread_name(&project.name, &project.prompt))
                        .await
                }
                ThreadKind::Opened => {
                    let name = thread_name(&project.name, &project.prompt);
                    Arc::clone(&self.shared.threads)
                        .open(
                            name.clone(),
                            format!("Session started from the interface: {name}"),
                        )
                        .await
                }
            },
        };
        let thread = match thread {
            Err(error) => {
                // No thread means no session and, deliberately, no sandbox:
                // starting one would leave an agent running that nobody could
                // see or stop.
                self.options.scheduler.release_session();
                let _ = std::fs::remove_dir_all(&state_dir);
                let _ = std::fs::remove_dir_all(record_dir(&state_dir));
                return StartOutcome::Refused {
                    reason: format!("no session was started: {error}"),
                };
            }
            Ok(thread) => thread,
        };

        let transcript = Transcript::new(
            std::path::Path::new(&prepare_record_dir(&state_dir)).join(TRANSCRIPT_FILENAME),
            Some(self.shared.log.clone()),
        );
        let fan_out = Arc::new(ViewFanOut::with_recorder(
            self.shared.log.clone(),
            DEFAULT_TRANSCRIPT_LIMIT,
            Some(Arc::new(transcript)),
        ));
        fan_out
            .clone()
            .attach(Arc::clone(&thread.view) as Arc<dyn SessionView>)
            .await;
        self.state
            .views
            .lock()
            .expect("the views map lock")
            .insert(id.clone(), Arc::clone(&fan_out));

        let session = SessionHandle::spawn(SessionOptions {
            id: id.clone(),
            project: project.clone(),
            chosen: chosen.clone(),
            state_dir: state_dir.clone(),
            views: Arc::new(Redacting::new(
                Arc::clone(&fan_out),
                self.reported_secrets(),
            )),
            launcher: Arc::clone(&self.shared.launcher),
            scheduler: Arc::clone(&self.shared.scheduler),
            config: self.shared.config.clone(),
            log: self.shared.log.clone(),
            timers: None,
            owner_id: message.author_id.clone(),
            owner_name: message.author_name.clone(),
            start_turn: None,
            open_pull_request: Some(daemon_open_pull_request()),
            thread_id: Some(thread.id.clone()),
            guild_id: self.shared.guild_id.lock().expect("the guild lock").clone(),
            public_url: self.shared.public_url.clone(),
            available_models: self.shared.available_models.clone(),
            delegate_base_url: self.shared.delegate_base_url.clone(),
            unavailable: self.shared.unavailable.clone(),
            operator_ids: self.shared.operator_ids.clone(),
            guest_ids: Vec::new(),
            on_guests_changed: Some(on_guests_changed(&self.shared.registry, &thread.id)),
            on_model_changed: Some(on_model_changed(&self.shared.registry, &thread.id)),
            memory: self.shared.memory.clone(),
            fetch_attachment: None,
            describe_images: self.shared.describe_images.clone(),
            resume: false,
            on_ended: on_ended(
                Arc::clone(&self.state),
                Arc::clone(&self.shared.registry),
                Arc::clone(&self.shared.threads),
                self.shared.log.clone(),
                thread.id.clone(),
                id.clone(),
            ),
        });

        self.state
            .by_thread
            .lock()
            .expect("the thread map lock")
            .insert(thread.id.clone(), session.clone());
        self.options
            .registry
            .lock()
            .expect("the registry lock")
            .remember(ThreadRecord {
                thread_id: thread.id.clone(),
                session_id: id.clone(),
                state_dir,
                project_name: project.name.clone(),
                project_path: project.path.clone(),
                owner_id: message.author_id.clone(),
                guests: Vec::new(),
                provider: chosen.as_ref().and_then(|chosen| chosen.provider.clone()),
                model: chosen.as_ref().map(|chosen| chosen.model.clone()),
                updated_at: self.now_ms(),
            });

        let mut first = message.clone();
        first.content = project.prompt;
        session.start(first).await;
        StartOutcome::Started { session }
    }

    /// Restarts a thread's session, continuing the agent conversation.
    ///
    /// A daemon restart ends every sandbox, but the agent's history lives in
    /// the session state directory, so a thread can be picked up where it
    /// stopped rather than being told it is over.
    // The resume mirrors the launch step for step, with the stored record
    // in place of the opening message's choices.
    #[allow(clippy::too_many_lines)]
    pub async fn resume(&self, thread_id: &str, message: IncomingMessage) -> StartOutcome {
        let record = self
            .options
            .registry
            .lock()
            .expect("the registry lock")
            .get(thread_id)
            .cloned();
        let Some(record) = record else {
            return StartOutcome::Refused {
                reason: "this thread is not one of mine to resume".to_owned(),
            };
        };
        if self
            .state
            .by_thread
            .lock()
            .expect("the thread map lock")
            .contains_key(thread_id)
        {
            return StartOutcome::Refused {
                reason: "this thread already has a live session".to_owned(),
            };
        }

        if let Some(busy) = self
            .sessions()
            .into_iter()
            .find(|other| other.project().path == record.project_path)
        {
            return StartOutcome::Refused {
                reason: format!(
                    "{} already has a live session ({}); continue there, or stop it first",
                    record.project_name,
                    busy.id()
                ),
            };
        }

        if self.options.scheduler.reserve_session().is_none() {
            return StartOutcome::Refused {
                reason: self.options.scheduler.session_refused_reason(),
            };
        }

        let Some(view) = Arc::clone(&self.options.threads)
            .port_for(thread_id.to_owned())
            .await
        else {
            self.options.scheduler.release_session();
            return StartOutcome::Refused {
                reason: "this thread could not be reopened".to_owned(),
            };
        };

        let transcript = Transcript::new(
            std::path::Path::new(&prepare_record_dir(&record.state_dir)).join(TRANSCRIPT_FILENAME),
            Some(self.shared.log.clone()),
        );
        let stored = transcript.read();
        let fan_out = Arc::new(ViewFanOut::with_recorder(
            self.shared.log.clone(),
            DEFAULT_TRANSCRIPT_LIMIT,
            Some(Arc::new(transcript)),
        ));

        // The thread is attached while the record is still empty, because it
        // is the surface that produced this history and already shows it.
        // Restoring first would post the whole conversation back into it.
        fan_out.clone().attach(view).await;
        fan_out.restore(
            &stored
                .entries
                .iter()
                .map(|held| Held {
                    turn: held.turn,
                    entry: held.entry.clone(),
                })
                .collect::<Vec<_>>(),
            stored.dropped,
        );
        self.state
            .views
            .lock()
            .expect("the views map lock")
            .insert(record.session_id.clone(), Arc::clone(&fan_out));

        // Back on the model it was working with, rather than the configured
        // one.
        let chosen = record.model.as_ref().map(|model| ChosenModel {
            provider: record.provider.clone(),
            model: model.clone(),
        });
        let session = SessionHandle::spawn(SessionOptions {
            id: record.session_id.clone(),
            project: ProjectSelection {
                name: record.project_name.clone(),
                path: record.project_path.clone(),
                prompt: message.content.clone(),
                was_explicit: true,
            },
            chosen,
            state_dir: record.state_dir.clone(),
            views: Arc::new(Redacting::new(
                Arc::clone(&fan_out),
                self.reported_secrets(),
            )),
            launcher: Arc::clone(&self.shared.launcher),
            scheduler: Arc::clone(&self.shared.scheduler),
            config: self.shared.config.clone(),
            log: self.shared.log.clone(),
            timers: None,
            owner_id: record.owner_id.clone(),
            owner_name: None,
            start_turn: Some(fan_out.current_turn()),
            open_pull_request: Some(daemon_open_pull_request()),
            thread_id: Some(record.thread_id.clone()),
            guild_id: self.shared.guild_id.lock().expect("the guild lock").clone(),
            public_url: self.shared.public_url.clone(),
            available_models: self.shared.available_models.clone(),
            delegate_base_url: self.shared.delegate_base_url.clone(),
            unavailable: self.shared.unavailable.clone(),
            operator_ids: self.shared.operator_ids.clone(),
            guest_ids: record.guests.clone(),
            on_guests_changed: Some(on_guests_changed(&self.shared.registry, thread_id)),
            // A thread that has been resumed can still be moved to another
            // model, and that has to be kept the same way a first run's is,
            // or the move lasts only until the next restart.
            on_model_changed: Some(on_model_changed(&self.shared.registry, thread_id)),
            memory: self.shared.memory.clone(),
            fetch_attachment: None,
            describe_images: self.shared.describe_images.clone(),
            resume: true,
            on_ended: on_ended(
                Arc::clone(&self.state),
                Arc::clone(&self.shared.registry),
                Arc::clone(&self.shared.threads),
                self.shared.log.clone(),
                thread_id.to_owned(),
                record.session_id.clone(),
            ),
        });

        self.state
            .by_thread
            .lock()
            .expect("the thread map lock")
            .insert(thread_id.to_owned(), session.clone());
        let mut updated = record.clone();
        updated.updated_at = self.now_ms();
        self.options
            .registry
            .lock()
            .expect("the registry lock")
            .remember(updated);
        session.start(message).await;
        StartOutcome::Started { session }
    }

    fn now_ms(&self) -> i64 {
        match &self.options.now {
            Some(now) => now(),
            None => crate::log::now_ms(),
        }
    }

    /// What must not reach a channel, a transcript, or the interface.
    ///
    /// The configured secrets, plus the operator's house rules: those are
    /// handed to the agent as a file it can read, so a session reading its
    /// own prompt back would otherwise report them.
    fn reported_secrets(&self) -> Vec<String> {
        match house_rules_text(self.options.config.agent.rules_path.as_deref()) {
            None => self.secrets.clone(),
            Some(rules) => {
                let mut all = self.secrets.clone();
                all.push(rules);
                all
            }
        }
    }

    /// Attaches a view to a live session and shows it what it missed.
    pub async fn attach_view(
        &self,
        session_id: &str,
        view: Arc<dyn SessionView>,
    ) -> Option<Attached> {
        let fan_out = self
            .state
            .views
            .lock()
            .expect("the views map lock")
            .get(session_id)
            .cloned()?;
        Some(fan_out.attach(view).await)
    }

    /// The thread a session belongs to, whether it is running or only
    /// remembered.
    ///
    /// A surface uses it to link back to the conversation, where the same
    /// session is also being shown.
    pub fn thread_id_for(&self, session_id: &str) -> Option<String> {
        for (thread_id, session) in self
            .state
            .by_thread
            .lock()
            .expect("the thread map lock")
            .iter()
        {
            if session.id() == session_id {
                return Some(thread_id.clone());
            }
        }
        self.options
            .registry
            .lock()
            .expect("the registry lock")
            .all()
            .into_iter()
            .find(|record| record.session_id == session_id)
            .map(|record| record.thread_id)
    }

    /// Reconciles a withdrawn message, live session or not.
    ///
    /// A live session is told, so the agent stops acting on what was taken
    /// back. One that has ended is not restarted for it: its record is
    /// reconciled where it lies, which is the whole of what is left to do.
    pub async fn withdraw(&self, message_id: &str, thread_id: &str) -> bool {
        if let Some(live) = self.for_thread(thread_id) {
            return live.withdraw(message_id.to_owned()).await;
        }
        let record = self
            .options
            .registry
            .lock()
            .expect("the registry lock")
            .get(thread_id)
            .cloned();
        let Some(record) = record else { return false };
        withdraw_from_record(&record.state_dir, message_id)
            .ok()
            .flatten()
            .is_some()
    }

    /// Delivers a message to the session bound to its thread.
    pub async fn deliver(&self, thread_id: &str, message: IncomingMessage) -> bool {
        let Some(session) = self.for_thread(thread_id) else {
            return false;
        };
        session.handle(message).await;
        true
    }

    /// Delivers a message to a session by its own identifier.
    ///
    /// The interface knows sessions, not threads: a thread is one of the
    /// surfaces showing a session, and the browser never sees it. Writing to
    /// a session that has stopped resumes it, which is what sending to it is
    /// asking for.
    pub async fn deliver_to_session(&self, session_id: &str, message: IncomingMessage) -> bool {
        if let Some(session) = self.for_session(session_id) {
            session.handle(message).await;
            return true;
        }

        let Some(record) = self
            .resumable()
            .into_iter()
            .find(|candidate| candidate.session_id == session_id)
        else {
            return false;
        };
        self.resume(&record.thread_id, message).await.is_started()
    }

    /// Ends the session bound to a thread, if any.
    pub async fn end_thread(&self, thread_id: &str, reason: EndReason) {
        if let Some(session) = self.for_thread(thread_id) {
            session.stop(reason).await;
        }
    }

    /// Ends every live session, for shutdown.
    pub async fn shutdown(&self) {
        for session in self.sessions() {
            session.stop(EndReason::Shutdown).await;
        }
        self.state
            .by_thread
            .lock()
            .expect("the thread map lock")
            .clear();
    }

    /// True when a thread is one this daemon has seen before.
    pub fn can_resume(&self, thread_id: &str) -> bool {
        !self
            .state
            .by_thread
            .lock()
            .expect("the thread map lock")
            .contains_key(thread_id)
            && self
                .options
                .registry
                .lock()
                .expect("the registry lock")
                .get(thread_id)
                .is_some()
    }

    /// Sessions that are not running but could be picked up again.
    ///
    /// A session that idled out is not over: the agent's history outlives its
    /// sandbox, so it is listed rather than forgotten.
    pub fn resumable(&self) -> Vec<ThreadRecord> {
        let live = self.state.by_thread.lock().expect("the thread map lock");
        self.options
            .registry
            .lock()
            .expect("the registry lock")
            .all()
            .into_iter()
            .filter(|record| !live.contains_key(&record.thread_id))
            .collect()
    }
}

/// Which of the two ways a thread comes into being.
enum ThreadKind {
    /// Hung on the message that asked for the session.
    Created,
    /// Opened with a post of its own, for a session nobody typed to.
    Opened,
}

/// A request to start a session with no message to hang it on.
pub struct DetachedRequest {
    pub project: String,
    pub prompt: String,
    pub owner_id: String,
    pub owner_name: Option<String>,
}

/// Keeps the model a session moved to, so a restart comes back on it.
fn on_model_changed(registry: &Arc<Mutex<ThreadRegistry>>, thread_id: &str) -> OnModelChanged {
    let registry = Arc::clone(registry);
    let thread_id = thread_id.to_owned();
    Arc::new(move |provider: &str, model: &str| {
        let mut registry = registry.lock().expect("the registry lock");
        if let Some(current) = registry.get(&thread_id) {
            let mut updated = current.clone();
            updated.provider = Some(provider.to_owned());
            updated.model = Some(model.to_owned());
            registry.remember(updated);
        }
    })
}

/// Keeps the guest list, so a restart does not silently withdraw access.
fn on_guests_changed(registry: &Arc<Mutex<ThreadRegistry>>, thread_id: &str) -> OnGuestsChanged {
    let registry = Arc::clone(registry);
    let thread_id = thread_id.to_owned();
    Arc::new(move |guests: &[String]| {
        let mut registry = registry.lock().expect("the registry lock");
        if let Some(current) = registry.get(&thread_id) {
            let mut updated = current.clone();
            updated.guests = guests.to_vec();
            registry.remember(updated);
        }
    })
}

/// Lets go of a session that has ended.
///
/// Only a deliberate stop drops the thread from the durable index: stopping
/// is how somebody says they are finished with it. Everything else, including
/// a crash, an idle timeout, and a daemon restart, leaves the thread
/// resumable, which is the whole point of keeping the index.
fn on_ended(
    state: Arc<ManagerState>,
    registry: Arc<Mutex<ThreadRegistry>>,
    threads: Arc<dyn ThreadFactory>,
    log: Logger,
    thread_id: String,
    session_id: String,
) -> OnEnded {
    Arc::new(move |reason| {
        state
            .by_thread
            .lock()
            .expect("the thread map lock")
            .remove(&thread_id);
        state
            .ended_threads
            .lock()
            .expect("the ended set lock")
            .insert(thread_id.clone());
        state
            .views
            .lock()
            .expect("the views map lock")
            .remove(&session_id);
        if reason == EndReason::Stopped {
            registry
                .lock()
                .expect("the registry lock")
                .forget(&thread_id);
        }
        threads.release(&thread_id);
        log.info(
            "session removed from the registry",
            &fields([
                ("session", LogValue::from(session_id.as_str())),
                ("reason", LogValue::from(reason_name(reason))),
            ]),
        );
    })
}

/// What a session's ending carries into the manager.
pub type OnEnded = Arc<dyn Fn(EndReason) + Send + Sync>;

/// The name an end reason is written with, for the log.
fn reason_name(reason: EndReason) -> &'static str {
    match reason {
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

/// The pull request opener a session is given: the daemon's own route.
fn daemon_open_pull_request() -> OpenPullRequest {
    Arc::new(move |request: pr::Request| {
        Box::pin(async move {
            let run: pr::Run = Arc::new(pr::run_command);
            let api: pr::Api = Arc::new(pr::call_api);
            let sleep: pr::Sleep = Arc::new(pr::pause);
            pr::open_pull_request(&request, &run, &api, &sleep).await
        })
    })
}

#[cfg(test)]
#[path = "manager/tests.rs"]
mod tests;
