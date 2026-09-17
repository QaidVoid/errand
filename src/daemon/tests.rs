//! Daemon wiring tests, ported from `daemon_test.ts`.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::watch;

use super::{
    Daemon, DaemonOptions, SlashCommand, StartError, create_sandbox, inert_settings,
    render_startup_report,
};
use crate::agent::client::AgentProcess;
use crate::chat::inbound::{InboundDecision, RawMessage};
use crate::config::schema::SandboxBackend;
use crate::config::validate::validate_config;
use crate::log::{LogFields, LogLevel, Logger};
use crate::memory::store::{MemoryStore, Scope};
use crate::sandbox::backend::{
    CapabilityReport, SandboxLaunch, SandboxLaunchError, SandboxUnavailableError,
};
use crate::sandbox::paths;
use crate::session::manager::{CreatedThread, FoundView, MadeThread, SandboxPool, ThreadFactory};
use crate::session::session::IncomingMessage;
use crate::session::views::SessionView;

const OWNER: &str = "100000000000000001";

fn silent() -> Logger {
    Logger::new(LogFields::new(), Arc::new(|_level, _line| {}))
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(20)).await;
}

/// An agent that answers its readiness call and otherwise says nothing.
struct QuietAgent {
    queue: Mutex<std::collections::VecDeque<u8>>,
    closed: Mutex<bool>,
    exit: watch::Receiver<Option<i32>>,
}

#[derive(Clone)]
struct QuietControls {
    fake: Arc<QuietAgent>,
    exit: watch::Receiver<Option<i32>>,
    sender: Arc<Mutex<Option<watch::Sender<Option<i32>>>>>,
}

impl QuietAgent {
    fn new() -> (Arc<Self>, QuietControls) {
        let (exit_sender, exit_receiver) = watch::channel(None);
        let fake = Arc::new(Self {
            queue: Mutex::new(std::collections::VecDeque::new()),
            closed: Mutex::new(false),
            exit: exit_receiver.clone(),
        });
        (
            Arc::clone(&fake),
            QuietControls {
                fake,
                exit: exit_receiver,
                sender: Arc::new(Mutex::new(Some(exit_sender))),
            },
        )
    }
}

impl QuietControls {
    fn assemble(self) -> QuietControls {
        self
    }

    fn end(&self) {
        {
            let mut closed = self.fake.closed.lock().unwrap();
            if *closed {
                return;
            }
            *closed = true;
        }
        self.fake.queue.lock().unwrap().clear();
        if let Some(sender) = self.sender.lock().unwrap().take() {
            let _ = sender.send(Some(0));
        }
    }
}

