//! Wires configuration, backend, admission, sessions, and the connection
//! together.
//!
//! Kept apart from the entry point so the whole daemon can be built and
//! exercised in a test with no connection and no real sandbox.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::admission::scheduler::{Scheduler, SystemClock};
use crate::chat::inbound::{InboundDecision, RawMessage};
use crate::chat::render::{listed_models, model_pages};
use crate::config::duration::parse_duration;
use crate::config::live::LiveConfig;
use crate::config::redact::redact_config;
use crate::config::schema::defaults::IMAGE;
use crate::config::schema::{
    ALLOW_EVERY_USER, ChatConfig, Config, ConfigError, EgressMode, IncusConfig, SandboxBackend,
};
use crate::log::now_ms;
use crate::log::{LogValue, Logger, fields};
use crate::memory::store::{MemoryStore, Scope};
use crate::provider::discover::Catalog;
use crate::provider::images::{Picture, endpoint_with_effort};
use crate::provider::speech;
use crate::sandbox::Backend;
use crate::sandbox::backend::{CapabilityReport, SandboxUnavailableError};
use crate::sandbox::bailey::BaileyOptions;
use crate::sandbox::bailey::BaileySandbox;
use crate::sandbox::bailey::run_bailey_arc;
use crate::sandbox::broker::BrokerTemplate;
use crate::sandbox::broker::RECEIPTS_FILENAME;
use crate::sandbox::broker::receipts::forget_sessions;
use crate::sandbox::incus::IncusSandbox;
use crate::sandbox::incus::run_incus_arc;
use crate::sandbox::podman::PodmanSandbox;
use crate::sandbox::podman::run_podman_arc;
use crate::session::attachments::RawAttachment;
use crate::session::collect::{Sweep, cold_sessions, collect};
use crate::session::commands::image_args;
use crate::session::commands::{
    answer_without_session, argument_of, first_word, is_addressed_to_bot, is_aside, parse_user_id,
    speech_args,
};
use crate::session::event::Answer;
use crate::session::event::Card;
use crate::session::event::CardColor;
use crate::session::event::EndReason;
use crate::session::manager::SandboxPool;
use crate::session::manager::{
    ManagerOptions, SessionManager, StartOutcome, ThreadFactory, Unavailable,
};
use crate::session::model::{configured_model, split_level};
use crate::session::registry::ThreadRegistry;
use crate::session::session::{DescribeImages, DrawImage, FetchLinked, IncomingMessage, SpeakText};

/// Draining the daemon for maintenance, and restarting it.
pub mod maintenance;

use maintenance::{RestartNote, Restarter, Turn};

/// How often a drain looks at whether it has anything left to wait for.
const DRAIN_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// Raised when the backend cannot enforce what configuration demands.
#[derive(Debug, thiserror::Error)]
#[error(
    "sandbox.requireFullEnforcement is set and the backend cannot enforce everything on this host:\n{}",
    gaps.iter().map(|gap| format!("  - {gap}")).collect::<Vec<_>>().join("\n")
)]
pub struct EnforcementGapError {
    /// What the backend could not enforce on this host.
    pub gaps: Vec<String>,
}

