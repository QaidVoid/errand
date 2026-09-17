//! Wires configuration, backend, admission, sessions, and the connection
//! together.
//!
//! Kept apart from the entry point so the whole daemon can be built and
//! exercised in a test with no connection and no real sandbox.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::admission::scheduler::{Scheduler, SystemClock};
use crate::chat::inbound::{InboundDecision, RawMessage};
use crate::config::redact::redact_config;
use crate::config::schema::{ALLOW_EVERY_USER, ChatConfig, Config, SandboxBackend};
use crate::log::{LogValue, Logger, fields};
use crate::memory::store::{MemoryStore, Scope};
use crate::sandbox::backend::{CapabilityReport, SandboxUnavailableError};
use crate::session::commands::{
    answer_without_session, first_word, is_addressed_to_bot, is_aside, parse_user_id,
};
use crate::session::manager::{
    ManagerOptions, SessionManager, StartOutcome, ThreadFactory, Unavailable,
};
use crate::session::registry::ThreadRegistry;
use crate::session::session::{DescribeImages, IncomingMessage};

/// Raised when the backend cannot enforce what configuration demands.
#[derive(Debug, thiserror::Error)]
#[error(
    "sandbox.requireFullEnforcement is set and the backend cannot enforce everything on this host:\n{}",
    gaps.iter().map(|gap| format!("  - {gap}")).collect::<Vec<_>>().join("\n")
)]
pub struct EnforcementGapError {
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
}

/// Builds the backend named in configuration. Never falls back to the other.
///
/// `egress_proxy_port`, when given, is the host loopback port of the broker
/// every session's egress is forced through under `egress.mode = proxy`. Only
/// the bailey backend uses it; podman bounds the network by its own
/// namespace.
pub fn create_sandbox(
    config: &Config,
    log: Logger,
    egress_proxy_port: Option<u16>,
    brokering: Option<crate::sandbox::bailey::ProviderBrokering>,
) -> crate::sandbox::Backend {
    match config.sandbox.backend {
        SandboxBackend::Podman => {
            crate::sandbox::Backend::Podman(Arc::new(crate::sandbox::podman::PodmanSandbox::new(
                config.sandbox.clone(),
                log,
                crate::sandbox::podman::run_podman_arc(),
            )))
        }
        SandboxBackend::Bailey => {
            crate::sandbox::Backend::Bailey(Arc::new(crate::sandbox::bailey::BaileySandbox::new(
                config.sandbox.clone(),
                log,
                config.state_dir.clone(),
                crate::sandbox::bailey::run_bailey_arc(),
                crate::sandbox::bailey::BaileyOptions {
                    egress_proxy_port,
                    brokering,
                    ..Default::default()
                },
            )))
        }
    }
}

/// Settings the chosen backend will not read.
///
/// A setting that does nothing should say so rather than sit there looking
/// effective. Only a value that differs from the default counts, since that
/// is the only evidence available that somebody chose it deliberately.
pub fn inert_settings(config: &Config) -> Vec<String> {
    if config.sandbox.backend != crate::config::schema::SandboxBackend::Bailey {
        return Vec::new();
    }
    if config.sandbox.image == crate::config::schema::defaults::IMAGE {
        return Vec::new();
    }
    vec!["sandbox.image is set but only the podman backend uses it".to_owned()]
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
    pub thread_id: Option<String>,
    pub user_id: String,
    pub user_name: String,
    pub content: String,
}

/// What powers the host off, or says why it could not.
pub type PowerOff =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;

/// Says what is left of the provider's usage window.
pub type DescribeUsage =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = String> + Send>> + Send + Sync>;