impl AgentProcess for QuietAgent {
    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let parsed: serde_json::Value = serde_json::from_slice(bytes).expect("a written command");
        if let Some(id) = parsed.get("id") {
            self.queue.lock().unwrap().extend(
                json!({ "type": "response", "id": id })
                    .to_string()
                    .into_bytes(),
            );
            self.queue.lock().unwrap().push_back(b'\n');
        }
        Ok(())
    }

    fn read_stdout<'a>(
        &'a self,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = std::io::Result<usize>> + Send + 'a>> {
        Box::pin(async move {
            loop {
                {
                    let mut queue = self.queue.lock().unwrap();
                    let count = queue.len().min(buf.len());
                    if count > 0 {
                        for (index, byte) in queue.drain(..count).enumerate() {
                            buf[index] = byte;
                        }
                        return Ok(count);
                    }
                    if *self.closed.lock().unwrap() {
                        return Ok(0);
                    }
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
    }

    fn read_stderr<'a>(
        &'a self,
        _buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = std::io::Result<usize>> + Send + 'a>> {
        Box::pin(async { Ok(0) })
    }

    fn exited(&self) -> Pin<Box<dyn Future<Output = Option<i32>> + Send>> {
        let mut receiver = self.exit.clone();
        Box::pin(async move {
            let _ = receiver.changed().await;
            *receiver.borrow()
        })
    }
}

struct FakeSandbox {
    report: Mutex<Option<CapabilityReport>>,
    probe_fails: Mutex<bool>,
    launched: Mutex<Vec<SandboxLaunch>>,
    agents: Mutex<Vec<QuietControls>>,
}

impl FakeSandbox {
    fn new(report: Option<CapabilityReport>) -> Arc<Self> {
        Arc::new(Self {
            report: Mutex::new(report),
            probe_fails: Mutex::new(false),
            launched: Mutex::new(Vec::new()),
            agents: Mutex::new(Vec::new()),
        })
    }
}

impl SandboxPool for FakeSandbox {
    fn probe(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<CapabilityReport, SandboxUnavailableError>> + Send + '_>>
    {
        if *self.probe_fails.lock().unwrap() {
            return Box::pin(async move {
                Err(SandboxUnavailableError {
                    backend: SandboxBackend::Bailey,
                    reasons: vec!["broken".to_owned()],
                })
            });
        }
        let report = self
            .report
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(CapabilityReport {
                backend: SandboxBackend::Bailey,
                gaps: Vec::new(),
                notes: Vec::new(),
            });
        Box::pin(async move { Ok(report) })
    }

    fn launch(
        self: Arc<Self>,
        launch: SandboxLaunch,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<crate::session::session::RunningBox, SandboxLaunchError>>
                + Send,
        >,
    > {
        Box::pin(async move {
            let (agent, controls) = QuietAgent::new();
            self.agents.lock().unwrap().push(controls.clone());
            self.launched.lock().unwrap().push(launch.clone());
            let project_path = launch.project_path.clone();
            Ok(crate::session::session::RunningBox {
                process: agent,
                to_host_path: Arc::new(move |path: &str| {
                    paths::host_path_under("/workspace", &project_path, path)
                }),
                stop: Arc::new(move || {
                    controls.end();
                    Box::pin(async { false }) as Pin<Box<dyn Future<Output = bool> + Send>>
                }),
            })
        })
    }

    fn list_orphans(&self) -> Pin<Box<dyn Future<Output = Vec<String>> + Send + '_>> {
        Box::pin(async { Vec::new() })
    }

    fn remove_orphans<'a>(
        &'a self,
        _names: &'a [String],
    ) -> Pin<Box<dyn Future<Output = usize> + Send + 'a>> {
        Box::pin(async { 0 })
    }
}

/// A thread factory that answers silently, recording what it made.
struct FakeThreads {
    created: Mutex<Vec<String>>,
    closed: Arc<Mutex<Vec<crate::session::event::EndReason>>>,
    next: Mutex<u32>,
}

impl ThreadFactory for FakeThreads {
    fn create(self: Arc<Self>, _message: IncomingMessage, name: String) -> MadeThread {
        Box::pin(async move {
            self.created.lock().unwrap().push(name);
            let id = format!("thread-{}", *self.next.lock().unwrap());
            *self.next.lock().unwrap() += 1;
            Ok(CreatedThread {
                id,
                view: self.quiet_view(),
            })
        })
    }

    fn open(self: Arc<Self>, name: String, _opener: String) -> MadeThread {
        Box::pin(async move {
            self.created.lock().unwrap().push(name);
            let id = format!("thread-{}", *self.next.lock().unwrap());
            *self.next.lock().unwrap() += 1;
            Ok(CreatedThread {
                id,
                view: self.quiet_view(),
            })
        })
    }

    fn port_for(self: Arc<Self>, _thread_id: String) -> FoundView {
        Box::pin(async move {
            Some(Arc::new(QuietView {
                closed: Arc::clone(&self.closed),
            }) as Arc<dyn SessionView>)
        })
    }

    fn release(&self, _thread_id: &str) {}
}

impl FakeThreads {
    fn quiet_view(&self) -> Arc<QuietView> {
        Arc::new(QuietView {
            closed: Arc::clone(&self.closed),
        })
    }
}

/// A view that records only that its session ended.
struct QuietView {
    closed: Arc<Mutex<Vec<crate::session::event::EndReason>>>,
}

impl SessionView for QuietView {
    fn observe<'a>(
        &'a self,
        event: &'a crate::session::event::SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), crate::session::views::ViewError>> + Send + 'a>>
    {
        Box::pin(async move {
            if let crate::session::event::SessionEvent::Close { reason } = event {
                self.closed.lock().unwrap().push(*reason);
            }
            Ok(())
        })
    }
}

struct NoClock;

impl crate::admission::scheduler::Clock for NoClock {
    fn now(&self) -> i64 {
        0
    }

    fn set_timeout(&self, _action: crate::admission::scheduler::Timer, _ms: i64) -> u64 {
        0
    }

    fn clear_timeout(&self, _handle: u64) {}
}

fn config_with(overrides: &serde_json::Value) -> crate::config::schema::Config {
    let mut base = json!({
        "chat": {
            "token": "a.token.value",
            "channelId": "chan",
            "allowedUserIds": [OWNER],
        },
        "agent": {
            "provider": "anthropic",
            "credentialName": "ANTHROPIC_API_KEY",
            "credential": "secret",
        },
        "projectRoot": "/tmp/errand-projects",
        "stateDir": "/tmp/errand-state",
    });
    if let Some(overrides) = overrides.as_object() {
        for (key, value) in overrides {
            base[key.as_str()] = value.clone();
        }
    }
    validate_config(&base).expect("the test configuration is accepted")
}

fn raw(content: &str) -> RawMessage {
    RawMessage {
        id: "m1".to_owned(),
        author_id: OWNER.to_owned(),
        author_name: Some("amelia".to_owned()),
        author_is_bot: false,
        channel_id: "chan".to_owned(),
        parent_channel_id: None,
        content: content.to_owned(),
        attachments: Vec::new(),
    }
}

fn raw_with(content: &str, id: &str, author_id: &str) -> RawMessage {
    let mut sent = raw(content);
    sent.id = id.to_owned();
    sent.author_id = author_id.to_owned();
    sent
}

struct Harness {
    daemon: Daemon,
    threads: Arc<FakeThreads>,
    sandbox: Arc<FakeSandbox>,
    replies: Arc<Mutex<Vec<String>>>,
    lines: Arc<Mutex<Vec<(LogLevel, String)>>>,
    _root: tempfile::TempDir,
}

struct DaemonCase {
    settings: Option<serde_json::Value>,
    power_off: Option<super::PowerOff>,
    describe_usage: Option<super::DescribeUsage>,
    report: Option<CapabilityReport>,
    start: bool,
    memory: Option<Arc<MemoryStore>>,
}

impl Default for DaemonCase {
    fn default() -> Self {
        Self {
            settings: None,
            power_off: None,
            describe_usage: None,
            report: None,
            start: true,
            memory: None,
        }
    }
}

async fn with_daemon(
    case: DaemonCase,
    run: impl FnOnce(&Harness) -> Pin<Box<dyn Future<Output = ()> + '_>>,
) {
    let root = tempfile::tempdir().expect("a temp directory");
    let mut settings = json!({
        "projectRoot": root.path().join("projects").display().to_string(),
        "stateDir": root.path().join("state").display().to_string(),
    });
    if let Some(overrides) = case.settings.as_ref().and_then(|value| value.as_object()) {
        for (key, value) in overrides {
            settings[key.as_str()] = value.clone();
        }
    }
    let config = config_with(&settings);
    let threads = Arc::new(FakeThreads {
        created: Mutex::new(Vec::new()),
        closed: Arc::new(Mutex::new(Vec::new())),
        next: Mutex::new(1),
    });
    let sandbox = FakeSandbox::new(case.report.clone());
    let replies = Arc::new(Mutex::new(Vec::new()));
    let lines = Arc::new(Mutex::new(Vec::new()));

    let daemon = Daemon::new(DaemonOptions {
        config,
        sandbox: Arc::clone(&sandbox) as Arc<dyn SandboxPool>,
        threads: Arc::clone(&threads) as Arc<dyn ThreadFactory>,
        log: {
            let lines = Arc::clone(&lines);
            Logger::new(
                LogFields::new(),
                Arc::new(move |level, line| {
                    lines.lock().unwrap().push((level, line.to_owned()));
                }),
            )
        },
        reply_in_channel: {
            let replies = Arc::clone(&replies);
            Arc::new(move |_message: IncomingMessage, text: String| {
                let replies = Arc::clone(&replies);
                Box::pin(async move { replies.lock().unwrap().push(text) })
            })
        },
        memory: case.memory.clone(),
        power_off: case.power_off.clone(),
        describe_usage: case.describe_usage.clone(),
        public_url: None,
        available_models: Vec::new(),
        delegate_base_url: None,
        operator_ids: None,
        unavailable: None,
    });

    let harness = Harness {
        daemon,
        threads,
        sandbox,
        replies,
        lines,
        _root: root,
    };

    if case.start {
        harness.daemon.start(None).await.expect("the daemon starts");
    }

    run(&harness).await;

    harness.daemon.shutdown().await;
}

fn created(threads: &FakeThreads) -> Vec<String> {
    threads.created.lock().unwrap().clone()
}

#[tokio::test]
async fn starting_reports_the_configuration_with_no_secret_in_it() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            let logged = harness
                .lines
                .lock()
                .unwrap()
                .iter()
                .map(|(_, line)| line.clone())
                .collect::<Vec<_>>()
                .join("\n");

            assert!(harness.daemon.is_accepting());
            assert!(logged.contains("effective configuration"));
            assert!(!logged.contains("a.token.value"));
            assert!(logged.contains("[redacted]"));
        })
    })
    .await;
}