/// Why the daemon could not be started, in the shape main reports.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The backend cannot run here at all.
    #[error(transparent)]
    Unavailable(#[from] SandboxUnavailableError),
    /// The backend runs, but not everything is enforced, and configuration
    /// forbids that.
    #[error(transparent)]
    EnforcementGap(#[from] EnforcementGapError),
    /// The configuration names a provider the kage sessions run would refuse.
    #[error(transparent)]
    Refused(#[from] ConfigError),
}

/// Builds the backend named in configuration. Never falls back to the other.
///
/// `broker`, when given, is what each session's egress broker is built from
/// under `egress.mode = proxy`: a session gets a listener and nonces of its
/// own at launch, under either backend. `built_in` is the host store's definitions of each
/// defined provider's models, which either backend writes under an entry
/// naming one.
pub fn create_sandbox(
    config: &Config,
    log: Logger,
    broker: Option<BrokerTemplate>,
    built_in: BTreeMap<String, Vec<serde_json::Value>>,
) -> Backend {
    match config.sandbox.backend {
        SandboxBackend::Podman => Backend::Podman(Arc::new(PodmanSandbox::new(
            config.sandbox.clone(),
            log,
            run_podman_arc(),
            built_in,
            broker,
        ))),
        SandboxBackend::Bailey => Backend::Bailey(Arc::new(BaileySandbox::new(
            config.sandbox.clone(),
            log,
            config.state_dir.clone(),
            run_bailey_arc(),
            BaileyOptions {
                broker,
                built_in,
                ..Default::default()
            },
        ))),
        SandboxBackend::Incus => Backend::Incus(Arc::new(IncusSandbox::new(
            config.sandbox.clone(),
            log,
            config.project_root.clone(),
            config.state_dir.clone(),
            crate::sandbox::incus::HostFacts::read(),
            run_incus_arc(),
            built_in,
            broker,
        ))),
    }
}

/// What a slash command that posts a file to the channel answers, once it
/// knows whether the file went out.
fn posted_reply(posted: bool, done: &str, file: &str) -> String {
    if posted {
        format!("{done}, and the {file} is in the channel")
    } else {
        format!("{done}, but the {file} was not posted")
    }
}

/// Settings the chosen backend will not read.
///
/// A setting that does nothing should say so rather than sit there looking
/// effective. Only a value that differs from the default counts, since that
/// is the only evidence available that somebody chose it deliberately.
pub fn inert_settings(config: &Config) -> Vec<String> {
    let sandbox = &config.sandbox;
    let mut inert = Vec::new();
    let incus_set = sandbox.incus != IncusConfig::default();
    let image_set = sandbox.image != IMAGE;
    let ports_set = sandbox.egress_ports != vec![443];

    match sandbox.backend {
        SandboxBackend::Bailey => {
            if image_set {
                inert.push("sandbox.image is set but only the podman backend uses it".to_owned());
            }
            if incus_set {
                inert.push("sandbox.incus is set but only the incus backend uses it".to_owned());
            }
        }
        SandboxBackend::Podman => {
            if incus_set {
                inert.push("sandbox.incus is set but only the incus backend uses it".to_owned());
            }
            if sandbox.policy_extra.is_some() {
                inert.push(
                    "sandbox.policyExtra is set but only the bailey backend uses it".to_owned(),
                );
            }
            if sandbox.path_extra.is_some() {
                inert.push(
                    "sandbox.pathExtra is set but only the bailey backend uses it".to_owned(),
                );
            }
            if sandbox.hide_host_address {
                inert.push(
                    "sandbox.hideHostAddress is set but only the bailey backend uses it".to_owned(),
                );
            }
            if sandbox.egress.mode == EgressMode::Open && ports_set {
                inert.push(
                    "sandbox.egressPorts is set but egress.mode is open, so it is not enforced"
                        .to_owned(),
                );
            }
        }
        SandboxBackend::Incus => {
            if image_set {
                inert.push("sandbox.image is set but only the podman backend uses it".to_owned());
            }
            if sandbox.policy_extra.is_some() {
                inert.push(
                    "sandbox.policyExtra is set but only the bailey backend uses it".to_owned(),
                );
            }
            if sandbox.path_extra.is_some() {
                inert.push(
                    "sandbox.pathExtra is set but only the bailey backend uses it".to_owned(),
                );
            }
            if sandbox.hide_host_address {
                inert.push(
                    "sandbox.hideHostAddress has no effect under incus, which hides the host by having no NIC in proxy mode"
                        .to_owned(),
                );
            }
            if sandbox.egress.mode == EgressMode::Open && ports_set {
                inert.push(
                    "sandbox.egressPorts is set but egress.mode is open, so it is not enforced"
                        .to_owned(),
                );
            }
        }
    }
    inert
}

/// Renders what the chosen backend can and cannot enforce here.
///
/// A gap is always stated. Presenting a weaker boundary as if it were a
/// stronger one is worse than the weaker boundary itself, because it removes
/// the chance to decide about it.
pub fn render_startup_report(
    report: &CapabilityReport,
    inert: &[String],
    chat: Option<&ChatConfig>,
) -> Vec<String> {
    let mut lines = Vec::new();

    if let Some(chat) = chat
        && chat
            .allowed_user_ids
            .iter()
            .any(|id| id == ALLOW_EVERY_USER)
    {
        lines.push(
            "ACCESS: the allowlist is open to everyone who can post in the served channel"
                .to_owned(),
        );
        lines.push(
            "  anyone who can post there can run code in a sandbox with write access to the project root"
                .to_owned(),
        );
        lines.push("  set chat.allowedUserIds to specific account ids to close it".to_owned());
    }

    if let Some(chat) = chat
        && !chat.blocked_user_ids.is_empty()
    {
        lines.push(format!(
            "ACCESS: {} blocked, refused before every other rule",
            chat.blocked_user_ids.len()
        ));
    }

    lines.push(format!("sandbox backend: {}", report.backend));
    for note in &report.notes {
        lines.push(format!("  {note}"));
    }

    if report.gaps.is_empty() {
        lines.push("  this backend enforces every configured guarantee on this host".to_owned());
    } else {
        lines.push(format!(
            "  {} guarantee(s) cannot be enforced on this host:",
            report.gaps.len()
        ));
        for gap in &report.gaps {
            lines.push(format!("    - {gap}"));
        }
    }

    for setting in inert {
        lines.push(format!("  {setting}"));
    }
    lines
}

/// A slash command run against the daemon.
pub struct SlashCommand {
    /// The thread the command was used in, or none when used outside one.
    pub thread_id: Option<String>,
    /// Who ran it.
    pub user_id: String,
    /// What to call them in the answer.
    pub user_name: String,
    /// The command as text, exactly as an in-thread message would have been.
    pub content: String,
}

/// What powers the host off, or says why it could not.
pub type PowerOff =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;

/// Says what is left of the provider's usage window.
///
/// The flag asks the providers again rather than answering from what the
/// daemon already holds, for a window that changed before its expiry. The
/// answer is a card, whose color says whether anything is spent.
pub type DescribeUsage =
    Arc<dyn Fn(bool) -> Pin<Box<dyn Future<Output = Card> + Send>> + Send + Sync>;

/// Asks the providers for their models again, and says what came of it.
pub type RefreshModels =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = String> + Send>> + Send + Sync>;

/// Posts an answer back to the channel, outside any thread.
pub type ReplyInChannel =
    Arc<dyn Fn(IncomingMessage, Answer) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Posts a file back to the channel, outside any thread, with its caption.
pub type ReplyWithFile = Arc<
    dyn Fn(IncomingMessage, String, Vec<u8>, String) -> Pin<Box<dyn Future<Output = bool> + Send>>
        + Send
        + Sync,
>;

/// The one command that starts work from the channel itself.
const IMAGE_GENERATION: &str = "!gen-image";

/// The other one: a clip needs no conversation to live in either.
const SPEECH_GENERATION: &str = "!gen-speech";

/// Everything the daemon needs, with the transport injected for testability.
pub struct DaemonOptions {
    /// The configuration every session is started from, and which a change to
    /// the file may replace while the daemon runs.
    pub config: LiveConfig,
    /// The backend sessions are confined by.
    pub sandbox: Arc<dyn SandboxPool>,
    /// What makes a thread for a new session.
    pub threads: Arc<dyn ThreadFactory>,
    /// Where the daemon says what it is doing.
    pub log: Logger,
    /// Posts a refusal back to the channel, outside any thread.
    pub reply_in_channel: ReplyInChannel,
    /// Posts a file back to the channel, outside any thread.
    pub reply_with_file: ReplyWithFile,
    /// Memory, or none when it is switched off.
    pub memory: Option<Arc<MemoryStore>>,
    /// Powers the host off, or none when `!shutdown` is not offered.
    pub power_off: Option<PowerOff>,
    /// Says what is left of the provider window, or none when unmetered.
    pub describe_usage: Option<DescribeUsage>,
    /// Asks the providers for their models again, and says what they listed.
    pub refresh_models: Option<RefreshModels>,
    /// Describes an attached image for a session whose model cannot see one,
    /// or none when every session's model can.
    pub describe_images: Option<DescribeImages>,
    /// Draws a picture with the image model, or none when the host cannot.
    pub draw_image: Option<DrawImage>,
    /// Reads a text aloud with the speech model, or none when the host
    /// cannot.
    pub speak_text: Option<SpeakText>,
    /// Fetches a song the agent could only link to, or none when the host
    /// cannot.
    pub fetch_linked: Option<FetchLinked>,
    /// Where the interface is published, when it is.
    pub public_url: Option<String>,
    /// The provider definitions sessions launch with and the models `!model`
    /// can switch to.
    pub catalog: Catalog,
    /// Where a delegated question is sent, read from the host's model store.
    pub delegate_base_url: Option<String>,
    /// Who may control any session, beyond the configured list.
    pub operator_ids: Option<Vec<String>>,
    /// Why nothing can run yet, or none when it can.
    pub unavailable: Option<Unavailable>,
    /// Restarts the daemon into its binary, or none when `!restart` is not
    /// offered.
    pub restart: Option<Restarter>,
}

/// The running daemon.
pub struct Daemon {
    scheduler: Arc<Scheduler>,
    sessions: Arc<SessionManager>,
    accepting: AtomicBool,
    registry: Arc<Mutex<ThreadRegistry>>,
    options: DaemonOptions,
    /// Counts drains begun and ended, so a drain's watcher stops once another
    /// has replaced it or `!undrain` ended it.
    maintenance: Arc<AtomicU64>,
    /// When the pending restart happens at the latest, or none when a drain
    /// only waits or nothing drains.
    restart_at: Mutex<Option<i64>>,
    /// Held while cold sessions are swept, so two sweeps never share a
    /// cleanup guest.
    sweeping: tokio::sync::Mutex<()>,
    /// When the daemon was built, in epoch milliseconds, for `!status`.
    started_at: i64,
}

/// Where a slash command used in the channel is answered with a file.
fn answered_in_channel(command: &SlashCommand) -> IncomingMessage {
    IncomingMessage {
        id: format!("slash-{}", command.user_id),
        author_id: command.user_id.clone(),
        channel_id: String::new(),
        author_name: Some(command.user_name.clone()),
        content: command.content.clone(),
        attachments: Vec::new(),
        from_interface: false,
    }
}

impl Daemon {
    /// Builds the daemon over the given connections.
    pub fn new(options: DaemonOptions) -> Self {
        // The clock's callback reaches the scheduler it belongs to through a
        // holder that is filled the moment the scheduler exists.
        let holder: Arc<Mutex<Option<Arc<Scheduler>>>> = Arc::new(Mutex::new(None));
        let clock = {
            let holder = Arc::clone(&holder);
            SystemClock::new(move |action| {
                if let Some(scheduler) = holder.lock().expect("the clock holder lock").as_ref() {
                    scheduler.timer_fired(action);
                }
            })
        };
        let starting = options.config.get();
        let scheduler = Scheduler::start(
            starting.limits.clone(),
            Arc::new(clock),
            5_000,
            300_000,
            750,
        );
        *holder.lock().expect("the clock holder lock") = Some(Arc::clone(&scheduler));

        let registry = Arc::new(Mutex::new(ThreadRegistry::new(
            ThreadRegistry::path_for(&starting.state_dir),
            options.log.clone(),
        )));
        let sessions = SessionManager::new(ManagerOptions {
            config: options.config.clone(),
            sandbox: Arc::clone(&options.sandbox),
            scheduler: Arc::clone(&scheduler),
            threads: Arc::clone(&options.threads),
            registry: Arc::clone(&registry),
            log: options.log.clone(),
            make_id: None,
            unavailable: options.unavailable.clone(),
            operator_ids: options.operator_ids.clone(),
            memory: options.memory.clone(),
            describe_images: options.describe_images.clone(),
            draw_image: options.draw_image.clone(),
            speak_text: options.speak_text.clone(),
            fetch_linked: options.fetch_linked.clone(),
            public_url: options.public_url.clone(),
            catalog: options.catalog.clone(),
            delegate_base_url: options.delegate_base_url.clone(),
            now: None,
        });
        Self {
            scheduler,
            sessions: Arc::new(sessions),
            accepting: AtomicBool::new(false),
            registry,
            options,
            maintenance: Arc::new(AtomicU64::new(0)),
            restart_at: Mutex::new(None),
            sweeping: tokio::sync::Mutex::new(()),
            started_at: now_ms(),
        }
    }

    /// The session manager, for the interface and the wiring.
    pub fn sessions(&self) -> Arc<SessionManager> {
        Arc::clone(&self.sessions)
    }

    /// True once startup finished and messages may be acted on.
    pub fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::SeqCst)
    }

    /// Checks the backend and reports what it can enforce here.
    ///
    /// Fails with [`StartError`] when the backend cannot run here, or when
    /// gaps exist and configuration forbids them.
    pub async fn probe(&self) -> Result<CapabilityReport, StartError> {
        let config = self.options.config.get();
        probe_sandbox(self.options.sandbox.as_ref(), &config, &self.options.log).await
    }

    /// Probes the backend, loads the thread index, and sweeps orphans.
    ///
    /// Nothing is accepted until this finishes, so a crashed previous daemon
    /// cannot leave sandboxes running against a project while new ones start.
    pub async fn start(
        &self,
        probed: Option<CapabilityReport>,
    ) -> Result<CapabilityReport, StartError> {
        let starting = self.options.config.get();
        let _ = std::fs::create_dir_all(&starting.state_dir);
        self.options.log.info(
            "effective configuration",
            &fields([(
                "config",
                LogValue::from(redact_config(&starting).to_string()),
            )]),
        );

        let report = match probed {
            Some(report) => report,
            None => self.probe().await?,
        };
        self.registry.lock().expect("the registry lock").load();
        let threads = self.registry.lock().expect("the registry lock").size();
        self.options.log.info(
            "thread index loaded",
            &fields([("threads", LogValue::from(threads))]),
        );
        self.sessions.sweep_orphans().await;
        self.accepting.store(true, Ordering::SeqCst);
        Ok(report)
    }

    /// The cold sessions a sweep now would collect, by thread, with what
    /// each holds, largest first. Nothing is claimed or deleted.
    pub async fn preview_cold(&self) -> Vec<(String, u64)> {
        let parts = SweepParts {
            config: self.options.config.get(),
            registry: Arc::clone(&self.registry),
            sessions: Arc::clone(&self.sessions),
            log: self.options.log.clone(),
        };
        let log = self.options.log.clone();
        tokio::task::spawn_blocking(move || parts.preview())
            .await
            .unwrap_or_else(|error| {
                log.error(
                    "measuring the cold sessions failed",
                    &fields([("detail", LogValue::from(error.to_string()))]),
                );
                Vec::new()
            })
    }

    /// Collects every cold session once, as `retention` says, and says what
    /// it collected and what it kept. Does nothing while `retention.enabled`
    /// is false.
    ///
    /// The deleting runs off the async workers. A session whose files the
    /// daemon may not delete, because its guest wrote them as root or as a
    /// rootless container's user, is emptied from inside a guest and tried
    /// again. One sweep runs at a time.
    pub async fn collect_cold(&self) -> Collection {
        let _sweeping = self.sweeping.lock().await;
        let parts = SweepParts {
            config: self.options.config.get(),
            registry: Arc::clone(&self.registry),
            sessions: Arc::clone(&self.sessions),
            log: self.options.log.clone(),
        };
        let mut swept = parts.sweep_off_workers(None).await;
        if !swept.refused.is_empty() {
            let paths: Vec<std::path::PathBuf> = swept
                .refused
                .iter()
                .flat_map(|refused| refused.paths.iter().cloned())
                .collect();
            match self.options.sandbox.remove_as_guest(&paths).await {
                Ok(()) => {
                    let again: BTreeMap<String, u64> = swept
                        .refused
                        .iter()
                        .map(|refused| (refused.session_id.clone(), refused.bytes))
                        .collect();
                    let retried = parts.sweep_off_workers(Some(again)).await;
                    swept.collected.extend(retried.collected);
                    swept.refused = retried.refused;
                }
                Err(why) => {
                    for refused in &mut swept.refused {
                        refused.why = format!("{}; and from inside a guest: {why}", refused.why);
                    }
                }
            }
        }
        for refused in &swept.refused {
            self.options.log.warn(
                "a cold session could not be collected, and is kept for the next sweep",
                &fields([
                    ("thread", LogValue::from(refused.thread_id.as_str())),
                    ("detail", LogValue::from(refused.why.as_str())),
                ]),
            );
        }
        let collected: Vec<String> = swept
            .collected
            .iter()
            .map(|(session_id, _)| session_id.clone())
            .collect();
        let receipts = std::path::Path::new(&parts.config.state_dir).join(RECEIPTS_FILENAME);
        if let Err(error) = forget_sessions(&receipts, &collected) {
            self.options.log.warn(
                "the receipts of collected sessions could not be deleted",
                &fields([("detail", LogValue::from(error.to_string()))]),
            );
        }
        Collection {
            collected: collected.len(),
            freed: swept.collected.iter().map(|(_, bytes)| bytes).sum(),
            kept: swept
                .refused
                .into_iter()
                .map(|refused| (refused.thread_id, refused.why))
                .collect(),
        }
    }

    /// Whether this account is an operator under the configuration in force.
    ///
    /// Read from the configuration rather than from a list held at startup, so
    /// that adding somebody to `chat.operatorUserIds` takes effect on the next
    /// command instead of the next restart. The read is one copy of the
    /// configuration, which a command already needs for the rest of its answer.
    fn is_operator(&self, author_id: &str) -> bool {
        self.options
            .config
            .get()
            .chat
            .operator_user_ids
            .iter()
            .any(|id| id == author_id)
    }

    /// Answers `!drain`, `!undrain`, and `!restart`, for an operator.
    ///
    /// Answered here rather than in a session, since they act on every
    /// session at once. `message` is where the answer goes, and where a
    /// drain says later that it is done.
    fn maintain(&self, message: &IncomingMessage) -> Option<Answer> {
        let command = first_word(&message.content);
        if !matches!(command, "!drain" | "!undrain" | "!restart") {
            return None;
        }
        if !self.is_operator(&message.author_id) {
            return Some(Answer::Card(maintenance::refused_card(&format!(
                "only an operator may use `{command}`"
            ))));
        }
        let card = match command {
            "!drain" if self.scheduler.is_draining() => {
                let (running, unthreaded) = self.running_turns();
                maintenance::draining_card(
                    &running,
                    unthreaded,
                    self.scheduler.queue_length(),
                    *self.restart_at.lock().expect("the restart lock"),
                )
            }
            "!drain" => self.drain(message, None),
            "!undrain" => self.undrain(),
            _ => match self.restart_plan(&message.content) {
                Ok(plan) => self.drain(message, Some(plan)),
                Err(why) => maintenance::refused_card(&why),
            },
        };
        Some(Answer::Card(card))
    }

    /// What `!restart` would restart into and when, or why it cannot.
    fn restart_plan(&self, content: &str) -> Result<(Restarter, i64), String> {
        let restarter = self
            .options
            .restart
            .clone()
            .ok_or_else(|| "this daemon cannot restart itself".to_owned())?;
        restarter.check()?;
        let wait = maintenance::wait_of(
            argument_of(content, "!restart"),
            self.options.config.get().timeouts.drain_ms,
        )?;
        Ok((restarter, now_ms().saturating_add(wait)))
    }

    /// The turns running now with the thread each runs in, and how many run
    /// outside any thread.
    fn running_turns(&self) -> (Vec<Turn>, usize) {
        running_turns(&self.sessions)
    }

    /// Refuses new work from now on and watches for the running work to end,
    /// then says so, or restarts when `restart` names a restarter and the
    /// moment to restart by at the latest.
    ///
    /// A drain already under way is replaced, which is how `!restart` after
    /// `!drain` turns a drain into a restart.
    fn drain(&self, asked: &IncomingMessage, restart: Option<(Restarter, i64)>) -> Card {
        let generation = self.maintenance.fetch_add(1, Ordering::SeqCst) + 1;
        self.scheduler.set_draining(true);
        let (running, unthreaded) = self.running_turns();
        let restart_at = restart.as_ref().map(|(_, at)| *at);
        *self.restart_at.lock().expect("the restart lock") = restart_at;
        self.options.log.warn(
            "draining for maintenance",
            &fields([
                ("user", LogValue::from(asked.author_id.as_str())),
                ("running", LogValue::from(running.len() + unthreaded)),
                ("waiting", LogValue::from(self.scheduler.queue_length())),
                ("restart", LogValue::from(restart.is_some())),
            ]),
        );
        tokio::spawn(watch_drain(DrainWatch {
            generation,
            current: Arc::clone(&self.maintenance),
            scheduler: Arc::clone(&self.scheduler),
            sessions: Arc::clone(&self.sessions),
            reply: Arc::clone(&self.options.reply_in_channel),
            asked: asked.clone(),
            restart,
            state_dir: self.options.config.get().state_dir,
            log: self.options.log.clone(),
        }));
        maintenance::draining_card(
            &running,
            unthreaded,
            self.scheduler.queue_length(),
            restart_at,
        )
    }

    /// Ends a drain, and with it any restart it was waiting to make.
    fn undrain(&self) -> Card {
        if !self.scheduler.is_draining() {
            return maintenance::refused_card("the bot is not draining");
        }
        self.maintenance.fetch_add(1, Ordering::SeqCst);
        self.scheduler.set_draining(false);
        let called_off = self
            .restart_at
            .lock()
            .expect("the restart lock")
            .take()
            .is_some();
        self.options
            .log
            .info("accepting work again after a drain", &fields([]));
        maintenance::undrained_card(called_off)
    }

    /// Acts on the note a restart left: says the daemon is back where the
    /// restart was asked, tells each thread whose waiting message was dropped,
    /// and continues each turn the restart cut off, in its owner's name.
    ///
    /// Does nothing on a start that did not follow `!restart`.
    pub async fn after_restart(&self) {
        let state_dir = self.options.config.get().state_dir;
        let note = match maintenance::take_note(&state_dir) {
            Ok(Some(note)) => note,
            Ok(None) => return,
            Err(why) => {
                self.options.log.warn(
                    "the note the last restart left cannot be read, so no turn is continued",
                    &fields([("detail", LogValue::from(why))]),
                );
                return;
            }
        };
        self.options.log.info(
            "back from a restart",
            &fields([
                ("continued", LogValue::from(note.interrupted.len())),
                ("dropped", LogValue::from(note.dropped.len())),
            ]),
        );
        let reply = &self.options.reply_in_channel;
        reply(
            posted_in(&note.asked_in),
            Answer::Card(maintenance::back_card(env!("ERRAND_VERSION"))),
        )
        .await;
        for thread_id in &note.dropped {
            reply(
                posted_in(thread_id),
                Answer::Card(maintenance::dropped_card()),
            )
            .await;
        }
        for turn in note.interrupted {
            reply(
                posted_in(&turn.thread_id),
                Answer::Card(maintenance::continued_card()),
            )
            .await;
            let prompt = IncomingMessage {
                id: format!("restart-{}", turn.thread_id),
                author_id: turn.owner_id,
                content: maintenance::CONTINUE_PROMPT.to_owned(),
                ..posted_in(&turn.thread_id)
            };
            self.deliver_to_thread(&turn.thread_id, prompt).await;
        }
    }

    /// Turns the host off, if this person may.
    ///
    /// Answered here rather than in a session: whoever starts a thread owns
    /// it, and owning a thread is no reason to be able to turn the computer
    /// off. The only list that counts is the daemon's own.
    async fn power_off_host(&self, content: &str, author_id: &str) -> Option<String> {
        if first_word(content) != "!shutdown" {
            return None;
        }

        let allowed = self.options.config.get().shutdown.allowed_user_ids;
        if allowed.is_empty() {
            return Some(
                "nobody may power off this host; set shutdown.allowedUserIds to change that"
                    .to_owned(),
            );
        }
        if !allowed.iter().any(|id| id == author_id) {
            return Some(
                "you are not on the list of accounts that may power off this host".to_owned(),
            );
        }
        let Some(power_off) = &self.options.power_off else {
            return Some("this daemon cannot power off the host".to_owned());
        };

        self.options.log.warn(
            "powering off on request",
            &fields([("user", LogValue::from(author_id))]),
        );
        Some(
            power_off()
                .await
                .unwrap_or_else(|| "powering off now".to_owned()),
        )
    }

    /// Says what is left of the provider's usage window.
    ///
    /// `!usage refresh` asks the providers again instead of answering from
    /// what the daemon holds. That is the only way past the gate, which stops
    /// asking about a spent window until it rolls over: a provider that
    /// resets or tops a window up early would otherwise leave the daemon
    /// refusing work the window can now serve until it restarted. The reading
    /// is open to anyone, and the asking is an operator's, since each refresh
    /// is a request to every provider.
    async fn describe_usage(&self, content: &str, author_id: &str) -> Option<Answer> {
        if first_word(content) != "!usage" {
            return None;
        }
        let refresh = match argument_of(content, "!usage") {
            "" => false,
            "refresh" => true,
            _ => {
                return Some(Answer::Text(
                    "say `!usage refresh` to ask the providers about their window again".to_owned(),
                ));
            }
        };
        let Some(describe) = &self.options.describe_usage else {
            return Some(Answer::Text(
                "this provider does not report a usage window".to_owned(),
            ));
        };
        if refresh && !self.is_operator(author_id) {
            return Some(Answer::Text(
                "only an operator may ask the providers again".to_owned(),
            ));
        }
        Some(Answer::Card(describe(refresh).await))
    }

    /// Asks the providers for their models again, for an operator.
    ///
    /// Reaches every session's `!model` at once. A running sandbox keeps what
    /// it was launched with until its next launch.
    async fn refresh_models(&self, content: &str, author_id: &str) -> Option<String> {
        if first_word(content) != "!models" {
            return None;
        }
        if argument_of(content, "!models") != "refresh" {
            return Some(
                "say `!models refresh` to ask the providers for their models again".to_owned(),
            );
        }
        if !self.is_operator(author_id) {
            return Some("only an operator may ask the providers again".to_owned());
        }
        match &self.options.refresh_models {
            None => Some("no provider is set to be asked for its models".to_owned()),
            Some(refresh) => Some(refresh().await),
        }
    }

    /// What the host is doing, for `!status` typed outside any session.
    ///
    /// Inside a thread the session answers `!status` about itself, so this
    /// answers only in the channel.
    fn host_status(&self, content: &str, in_thread: bool) -> Option<Answer> {
        if in_thread || first_word(content) != "!status" {
            return None;
        }
        let limits = self.options.config.get().limits;
        let (running, unthreaded) = self.running_turns();
        let as_count = |limit: u32| usize::try_from(limit).unwrap_or(usize::MAX);
        let status = maintenance::HostStatus {
            version: env!("ERRAND_VERSION"),
            backend: self.options.config.get().sandbox.backend.to_string(),
            started_at: self.started_at,
            live: (
                self.sessions.sessions().len(),
                as_count(limits.max_live_sessions),
            ),
            running: (running, as_count(limits.max_concurrent_turns)),
            unthreaded,
            waiting: (
                self.scheduler.queue_length(),
                as_count(limits.max_queue_length),
            ),
            resumable: self.sessions.resumable().len(),
            paused: self.scheduler.paused_because(),
            draining: (
                self.scheduler.is_draining(),
                *self.restart_at.lock().expect("the restart lock"),
            ),
        };
        Some(Answer::Card(maintenance::status_card(&status)))
    }

    /// Collects cold sessions now, for an operator, and says what it did.
    ///
    /// The same sweep the daemon runs every `retention.every`, so it is safe
    /// while sessions run: a live session, or one used since the sweep began,
    /// is left whole.
    async fn collect_on_request(&self, content: &str, author_id: &str) -> Option<Answer> {
        if first_word(content) != "!gc" {
            return None;
        }
        if !self.is_operator(author_id) {
            return Some(Answer::Card(maintenance::refused_card(
                "only an operator may use `!gc`",
            )));
        }
        if !self.options.config.get().retention.enabled {
            return Some(Answer::Card(maintenance::refused_card(
                "retention.enabled is false, so nothing is collected",
            )));
        }
        match argument_of(content, "!gc") {
            "" => {}
            "status" => {
                let retention = self.options.config.get().retention;
                let cold = self.preview_cold().await;
                return Some(Answer::Card(maintenance::preview_card(
                    &cold,
                    &retention.older_than,
                )));
            }
            other => {
                return Some(Answer::Card(maintenance::refused_card(&format!(
                    "`!gc` takes nothing or `status`, not `{}`",
                    crate::chat::render::inert(other)
                ))));
            }
        }
        self.options.log.info(
            "collecting cold sessions on request",
            &fields([("user", LogValue::from(author_id))]),
        );
        let collection = self.collect_cold().await;
        Some(Answer::Card(maintenance::collection_card(&collection)))
    }

    /// Whatever the daemon answers itself, wherever it was typed.
    async fn answer_as_daemon(
        &self,
        content: &str,
        author_id: &str,
        in_thread: bool,
    ) -> Option<Answer> {
        if let Some(answer) = self.collect_on_request(content, author_id).await {
            return Some(answer);
        }
        if let Some(answer) = self.power_off_host(content, author_id).await {
            return Some(Answer::Text(answer));
        }
        if let Some(answer) = self.describe_usage(content, author_id).await {
            return Some(answer);
        }
        if let Some(answer) = self.refresh_models(content, author_id).await {
            return Some(Answer::Text(answer));
        }
        if let Some(answer) = self.list_models(content, in_thread) {
            return Some(answer);
        }
        self.answer_about_memory(content, author_id, in_thread)
            .map(Answer::Text)
    }

    /// Lists the models this host serves, for a channel with no session in it.
    ///
    /// Which models exist is a property of the host rather than of a session,
    /// so asking in the channel should not start one, and should not pay for
    /// a sandbox and a model call to answer. Inside a thread the session
    /// answers instead, because that is where a name given is acted on: a
    /// session runs on one model at a time, and switching it needs the session
    /// that would change.
    fn list_models(&self, content: &str, in_thread: bool) -> Option<Answer> {
        if first_word(content) != "!model" || in_thread {
            return None;
        }
        if !argument_of(content, "!model").is_empty() {
            return Some(Answer::Text(
                "a session runs on one model at a time, so switch it inside a session thread with `!model <name>`"
                    .to_owned(),
            ));
        }

        let agent = self.options.config.get().agent;
        let available = self.options.catalog.models();
        let chosen = configured_model(&agent);
        let provider = chosen
            .as_ref()
            .and_then(|chosen| chosen.provider.clone())
            .unwrap_or_else(|| agent.provider.clone());
        // Shown qualified, the way the list below names it: the qualified name
        // is the one that always resolves.
        let (shown, current) = match chosen {
            Some(chosen) => {
                let current = split_level(&chosen.model).0;
                (format!("{provider}/{}", chosen.model), current)
            }
            None => ("the provider default".to_owned(), String::new()),
        };

        if available.is_empty() {
            return Some(Answer::Text(
                "this host lists no models to switch to".to_owned(),
            ));
        }
        // The first entry of the list is a spacer, so an intro written above
        // it stands apart from the first header; the card's body needs no
        // spacer under its title.
        let list = listed_models(
            &available,
            &provider,
            &current,
            &self.options.catalog.aliases(),
        );
        Some(Answer::Card(Card::paged(
            CardColor::Ready,
            "models on this host".to_owned(),
            model_pages(&format!("a new session runs on `{shown}`"), &list),
            Some("switch with `!model <name>` in a session thread".to_owned()),
        )))
    }

    /// Answers what is remembered, for somebody who is not in a thread.
    ///
    /// Memory is about a person and a project rather than about a session, so
    /// asking should not need one. Inside a thread the session answers
    /// instead: it knows which project `!facts project` means, and it knows
    /// who was invited, neither of which is visible from here.
    fn answer_about_memory(
        &self,
        content: &str,
        author_id: &str,
        in_thread: bool,
    ) -> Option<String> {
        let word = first_word(content);
        if word != "!facts" && word != "!forget" {
            return None;
        }
        if in_thread {
            return None;
        }

        let memory = self.options.memory.as_ref()?;
        let rest = content[word.len()..].trim();
        if rest.to_lowercase() == "project" {
            return Some("a project is a thread's own, so ask in one".to_owned());
        }

        if word == "!forget" {
            // "Owner" means nothing in a channel, so this is the operator's,
            // the way anything else acting beyond one session is.
            if !self.is_operator(author_id) {
                return Some(
                    "only an operator may forget what is remembered from here; ask in a thread you own"
                        .to_owned(),
                );
            }
            if rest.is_empty() {
                return Some("say who, as `!forget @somebody`".to_owned());
            }
            let Some(target) = parse_user_id(rest) else {
                return Some("say who, as `!forget @somebody`".to_owned());
            };
            let gone = memory.forget(Scope::User, &target).unwrap_or(0);
            return Some(if gone == 0 {
                format!("nothing was remembered about <@{target}>")
            } else {
                format!(
                    "forgot {gone} fact{} about <@{target}>",
                    if gone == 1 { "" } else { "s" }
                )
            });
        }

        let subject = if rest.is_empty() {
            author_id.to_owned()
        } else {
            parse_user_id(rest)?
        };
        let facts = memory
            .facts_for(Scope::User, &subject, i64::MAX)
            .unwrap_or_default();
        if facts.is_empty() {
            Some(format!("nothing is remembered about <@{subject}>"))
        } else {
            Some(format!(
                "remembered about <@{subject}>:\n{}",
                facts
                    .iter()
                    .map(|fact| format!("- {}", fact.fact))
                    .collect::<Vec<_>>()
                    .join("\n")
            ))
        }
    }

    /// Acts on a message the gateway has already filtered.
    pub async fn handle(&self, raw: RawMessage, decision: InboundDecision) {
        if !self.is_accepting() {
            self.options.log.warn(
                "a message arrived before startup finished and was not acted on",
                &fields([]),
            );
            return;
        }

        let message = IncomingMessage {
            id: raw.id,
            author_id: raw.author_id,
            channel_id: raw.channel_id,
            author_name: raw.author_name,
            content: raw.content,
            attachments: raw
                .attachments
                .into_iter()
                .map(|file| RawAttachment {
                    id: file.id,
                    name: file.name,
                    url: file.url,
                    size: file.size,
                    content_type: file.content_type,
                })
                .collect(),
            from_interface: false,
        };

        // Before anything is routed. These are not session commands, and being
        // in a thread is not a reason to be allowed to run one.
        if let Some(answered) = self.maintain(&message).or_else(|| {
            self.host_status(
                &message.content,
                matches!(decision, InboundDecision::Thread { .. }),
            )
        }) {
            (self.options.reply_in_channel)(message, answered).await;
            return;
        }
        if let Some(answered) = self
            .answer_as_daemon(
                &message.content,
                &message.author_id,
                matches!(decision, InboundDecision::Thread { .. }),
            )
            .await
        {
            (self.options.reply_in_channel)(message, answered).await;
            return;
        }

        if let InboundDecision::Thread { thread_id } = decision {
            self.deliver_to_thread(&thread_id, message).await;
            return;
        }

        self.start_from_channel(message).await;
    }

    async fn deliver_to_thread(&self, thread_id: &str, message: IncomingMessage) {
        if self.sessions.deliver(thread_id, message.clone()).await {
            return;
        }

        // Reading the project needs nothing running, so it is answered from
        // disk rather than by waking the session, even while the bot drains.
        if self
            .sessions
            .read_without_session(thread_id, &message)
            .await
        {
            return;
        }

        // A thread with no live session may still be resumable: the agent's
        // history outlives the sandbox, so a restart does not end a
        // conversation.
        if self.sessions.can_resume(thread_id) {
            let aside = is_aside(&message.content);
            let outcome = self.sessions.resume(thread_id, message.clone()).await;
            // An aside is people talking to each other, so one that could not
            // wake the session is left without an answer rather than
            // interrupting them.
            if let StartOutcome::Refused { reason } = outcome
                && !aside
            {
                (self.options.reply_in_channel)(message, reason.into()).await;
            }
            return;
        }

        if self.sessions.is_finished_thread(thread_id) {
            (self.options.reply_in_channel)(
                message,
                Answer::Text(
                    "this session has ended; post in the channel to start a new one".to_owned(),
                ),
            )
            .await;
        }
    }

    async fn start_from_channel(&self, message: IncomingMessage) {
        // An aside is people talking, not work to start. The channel is where
        // they talk, so one here is left alone entirely: no session, no
        // thread, and no reply, which would itself be noise in the
        // conversation it was deliberately kept out of.
        if is_aside(&message.content) {
            return;
        }

        // Answered here rather than by starting a session: a command that only
        // describes the system needs nothing running, and opening a thread and
        // a sandbox to print a list is not an answer.
        if let Some(listed) = answer_without_session(&message.content) {
            (self.options.reply_in_channel)(message, Answer::Card(listed)).await;
            return;
        }

        // A picture needs no conversation to live in, so the channel draws
        // it here rather than opening a thread and a sandbox for one turn.
        if first_word(&message.content) == IMAGE_GENERATION {
            self.draw_into_channel(&message).await;
            return;
        }

        // Neither does a clip.
        if first_word(&message.content) == SPEECH_GENERATION {
            self.speak_into_channel(&message).await;
            return;
        }

        // Another bot's command, or one of this system's that needs a session.
        // Either way it is not work to start, and the channel is shared, so it
        // is left alone rather than answered.
        if is_addressed_to_bot(&message.content) {
            return;
        }

        let outcome = self.sessions.start(message.clone()).await;
        if let StartOutcome::Refused { reason } = outcome {
            (self.options.reply_in_channel)(message, reason.into()).await;
        }
    }

    /// Draws a picture asked for in the channel, and posts it there.
    ///
    /// One request to the provider's images endpoint, answered as bytes and
    /// posted as an upload. No session, no thread, and no sandbox: a picture
    /// is one call, and opening a conversation to make one is what used to
    /// send the prompt to a chat model instead.
    async fn draw_into_channel(&self, message: &IncomingMessage) {
        let asked = match image_args(argument_of(&message.content, IMAGE_GENERATION)) {
            Ok(asked) => asked,
            Err(refused) => {
                (self.options.reply_in_channel)(message.clone(), refused.into()).await;
                return;
            }
        };
        if asked.prompt.is_empty() {
            (self.options.reply_in_channel)(
                message.clone(),
                "say what to generate, as `!gen-image [--effort <level>] <prompt>`"
                    .to_owned()
                    .into(),
            )
            .await;
            return;
        }
        match self
            .draw(asked.model, asked.effort.as_deref(), asked.prompt)
            .await
        {
            Ok(drawn) => {
                (self.options.reply_with_file)(
                    message.clone(),
                    drawn.name,
                    drawn.bytes,
                    drawn.caption,
                )
                .await;
            }
            Err(reason) => {
                (self.options.reply_in_channel)(message.clone(), reason.into()).await;
            }
        }
    }

    /// Reads a text aloud asked for in the channel, and posts the clip
    /// there.
    ///
    /// One request to the provider's speech endpoint, answered as bytes and
    /// posted as an upload. No session, no thread, and no sandbox, as for a
    /// picture.
    async fn speak_into_channel(&self, message: &IncomingMessage) {
        let asked = match speech_args(argument_of(&message.content, SPEECH_GENERATION)) {
            Ok(asked) => asked,
            Err(refused) => {
                (self.options.reply_in_channel)(message.clone(), refused.into()).await;
                return;
            }
        };
        match self
            .read_aloud(asked.voice, asked.language, asked.text)
            .await
        {
            Ok(clip) => {
                (self.options.reply_with_file)(
                    message.clone(),
                    clip.name,
                    clip.bytes,
                    clip.caption,
                )
                .await;
            }
            Err(reason) => {
                (self.options.reply_in_channel)(message.clone(), reason.into()).await;
            }
        }
    }

    /// Draws one picture, with the model and effort it was asked for.
    ///
    /// The endpoint is resolved from the configuration in force rather than a
    /// copy taken at startup, so a model or a provider changed in the file is
    /// one this already draws with. A model named on the command beats the
    /// configured one, and an effort asked for beats the level it carries.
    async fn draw(
        &self,
        named: Option<&str>,
        effort: Option<&str>,
        prompt: &str,
    ) -> Result<Picture, String> {
        let config = self.options.config.get();
        let configured = config
            .agent
            .image_gen
            .as_deref()
            .map(str::trim)
            .filter(|named| !named.is_empty());
        let Some(named) = named.or(configured) else {
            return Err("no image model is configured; name one with `agent.imageGen`".to_owned());
        };
        let Some(draw_image) = self.options.draw_image.clone() else {
            return Err("this host has no way to draw an image".to_owned());
        };
        let endpoint = endpoint_with_effort(
            &config.agent,
            &self.options.catalog.models(),
            named,
            &config.agent.aliases,
            effort,
        )
        .map_err(|refused| refused.0)?;
        let drawn = draw_image(endpoint.clone(), prompt.to_owned()).await?;
        drawn
            .into_picture(prompt, &endpoint.model, config.output.max_upload_bytes)
            .map_err(|refused| refused.0)
    }

    /// Reads one text aloud, with the voice and language it was asked for.
    ///
    /// The endpoint is resolved from the configuration in force, as a
    /// picture's is. The model is `agent.speechGen`, which the command
    /// cannot override, since the voice and language are the only choices a
    /// reading offers.
    async fn read_aloud(
        &self,
        voice: Option<String>,
        language: Option<String>,
        text: &str,
    ) -> Result<speech::Clip, String> {
        let config = self.options.config.get();
        let Some(named) = config
            .agent
            .speech_gen
            .as_deref()
            .map(str::trim)
            .filter(|named| !named.is_empty())
        else {
            return Err(
                "no speech model is configured; set `agent.speechGen` to a speech model that \
                 answers `/audio/speech`"
                    .to_owned(),
            );
        };
        let Some(speak_text) = self.options.speak_text.clone() else {
            return Err("this host has no way to read aloud".to_owned());
        };
        let endpoint = speech::endpoint(
            &config.agent,
            &self.options.catalog.models(),
            named,
            &config.agent.aliases,
        )
        .map_err(|refused| refused.0)?;
        let spoken = speak_text(endpoint.clone(), text.to_owned(), voice, language).await?;
        spoken
            .into_clip(config.output.max_upload_bytes)
            .map_err(|refused| refused.0)
    }

    /// Runs a slash command, which is the same command an in-thread message
    /// runs. Returns a short line to acknowledge the interaction with.
    pub async fn run_command(&self, command: &SlashCommand) -> String {
        if !self.is_accepting() {
            return "the daemon is still starting up".to_owned();
        }

        // Answered in words here like the rest, but a drain also says later
        // where it was asked that it is done, so it is told where that was.
        let asked = IncomingMessage {
            channel_id: command.thread_id.clone().unwrap_or_default(),
            ..answered_in_channel(command)
        };
        if let Some(answered) = self
            .maintain(&asked)
            .or_else(|| self.host_status(&command.content, command.thread_id.is_some()))
        {
            return answered.words();
        }

        // A slash command is typed at the channel, never inside a thread. An
        // ephemeral acknowledgement has no place to draw a card in, so only
        // the words of the answer go back.
        if let Some(answered) = self
            .answer_as_daemon(&command.content, &command.user_id, false)
            .await
        {
            return answered.words();
        }

        // Some commands describe the system rather than act on a session, so
        // they answer anywhere. The answer is the acknowledgement: it goes
        // back to the person who ran it and nowhere else.
        if let Some(listed) = answer_without_session(&command.content) {
            return listed.words();
        }

        // A picture needs no session, so outside a thread it is drawn here
        // and posted to the channel. The acknowledgement is the answer: the
        // interaction was already claimed, and the picture follows in the
        // channel it was asked in.
        if command.thread_id.is_none() && first_word(&command.content) == IMAGE_GENERATION {
            let asked = match image_args(argument_of(&command.content, IMAGE_GENERATION)) {
                Ok(asked) => asked,
                Err(refused) => return refused,
            };
            if asked.prompt.is_empty() {
                return "say what to generate, as `/gen-image <prompt>`".to_owned();
            }
            let prompt = asked.prompt.to_owned();
            let where_to = answered_in_channel(command);
            let drawing = self
                .draw(asked.model, asked.effort.as_deref(), &prompt)
                .await;
            return match drawing {
                Ok(drawn) => {
                    let reply = &self.options.reply_with_file;
                    let posted = reply(where_to, drawn.name, drawn.bytes, drawn.caption).await;
                    posted_reply(posted, "drew it", "picture")
                }
                Err(reason) => reason,
            };
        }

        // Neither does a clip.
        if command.thread_id.is_none() && first_word(&command.content) == SPEECH_GENERATION {
            let asked = match speech_args(argument_of(&command.content, SPEECH_GENERATION)) {
                Ok(asked) => asked,
                Err(refused) => return refused,
            };
            let where_to = answered_in_channel(command);
            let reading = self
                .read_aloud(asked.voice, asked.language, asked.text)
                .await;
            return match reading {
                Ok(clip) => {
                    let reply = &self.options.reply_with_file;
                    let posted = reply(where_to, clip.name, clip.bytes, clip.caption).await;
                    posted_reply(posted, "read it", "clip")
                }
                Err(reason) => reason,
            };
        }

        let Some(thread_id) = &command.thread_id else {
            return "use this inside a session thread; post in the channel to start one".to_owned();
        };

        let delivered = self
            .sessions
            .deliver(
                thread_id,
                IncomingMessage {
                    id: format!("slash-{thread_id}"),
                    author_id: command.user_id.clone(),
                    channel_id: thread_id.clone(),
                    author_name: Some(command.user_name.clone()),
                    content: command.content.clone(),
                    attachments: Vec::new(),
                    from_interface: false,
                },
            )
            .await;
        if delivered {
            return format!("ran {}", first_word(&command.content));
        }

        if self.sessions.can_resume(thread_id) {
            return "this thread is asleep; post a message in it to wake the session first"
                .to_owned();
        }
        "this thread has no session".to_owned()
    }

    /// Records which guild the served channel is in.
    pub fn set_guild(&self, guild_id: String) {
        self.sessions.set_guild(guild_id);
    }

    /// Ends the session bound to a thread that was closed from outside.
    pub async fn thread_closed(&self, thread_id: &str) {
        self.sessions
            .end_thread(thread_id, EndReason::ThreadArchived)
            .await;
    }

    /// Reconciles a message that was deleted in the chat.
    ///
    /// A deletion in the channel names no thread, but a thread started from a
    /// message carries that message's own id, so the id is tried as one.
    /// Nothing is started to do this: a session that has ended still has its
    /// record reconciled, and that happens without bringing it back.
    pub async fn withdraw(&self, message_id: &str, thread_id: Option<&str>) {
        self.sessions
            .withdraw(message_id, thread_id.unwrap_or(message_id))
            .await;
    }

    /// Stops accepting, ends every session, and stops every timer.
    pub async fn shutdown(&self) {
        self.accepting.store(false, Ordering::SeqCst);
        self.sessions.shutdown().await;
        self.scheduler.shutdown();
    }
}