/// Posts a refusal back to the channel, outside any thread.
pub type ReplyInChannel =
    Arc<dyn Fn(IncomingMessage, String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Everything the daemon needs, with the transport injected for testability.
pub struct DaemonOptions {
    pub config: Config,
    pub sandbox: Arc<dyn crate::session::manager::SandboxPool>,
    pub threads: Arc<dyn ThreadFactory>,
    pub log: Logger,
    pub reply_in_channel: ReplyInChannel,
    /// Memory, or none when it is switched off.
    pub memory: Option<Arc<MemoryStore>>,
    pub power_off: Option<PowerOff>,
    pub describe_usage: Option<DescribeUsage>,
    /// Describes an attached image for a session whose model cannot see one,
    /// or none when every session's model can.
    pub describe_images: Option<DescribeImages>,
    /// Where the interface is published, when it is.
    pub public_url: Option<String>,
    /// Models this host knows the provider serves, for `!model`.
    pub available_models: Vec<String>,
    /// Where a delegated question is sent, read from the host's model store.
    pub delegate_base_url: Option<String>,
    /// Who may control any session, beyond the configured list.
    pub operator_ids: Option<Vec<String>>,
    /// Why nothing can run yet, or none when it can.
    pub unavailable: Option<Unavailable>,
}

/// The running daemon.
pub struct Daemon {
    scheduler: Arc<Scheduler>,
    sessions: Arc<SessionManager>,
    accepting: AtomicBool,
    registry: Arc<Mutex<ThreadRegistry>>,
    options: DaemonOptions,
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
        let scheduler = Scheduler::start(
            options.config.limits.clone(),
            Arc::new(clock),
            5_000,
            300_000,
            750,
        );
        *holder.lock().expect("the clock holder lock") = Some(Arc::clone(&scheduler));

        let registry = Arc::new(Mutex::new(ThreadRegistry::new(
            ThreadRegistry::path_for(&options.config.state_dir),
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
            public_url: options.public_url.clone(),
            available_models: options.available_models.clone(),
            delegate_base_url: options.delegate_base_url.clone(),
            now: None,
        });
        Self {
            scheduler,
            sessions: Arc::new(sessions),
            accepting: AtomicBool::new(false),
            registry,
            options,
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
        probe_sandbox(
            self.options.sandbox.as_ref(),
            &self.options.config,
            &self.options.log,
        )
        .await
    }

    /// Probes the backend, loads the thread index, and sweeps orphans.
    ///
    /// Nothing is accepted until this finishes, so a crashed previous daemon
    /// cannot leave sandboxes running against a project while new ones start.
    pub async fn start(
        &self,
        probed: Option<CapabilityReport>,
    ) -> Result<CapabilityReport, StartError> {
        let _ = std::fs::create_dir_all(&self.options.config.state_dir);
        self.options.log.info(
            "effective configuration",
            &fields([(
                "config",
                LogValue::from(redact_config(&self.options.config).to_string()),
            )]),
        );

        let report = match probed {
            Some(report) => report,
            None => self.probe().await?,
        };
        self.registry.lock().expect("the registry lock").load();
        let threads = self.registry.lock().expect("the registry lock").size();
        // A count this large does not happen; the cast matches LogValue::from.
        #[allow(clippy::cast_possible_wrap)]
        let threads = threads as i64;
        self.options.log.info(
            "thread index loaded",
            &fields([("threads", LogValue::from(threads))]),
        );
        self.sessions.sweep_orphans().await;
        self.accepting.store(true, Ordering::SeqCst);
        Ok(report)
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

        let allowed = &self.options.config.shutdown.allowed_user_ids;
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
    async fn describe_usage(&self, content: &str) -> Option<String> {
        if first_word(content) != "!usage" {
            return None;
        }
        match &self.options.describe_usage {
            None => Some("this provider does not report a usage window".to_owned()),
            Some(describe) => Some(describe().await),
        }
    }

    /// Whatever the daemon answers itself, wherever it was typed.
    async fn answer_as_daemon(
        &self,
        content: &str,
        author_id: &str,
        in_thread: bool,
    ) -> Option<String> {
        if let Some(answer) = self.power_off_host(content, author_id).await {
            return Some(answer);
        }
        if let Some(answer) = self.describe_usage(content).await {
            return Some(answer);
        }
        self.answer_about_memory(content, author_id, in_thread)
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
            if !self
                .options
                .config
                .chat
                .operator_user_ids
                .iter()
                .any(|id| id == author_id)
            {
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
            author_name: raw.author_name,
            content: raw.content,
            attachments: raw
                .attachments
                .into_iter()
                .map(|file| crate::session::attachments::RawAttachment {
                    id: file.id,
                    name: file.name,
                    url: file.url,
                    size: file.size,
                    content_type: file.content_type,
                })
                .collect(),
        };

        // Before anything is routed. These are not session commands, and being
        // in a thread is not a reason to be allowed to run one.
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

        // A thread with no live session may still be resumable: the agent's
        // history outlives the sandbox, so a restart does not end a
        // conversation.
        if self.sessions.can_resume(thread_id) {
            let outcome = self.sessions.resume(thread_id, message.clone()).await;
            if let StartOutcome::Refused { reason } = outcome {
                (self.options.reply_in_channel)(message, reason).await;
            }
            return;
        }

        if self.sessions.is_finished_thread(thread_id) {
            (self.options.reply_in_channel)(
                message,
                "this session has ended; post in the channel to start a new one".to_owned(),
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
            (self.options.reply_in_channel)(message, listed).await;
            return;
        }

        // Another bot's command, or one of this system's that needs a session.
        // Either way it is not work to start, and the channel is shared, so it
        // is left alone rather than answered.
        if is_addressed_to_bot(&message.content) {
            return;
        }

        let outcome = self.sessions.start(message.clone()).await;
        if let crate::session::manager::StartOutcome::Refused { reason } = outcome {
            (self.options.reply_in_channel)(message, reason).await;
        }
    }

    /// Runs a slash command, which is the same command an in-thread message
    /// runs. Returns a short line to acknowledge the interaction with.
    pub async fn run_command(&self, command: &SlashCommand) -> String {
        if !self.is_accepting() {
            return "the daemon is still starting up".to_owned();
        }

        // A slash command is typed at the channel, never inside a thread.
        if let Some(answered) = self
            .answer_as_daemon(&command.content, &command.user_id, false)
            .await
        {
            return answered;
        }

        // Some commands describe the system rather than act on a session, so
        // they answer anywhere. The answer is the acknowledgement: it goes
        // back to the person who ran it and nowhere else.
        if let Some(listed) = answer_without_session(&command.content) {
            return listed;
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
                    author_name: Some(command.user_name.clone()),
                    content: command.content.clone(),
                    attachments: Vec::new(),
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
            .end_thread(thread_id, crate::session::event::EndReason::ThreadArchived)
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

/// Checks the backend and reports what it can enforce on this host.
///
/// Standalone so it can run before the connection is made: a missing image or
/// an unenforceable guarantee should fail immediately, not after a login
/// round trip.
pub async fn probe_sandbox(
    sandbox: &dyn crate::session::manager::SandboxPool,
    config: &Config,
    log: &Logger,
) -> Result<CapabilityReport, StartError> {
    let report = sandbox.probe().await?;
    for line in render_startup_report(&report, &inert_settings(config), Some(&config.chat)) {
        log.info(&line, &fields([]));
    }

    if !report.gaps.is_empty() && config.sandbox.require_full_enforcement {
        return Err(StartError::EnforcementGap(EnforcementGapError {
            gaps: report.gaps,
        }));
    }
    Ok(report)
}

#[cfg(test)]
#[path = "daemon/tests.rs"]
mod tests;