/// A crashed daemon must not leave agents running against a project.
#[tokio::test]
async fn nothing_is_acted_on_before_startup_has_finished() {
    with_daemon(
        DaemonCase {
            start: false,
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .daemon
                    .handle(raw("demo: go"), InboundDecision::Start)
                    .await;

                assert!(created(&harness.threads).is_empty());
                assert_eq!(
                    harness
                        .daemon
                        .run_command(&SlashCommand {
                            thread_id: Some("thread-1".to_owned()),
                            user_id: OWNER.to_owned(),
                            user_name: "amelia".to_owned(),
                            content: "!status".to_owned(),
                        })
                        .await,
                    "the daemon is still starting up"
                );
            })
        },
    )
    .await;
}

#[tokio::test]
async fn a_message_in_the_channel_starts_a_session_in_a_thread() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            harness
                .daemon
                .handle(raw("demo: fix the parser"), InboundDecision::Start)
                .await;

            assert_eq!(created(&harness.threads), ["demo: fix the parser"]);
            assert_eq!(harness.daemon.sessions().sessions().len(), 1);
        })
    })
    .await;
}

/// The channel is where people talk; an aside there is not work to start.
#[tokio::test]
async fn an_aside_in_the_channel_starts_nothing_and_says_nothing() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            harness
                .daemon
                .handle(raw("!!! anyone around?"), InboundDecision::Start)
                .await;

            assert!(created(&harness.threads).is_empty());
            assert!(harness.replies.lock().unwrap().is_empty());
        })
    })
    .await;
}