/// What a sweep for cold sessions did.
#[derive(Debug, Default)]
pub struct Collection {
    /// How many sessions were collected.
    pub collected: usize,
    /// How many bytes they held.
    pub freed: u64,
    /// The sessions kept, by thread, with why each could not be collected.
    pub kept: Vec<(String, String)>,
}

/// A cold session whose files could not all be deleted.
struct Refused {
    thread_id: String,
    session_id: String,
    /// What it held before anything of it was deleted.
    bytes: u64,
    paths: Vec<std::path::PathBuf>,
    why: String,
}

/// What one pass over the cold sessions did.
#[derive(Default)]
struct Swept {
    /// Each session collected, with the bytes it held.
    collected: Vec<(String, u64)>,
    refused: Vec<Refused>,
}

/// What a pass over the cold sessions works with, owned so it can run off the
/// async workers.
#[derive(Clone)]
struct SweepParts {
    config: Config,
    registry: Arc<Mutex<ThreadRegistry>>,
    sessions: Arc<SessionManager>,
    log: Logger,
}

impl SweepParts {
    /// Runs [`Self::sweep`] on a thread that may block, since a session can
    /// hold gigabytes to delete.
    async fn sweep_off_workers(&self, only: Option<BTreeMap<String, u64>>) -> Swept {
        let parts = self.clone();
        tokio::task::spawn_blocking(move || parts.sweep(only.as_ref()))
            .await
            .unwrap_or_else(|error| {
                self.log.error(
                    "a sweep for cold sessions failed, and the next is tried on schedule",
                    &fields([("detail", LogValue::from(error.to_string()))]),
                );
                Swept::default()
            })
    }

    /// What [`Self::sweep`] would collect now, by thread, largest first.
    ///
    /// The same sessions it would reach, skipping the same live ones and the
    /// ones whose project a live session works in, but without claiming a
    /// thread, so what it says can change before a sweep runs.
    fn preview(&self) -> Vec<(String, u64)> {
        let config = &self.config;
        let Some(older_than_ms) = parse_duration(&config.retention.older_than) else {
            return Vec::new();
        };
        let records = self.registry.lock().expect("the registry lock").all();
        let sessions = Arc::clone(&self.sessions);
        let is_live = move |session_id: &str| sessions.for_session(session_id).is_some();
        let sweep = Sweep {
            state_root: &config.state_dir,
            project_root: &config.project_root,
            now_ms: now_ms(),
            older_than_ms,
            is_live: &is_live,
        };
        let live = self.sessions.sessions();
        let mut cold: Vec<(String, u64)> = cold_sessions(&sweep, &records, false)
            .into_iter()
            .filter(|cold| {
                let own_project = std::path::Path::new(&config.project_root).join(&cold.session_id);
                !(cold.unnamed
                    && live.iter().any(|session| {
                        std::path::Path::new(&session.project().path) == own_project
                    }))
            })
            .filter_map(|cold| Some((cold.thread_id?, cold.bytes)))
            .collect();
        cold.sort_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
        cold
    }