/// Opening a thread and a sandbox to print a list is not an answer.
#[tokio::test]
async fn help_in_the_channel_is_answered_without_starting_anything() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            harness
                .daemon
                .handle(raw("!help"), InboundDecision::Start)
                .await;

            assert!(created(&harness.threads).is_empty());
            assert!(harness.replies.lock().unwrap()[0].contains("!steer"));
        })
    })
    .await;
}

#[tokio::test]
async fn a_command_needing_a_session_is_left_alone_in_the_channel() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            harness
                .daemon
                .handle(raw("!ls src"), InboundDecision::Start)
                .await;
            harness
                .daemon
                .handle(
                    raw_with("!somebodyelses thing", "m2", OWNER),
                    InboundDecision::Start,
                )
                .await;

            assert!(created(&harness.threads).is_empty());
            assert!(harness.replies.lock().unwrap().is_empty());
        })
    })
    .await;
}

#[tokio::test]
async fn a_refusal_to_start_is_said_in_the_channel_where_it_was_asked() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            harness
                .daemon
                .handle(raw("demo: first"), InboundDecision::Start)
                .await;
            harness
                .daemon
                .handle(
                    raw_with("demo: second", "m2", OWNER),
                    InboundDecision::Start,
                )
                .await;

            assert!(harness.replies.lock().unwrap()[0].contains("already has a live session"));
        })
    })
    .await;
}

#[tokio::test]
async fn a_message_in_a_thread_reaches_its_session() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            harness
                .daemon
                .handle(raw("demo: go"), InboundDecision::Start)
                .await;

            let mut sent = raw("carry on");
            sent.id = "m2".to_owned();
            sent.channel_id = "thread-1".to_owned();
            sent.parent_channel_id = Some("chan".to_owned());
            harness
                .daemon
                .handle(
                    sent,
                    InboundDecision::Thread {
                        thread_id: "thread-1".to_owned(),
                    },
                )
                .await;

            assert!(harness.replies.lock().unwrap().is_empty());
        })
    })
    .await;
}