    /// Collects each cold session, or only those `only` names.
    ///
    /// `only` carries what each held when a first pass measured it: a guest
    /// that emptied it since would have it measure as nothing.
    ///
    /// Each thread is claimed first, as a resume claims it, so a thread that
    /// comes back during the sweep is either reopened before the sweep
    /// reaches it or waits until its session is gone. Under the claim the
    /// entry is read again, and a session that is live, was used since the
    /// sweep began, or whose project another live session works in is left
    /// whole. The entry is forgotten only once everything else is deleted, so
    /// a sweep that fails or is cut short leaves it naming what remains, for
    /// the next one. Each collection is logged with what it freed.
    fn sweep(&self, only: Option<&BTreeMap<String, u64>>) -> Swept {
        let config = &self.config;
        let mut swept = Swept::default();
        if !config.retention.enabled {
            return swept;
        }
        let Some(older_than_ms) = parse_duration(&config.retention.older_than) else {
            return swept;
        };
        let records = self.registry.lock().expect("the registry lock").all();
        let sessions = Arc::clone(&self.sessions);
        let is_live = move |session_id: &str| sessions.for_session(session_id).is_some();
        let sweep = Sweep {
            state_root: &config.state_dir,
            project_root: &config.project_root,
            now_ms: now_ms(),
            older_than_ms,
            is_live: &is_live,
        };
        for cold in cold_sessions(&sweep, &records, false) {
            if only.is_some_and(|only| !only.contains_key(&cold.session_id)) {
                continue;
            }
            let bytes = only
                .and_then(|only| only.get(&cold.session_id).copied())
                .unwrap_or(cold.bytes);
            let Some(thread_id) = cold.thread_id.as_deref() else {
                continue;
            };
            let Some(_claim) = self.sessions.claim_thread(thread_id) else {
                continue;
            };
            let then = records.iter().find(|record| record.thread_id == thread_id);
            let now = self
                .registry
                .lock()
                .expect("the registry lock")
                .get(thread_id)
                .cloned();
            let own_project = std::path::Path::new(&config.project_root).join(&cold.session_id);
            let project_busy = cold.unnamed
                && self
                    .sessions
                    .sessions()
                    .iter()
                    .any(|live| std::path::Path::new(&live.project().path) == own_project);
            if now.as_ref() != then || is_live(&cold.session_id) || project_busy {
                continue;
            }
            match collect(&sweep, &cold) {
                Ok(()) => {
                    self.registry
                        .lock()
                        .expect("the registry lock")
                        .forget(thread_id);
                    self.log.info(
                        "collected a cold session",
                        &fields([
                            ("thread", LogValue::from(thread_id)),
                            ("session", LogValue::from(cold.session_id.as_str())),
                            ("bytes", LogValue::from(bytes)),
                        ]),
                    );
                    swept.collected.push((cold.session_id.clone(), bytes));
                }
                Err(why) => swept.refused.push(Refused {
                    thread_id: thread_id.to_owned(),
                    session_id: cold.session_id.clone(),
                    bytes,
                    paths: cold.paths.clone(),
                    why,
                }),
            }
        }
        swept
    }
}

/// A message to post into `channel_id`, a thread or the channel, that answers
/// nothing in particular.
fn posted_in(channel_id: &str) -> IncomingMessage {
    IncomingMessage {
        id: String::new(),
        author_id: String::new(),
        channel_id: channel_id.to_owned(),
        author_name: None,
        content: String::new(),
        attachments: Vec::new(),
        from_interface: false,
    }
}

/// The turns running now with the thread each runs in, and how many run
/// outside any thread, such as one started from the interface.
fn running_turns(sessions: &SessionManager) -> (Vec<Turn>, usize) {
    let mut threaded = Vec::new();
    let mut unthreaded = 0;
    for session in sessions.sessions() {
        if !sessions.is_busy(session.id()) {
            continue;
        }
        match sessions.thread_id_for(session.id()) {
            Some(thread_id) => threaded.push(Turn {
                thread_id,
                owner_id: session.owner_id().to_owned(),
            }),
            None => unthreaded += 1,
        }
    }
    (threaded, unthreaded)
}

/// What a drain's watcher needs, taken from the daemon when it begins.
struct DrainWatch {
    generation: u64,
    current: Arc<AtomicU64>,
    scheduler: Arc<Scheduler>,
    sessions: Arc<SessionManager>,
    reply: ReplyInChannel,
    asked: IncomingMessage,
    restart: Option<(Restarter, i64)>,
    state_dir: String,
    log: Logger,
}