/// The agent's history outlives the sandbox, so a restart does not end it.
#[tokio::test]
async fn a_message_in_a_sleeping_thread_wakes_the_session() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            harness
                .daemon
                .handle(raw("demo: go"), InboundDecision::Start)
                .await;
            harness
                .daemon
                .sessions()
                .end_thread("thread-1", crate::session::event::EndReason::Idle)
                .await;
            assert_eq!(harness.daemon.sessions().sessions().len(), 0);

            harness
                .daemon
                .handle(
                    raw_with("carry on", "m2", OWNER),
                    InboundDecision::Thread {
                        thread_id: "thread-1".to_owned(),
                    },
                )
                .await;

            assert_eq!(harness.daemon.sessions().sessions().len(), 1);
        })
    })
    .await;
}

#[tokio::test]
async fn a_message_in_a_thread_that_is_over_says_where_to_start_a_new_one() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            harness
                .daemon
                .handle(raw("demo: go"), InboundDecision::Start)
                .await;
            harness
                .daemon
                .sessions()
                .end_thread("thread-1", crate::session::event::EndReason::Stopped)
                .await;

            harness
                .daemon
                .handle(
                    raw_with("hello?", "m2", OWNER),
                    InboundDecision::Thread {
                        thread_id: "thread-1".to_owned(),
                    },
                )
                .await;

            assert!(
                harness.replies.lock().unwrap()[0]
                    .contains("post in the channel to start a new one")
            );
        })
    })
    .await;
}

#[tokio::test]
async fn a_thread_archived_from_outside_ends_its_session() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            harness
                .daemon
                .handle(raw("demo: go"), InboundDecision::Start)
                .await;

            harness.daemon.thread_closed("thread-1").await;

            assert_eq!(harness.daemon.sessions().sessions().len(), 0);
        })
    })
    .await;
}

/// Whoever starts a thread owns it, and owning a thread is no reason to be
/// able to turn the computer off. The only list that counts is the daemon's
/// own.
#[tokio::test]
async fn nobody_powers_off_the_host_unless_the_daemons_own_list_says_so() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            harness
                .daemon
                .handle(raw("!shutdown"), InboundDecision::Start)
                .await;

            assert!(harness.replies.lock().unwrap()[0].contains("nobody may power off this host"));
            assert!(created(&harness.threads).is_empty());
        })
    })
    .await;
}

#[tokio::test]
async fn an_account_not_on_the_shutdown_list_is_refused() {
    with_daemon(
        DaemonCase {
            settings: Some(json!({ "shutdown": { "allowedUserIds": [OWNER] } })),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .daemon
                    .handle(raw_with("!shutdown", "m1", "999"), InboundDecision::Start)
                    .await;

                assert!(harness.replies.lock().unwrap()[0].contains("not on the list"));
            })
        },
    )
    .await;
}

#[tokio::test]
async fn an_account_on_the_list_powers_the_host_off() {
    with_daemon(
        DaemonCase {
            settings: Some(json!({ "shutdown": { "allowedUserIds": [OWNER] } })),
            power_off: Some(Arc::new(|| {
                Box::pin(async { None::<String> })
                    as Pin<Box<dyn Future<Output = Option<String>> + Send>>
            })),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .daemon
                    .handle(raw("!shutdown"), InboundDecision::Start)
                    .await;

                assert!(harness.replies.lock().unwrap()[0].contains("powering off now"));
                let logged = harness
                    .lines
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(_, line)| line.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(logged.contains("powering off on request"));
            })
        },
    )
    .await;
}

#[tokio::test]
async fn a_power_off_that_fails_says_what_went_wrong() {
    with_daemon(
        DaemonCase {
            settings: Some(json!({ "shutdown": { "allowedUserIds": [OWNER] } })),
            power_off: Some(Arc::new(|| {
                Box::pin(async { Some("systemctl refused".to_owned()) })
                    as Pin<Box<dyn Future<Output = Option<String>> + Send>>
            })),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .daemon
                    .handle(raw("!shutdown"), InboundDecision::Start)
                    .await;

                assert!(harness.replies.lock().unwrap()[0].contains("systemctl refused"));
            })
        },
    )
    .await;
}

/// Shutting down from inside a thread must not be a way around the list.
#[tokio::test]
async fn the_shutdown_list_governs_the_slash_command_too() {
    with_daemon(
        DaemonCase {
            settings: Some(json!({ "shutdown": { "allowedUserIds": [OWNER] } })),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                let answer = harness
                    .daemon
                    .run_command(&SlashCommand {
                        thread_id: Some("thread-1".to_owned()),
                        user_id: "999".to_owned(),
                        user_name: "somebody".to_owned(),
                        content: "!shutdown".to_owned(),
                    })
                    .await;

                assert!(answer.contains("not on the list"));
            })
        },
    )
    .await;
}

#[tokio::test]
async fn a_slash_command_runs_the_same_command_a_message_would() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            harness
                .daemon
                .handle(raw("demo: go"), InboundDecision::Start)
                .await;

            let answer = harness
                .daemon
                .run_command(&SlashCommand {
                    thread_id: Some("thread-1".to_owned()),
                    user_id: OWNER.to_owned(),
                    user_name: "amelia".to_owned(),
                    content: "!status".to_owned(),
                })
                .await;

            assert_eq!(answer, "ran !status");
        })
    })
    .await;
}

/// A help listing wants to go back to whoever asked, not into the channel.
#[tokio::test]
async fn a_slash_command_that_needs_no_session_answers_the_caller_directly() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            let answer = harness
                .daemon
                .run_command(&SlashCommand {
                    thread_id: None,
                    user_id: OWNER.to_owned(),
                    user_name: "amelia".to_owned(),
                    content: "!help".to_owned(),
                })
                .await;

            assert!(answer.contains("!steer"));
            assert!(harness.replies.lock().unwrap().is_empty());
        })
    })
    .await;
}

#[tokio::test]
async fn a_slash_command_outside_a_thread_says_where_to_use_it() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            let answer = harness
                .daemon
                .run_command(&SlashCommand {
                    thread_id: None,
                    user_id: OWNER.to_owned(),
                    user_name: "amelia".to_owned(),
                    content: "!ls".to_owned(),
                })
                .await;

            assert!(answer.contains("post in the channel to start one"));
        })
    })
    .await;
}

#[tokio::test]
async fn a_slash_command_in_a_sleeping_thread_says_to_wake_it_first() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            harness
                .daemon
                .handle(raw("demo: go"), InboundDecision::Start)
                .await;
            harness
                .daemon
                .sessions()
                .end_thread("thread-1", crate::session::event::EndReason::Idle)
                .await;

            let answer = harness
                .daemon
                .run_command(&SlashCommand {
                    thread_id: Some("thread-1".to_owned()),
                    user_id: OWNER.to_owned(),
                    user_name: "amelia".to_owned(),
                    content: "!ls".to_owned(),
                })
                .await;

            assert!(answer.contains("post a message in it to wake the session"));
        })
    })
    .await;
}

#[tokio::test]
async fn shutting_down_ends_every_session_and_stops_accepting() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            harness
                .daemon
                .handle(raw("demo: go"), InboundDecision::Start)
                .await;

            harness.daemon.shutdown().await;

            assert!(!harness.daemon.is_accepting());
            assert!(harness.daemon.sessions().sessions().is_empty());
            assert_eq!(
                *harness.threads.closed.lock().unwrap(),
                [crate::session::event::EndReason::Shutdown]
            );
        })
    })
    .await;
}

/// Presenting a weaker boundary as a stronger one is worse than the weaker
/// boundary, because it removes the chance to decide about it.
#[test]
fn the_startup_report_always_states_what_cannot_be_enforced() {
    let lines = render_startup_report(
        &CapabilityReport {
            backend: SandboxBackend::Bailey,
            gaps: ["seccomp is unavailable".to_owned()].into_iter().collect(),
            notes: ["landlock v5".to_owned()].into_iter().collect(),
        },
        &[],
        None,
    )
    .join("\n");

    assert!(lines.contains("sandbox backend: bailey"));
    assert!(lines.contains("landlock v5"));
    assert!(lines.contains("1 guarantee(s) cannot be enforced"));
    assert!(lines.contains("seccomp is unavailable"));
}

#[test]
fn a_backend_with_nothing_missing_says_so_plainly() {
    let lines = render_startup_report(
        &CapabilityReport {
            backend: SandboxBackend::Podman,
            gaps: Vec::new(),
            notes: Vec::new(),
        },
        &[],
        None,
    )
    .join("\n");

    assert!(lines.contains("enforces every configured guarantee"));
}