/// Waits for a drain to have nothing left to wait for, then says so where it
/// was asked, or restarts once that happens or its moment comes.
///
/// Stops without a word once another drain replaced this one or `!undrain`
/// ended it.
async fn watch_drain(watch: DrainWatch) {
    loop {
        tokio::time::sleep(DRAIN_POLL).await;
        if watch.current.load(Ordering::SeqCst) != watch.generation {
            return;
        }
        let idle = watch.scheduler.is_idle();
        let Some((restarter, restart_at)) = &watch.restart else {
            if idle {
                watch
                    .log
                    .info("drained: no turn is running or waiting", &fields([]));
                (watch.reply)(watch.asked, Answer::Card(maintenance::drained_card())).await;
                return;
            }
            continue;
        };
        if !idle && now_ms() < *restart_at {
            continue;
        }
        let note = RestartNote {
            asked_in: watch.asked.channel_id.clone(),
            interrupted: running_turns(&watch.sessions).0,
            dropped: watch
                .scheduler
                .queued_sessions()
                .iter()
                .filter_map(|session_id| watch.sessions.thread_id_for(session_id))
                .collect(),
        };
        if let Err(error) = maintenance::leave_note(&watch.state_dir, &note) {
            // Without the note nothing cut off would be continued, so the
            // restart waits for somebody rather than losing work quietly.
            watch.log.error(
                "the restart is called off: its note cannot be written",
                &fields([("detail", LogValue::from(error.to_string()))]),
            );
            let why = format!(
                "the restart is called off, since its note cannot be written: {error}. \
                 The bot is still draining, and `!undrain` accepts work again."
            );
            (watch.reply)(watch.asked, Answer::Card(maintenance::refused_card(&why))).await;
            return;
        }
        watch.log.warn(
            "restarting for maintenance",
            &fields([
                ("continued", LogValue::from(note.interrupted.len())),
                ("dropped", LogValue::from(note.dropped.len())),
            ]),
        );
        (watch.reply)(
            watch.asked,
            Answer::Card(maintenance::restarting_card(&note)),
        )
        .await;
        restarter.restart();
        return;
    }
}

/// Checks the backend and reports what it can enforce on this host.
///
/// Standalone so it can run before the connection is made: a missing image or
/// an unenforceable guarantee should fail immediately, not after a login
/// round trip.
pub async fn probe_sandbox(
    sandbox: &dyn SandboxPool,
    config: &Config,
    log: &Logger,
) -> Result<CapabilityReport, StartError> {
    let report = sandbox.probe().await?;
    for line in render_startup_report(&report, &inert_settings(config), Some(&config.chat)) {
        log.info(&line, &fields([]));
    }

    let problems = report.kage_providers.refuses(&config.agent.providers);
    if !problems.is_empty() {
        return Err(StartError::Refused(ConfigError { problems }));
    }

    if !report.gaps.is_empty() && config.sandbox.require_full_enforcement {
        return Err(StartError::EnforcementGap(EnforcementGapError {
            gaps: report.gaps,
        }));
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