/// Anyone who can post can run code, which is worth saying out loud.
#[test]
fn an_open_allowlist_is_reported_as_the_decision_it_is() {
    let config = config_with(&json!({
        "chat": {
            "token": "t",
            "channelId": "c",
            "allowedUserIds": ["*"],
            "blockedUserIds": ["9"],
        },
    }));
    let lines = render_startup_report(
        &CapabilityReport {
            backend: SandboxBackend::Bailey,
            gaps: Vec::new(),
            notes: Vec::new(),
        },
        &[],
        Some(&config.chat),
    )
    .join("\n");

    assert!(lines.contains("open to everyone who can post"));
    assert!(lines.contains("1 blocked"));
}

#[test]
fn a_setting_the_chosen_backend_ignores_is_reported_as_inert() {
    assert!(inert_settings(&config_with(&json!({}))).is_empty());
    assert_eq!(
        inert_settings(&config_with(&json!({
            "sandbox": { "image": "localhost/mine:v2" },
        }))),
        ["sandbox.image is set but only the podman backend uses it".to_owned()]
    );
    assert!(
        inert_settings(&config_with(&json!({
            "sandbox": { "backend": "podman", "image": "localhost/mine:v2" },
        })))
        .is_empty()
    );
}

/// A guarantee that cannot be met must not be started around silently.
#[tokio::test]
async fn a_gap_the_configuration_forbids_stops_the_daemon_starting() {
    with_daemon(
        DaemonCase {
            start: false,
            report: Some(CapabilityReport {
                backend: SandboxBackend::Bailey,
                gaps: ["no landlock here".to_owned()].into_iter().collect(),
                notes: Vec::new(),
            }),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                assert!(matches!(
                    harness.daemon.start(None).await,
                    Err(StartError::EnforcementGap(_))
                ));
            })
        },
    )
    .await;
}

#[tokio::test]
async fn the_same_gap_is_allowed_when_the_configuration_allows_it() {
    with_daemon(
        DaemonCase {
            start: false,
            settings: Some(json!({ "sandbox": { "requireFullEnforcement": false } })),
            report: Some(CapabilityReport {
                backend: SandboxBackend::Bailey,
                gaps: ["no landlock here".to_owned()].into_iter().collect(),
                notes: Vec::new(),
            }),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .daemon
                    .start(None)
                    .await
                    .expect("the gap is allowed");

                assert!(harness.daemon.is_accepting());
                let logged = harness
                    .lines
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(_, line)| line.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(logged.contains("no landlock here"));
            })
        },
    )
    .await;
}

/// About the account the host shares, so a session is not needed to ask.
#[tokio::test]
async fn the_usage_window_is_reported_wherever_it_is_asked_about() {
    with_daemon(
        DaemonCase {
            describe_usage: Some(Arc::new(|| {
                Box::pin(async {
                    "58% of the provider's usage window is left, and it resets in 2 hours"
                        .to_owned()
                })
            })),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .daemon
                    .handle(raw("!usage"), InboundDecision::Start)
                    .await;

                assert!(
                    harness.replies.lock().unwrap()[0]
                        .contains("58% of the provider's usage window is left")
                );
                assert!(created(&harness.threads).is_empty());
                assert_eq!(
                    harness
                        .daemon
                        .run_command(&SlashCommand {
                            thread_id: None,
                            user_id: OWNER.to_owned(),
                            user_name: "amelia".to_owned(),
                            content: "!usage".to_owned(),
                        })
                        .await,
                    harness.replies.lock().unwrap()[0].clone()
                );
            })
        },
    )
    .await;
}

#[tokio::test]
async fn a_provider_that_meters_nothing_says_so_rather_than_inventing_a_number() {
    with_daemon(DaemonCase::default(), |harness| {
        Box::pin(async move {
            harness
                .daemon
                .handle(raw("!usage"), InboundDecision::Start)
                .await;

            assert!(harness.replies.lock().unwrap()[0].contains("does not report a usage window"));
        })
    })
    .await;
}

/// Memory is about a person, not a session, so asking should not need a
/// thread.
#[tokio::test]
async fn facts_is_answered_in_the_channel_with_no_session_running() {
    let memory = Arc::new(MemoryStore::open(":memory:").expect("the store opens"));
    memory
        .remember(Scope::User, OWNER, "prefers jj over git", "earlier", 0)
        .unwrap();

    with_daemon(
        DaemonCase {
            memory: Some(Arc::clone(&memory)),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .daemon
                    .handle(raw("!facts"), InboundDecision::Start)
                    .await;

                assert!(
                    harness
                        .replies
                        .lock()
                        .unwrap()
                        .join("\n")
                        .contains("prefers jj over git")
                );
                // Answered outright: no thread was opened and no session
                // started.
                assert!(created(&harness.threads).is_empty());
            })
        },
    )
    .await;
}

#[tokio::test]
async fn a_project_is_a_threads_own_so_the_channel_says_to_ask_there() {
    let memory = Arc::new(MemoryStore::open(":memory:").expect("the store opens"));
    with_daemon(
        DaemonCase {
            memory: Some(Arc::clone(&memory)),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .daemon
                    .handle(raw("!facts project"), InboundDecision::Start)
                    .await;
                assert!(
                    harness
                        .replies
                        .lock()
                        .unwrap()
                        .join("\n")
                        .contains("ask in one")
                );
            })
        },
    )
    .await;
}

/// Owner means nothing in a channel, so forgetting there is the operator's.
#[tokio::test]
async fn forget_in_the_channel_is_refused_to_anyone_but_an_operator() {
    let memory = Arc::new(MemoryStore::open(":memory:").expect("the store opens"));
    memory
        .remember(Scope::User, OWNER, "prefers jj over git", "earlier", 0)
        .unwrap();

    with_daemon(
        DaemonCase {
            memory: Some(Arc::clone(&memory)),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .daemon
                    .handle(raw(&format!("!forget <@{OWNER}>")), InboundDecision::Start)
                    .await;

                assert!(
                    harness
                        .replies
                        .lock()
                        .unwrap()
                        .join("\n")
                        .contains("only an operator")
                );
                assert_eq!(memory.facts_for(Scope::User, OWNER, 100).unwrap().len(), 1);
            })
        },
    )
    .await;
}

/// Inside a thread the session answers, because it knows the project.
#[tokio::test]
async fn in_a_thread_the_daemon_leaves_memory_to_the_session() {
    let memory = Arc::new(MemoryStore::open(":memory:").expect("the store opens"));
    memory
        .remember(Scope::User, OWNER, "prefers jj over git", "earlier", 0)
        .unwrap();

    with_daemon(
        DaemonCase {
            memory: Some(Arc::clone(&memory)),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .daemon
                    .handle(
                        raw("!facts"),
                        InboundDecision::Thread {
                            thread_id: "t1".to_owned(),
                        },
                    )
                    .await;
                // Not answered here, so it falls through to whatever the
                // thread holds.
                assert!(
                    !harness
                        .replies
                        .lock()
                        .unwrap()
                        .join("\n")
                        .contains("prefers jj over git")
                );
            })
        },
    )
    .await;
}

#[tokio::test]
async fn an_operator_may_forget_from_the_channel_and_is_told_what_went() {
    let memory = Arc::new(MemoryStore::open(":memory:").expect("the store opens"));
    memory
        .remember(Scope::User, OWNER, "prefers jj over git", "earlier", 0)
        .unwrap();

    with_daemon(
        DaemonCase {
            memory: Some(Arc::clone(&memory)),
            settings: Some(json!({
                "chat": {
                    "token": "t",
                    "channelId": "chan",
                    "allowedUserIds": [OWNER],
                    "operatorUserIds": [OWNER],
                },
            })),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .daemon
                    .handle(raw(&format!("!forget <@{OWNER}>")), InboundDecision::Start)
                    .await;

                assert!(
                    harness
                        .replies
                        .lock()
                        .unwrap()
                        .join("\n")
                        .contains("forgot 1 fact about")
                );
                assert_eq!(memory.facts_for(Scope::User, OWNER, 100).unwrap().len(), 0);
            })
        },
    )
    .await;
}

/// The builder names the backend configuration asked for, never a fallback.
#[test]
fn create_sandbox_names_the_configured_backend() {
    let root = tempfile::tempdir().unwrap();
    let config = config_with(&json!({
        "projectRoot": root.path().join("projects").display().to_string(),
        "stateDir": root.path().join("state").display().to_string(),
    }));
    let sandbox = create_sandbox(&config, silent(), None, None);
    assert!(matches!(sandbox, crate::sandbox::Backend::Bailey(_)));

    let config = config_with(&json!({
        "projectRoot": root.path().join("projects").display().to_string(),
        "stateDir": root.path().join("state").display().to_string(),
        "sandbox": { "backend": "podman" },
    }));
    let sandbox = create_sandbox(&config, silent(), None, None);
    assert!(matches!(sandbox, crate::sandbox::Backend::Podman(_)));
}
