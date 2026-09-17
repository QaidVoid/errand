//! Tests for the session manager, ported from `manager_test.ts`.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::watch;

use super::{
    CreatedThread, DetachedRequest, FoundView, MadeThread, ManagerOptions, SandboxPool,
    SessionManager, StartOutcome, ThreadFactory,
};
use crate::admission::scheduler::{Clock, Scheduler, Timer};
use crate::agent::client::AgentProcess;
use crate::config::validate::validate_config;
use crate::log::{LogFields, Logger};
use crate::sandbox::backend::{
    CapabilityReport, SandboxLaunch, SandboxLaunchError, SandboxUnavailableError,
};
use crate::sandbox::paths;
use crate::session::event::EndReason;
use crate::session::record::record_dir;
use crate::session::registry::ThreadRegistry;
use crate::session::session::{IncomingMessage, RunningBox, SessionHandle};
use crate::session::transcript::TRANSCRIPT_FILENAME;
use crate::session::views::{SessionView, ViewError};

const OWNER: &str = "100000000000000001";

fn silent() -> Logger {
    Logger::new(LogFields::new(), Arc::new(|_level, _line| {}))
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
}

/// An agent that answers its readiness call and then says nothing.
struct QuietAgent {
    queue: Mutex<std::collections::VecDeque<u8>>,
    written: Mutex<Vec<String>>,
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
            written: Mutex::new(Vec::new()),
            closed: Mutex::new(false),
            exit: exit_receiver.clone(),
        });
        let controls = QuietControls {
            fake: Arc::clone(&fake),
            exit: exit_receiver,
            sender: Arc::new(Mutex::new(Some(exit_sender))),
        };
        (fake, controls)
    }
}

impl QuietControls {
    fn send(&self, record: &serde_json::Value) {
        self.fake
            .queue
            .lock()
            .unwrap()
            .extend(format!("{record}\n").into_bytes());
    }

    fn end(&self, code: i32) {
        {
            let mut closed = self.fake.closed.lock().unwrap();
            if *closed {
                return;
            }
            *closed = true;
        }
        self.fake.queue.lock().unwrap().clear();
        if let Some(sender) = self.sender.lock().unwrap().take() {
            let _ = sender.send(Some(code));
        }
    }

    fn written(&self) -> Vec<String> {
        self.fake.written.lock().unwrap().clone()
    }
}

impl AgentProcess for QuietAgent {
    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let line = String::from_utf8_lossy(bytes).trim().to_owned();
        let id = serde_json::from_str::<Value>(&line)
            .ok()
            .and_then(|parsed| parsed.get("id").cloned());
        self.written.lock().unwrap().push(line);
        if let Some(id) = id {
            self.queue.lock().unwrap().extend(
                json!({ "type": "response", "id": id, "success": true })
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
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
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

/// A sandbox whose launches are quiet agents the manager drives.
struct FakeSandbox {
    launched: Mutex<Vec<SandboxLaunch>>,
    agents: Mutex<Vec<(Arc<QuietAgent>, QuietControls)>>,
    orphans: Mutex<Vec<String>>,
    removed: Mutex<Vec<String>>,
}

impl FakeSandbox {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            launched: Mutex::new(Vec::new()),
            agents: Mutex::new(Vec::new()),
            orphans: Mutex::new(Vec::new()),
            removed: Mutex::new(Vec::new()),
        })
    }

    fn written(&self) -> String {
        self.agents
            .lock()
            .unwrap()
            .first()
            .map(|(agent, _)| agent.written.lock().unwrap().join("\n"))
            .unwrap_or_default()
    }

    /// Settles the newest agent's turn, the way a finished model would.
    fn settle_latest(&self) {
        let controls = self
            .agents
            .lock()
            .unwrap()
            .last()
            .map(|(_, controls)| controls.clone())
            .expect("no agent has been launched");
        controls.send(&json!({ "type": "agent_start" }));
        controls.send(&json!({
            "type": "message_end",
            "message": { "role": "assistant", "content": [{ "type": "text", "text": "done" }] },
        }));
        controls.send(&json!({ "type": "turn_end", "usage": {
            "input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2, "cost": 0,
        } }));
        controls.send(&json!({ "type": "agent_settled" }));
    }
}

impl SandboxPool for FakeSandbox {
    fn probe(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<CapabilityReport, SandboxUnavailableError>> + Send + '_>>
    {
        Box::pin(async move {
            Ok(CapabilityReport {
                backend: crate::config::schema::SandboxBackend::Bailey,
                gaps: Vec::new(),
                notes: Vec::new(),
            })
        })
    }

    fn launch(
        self: Arc<Self>,
        launch: SandboxLaunch,
    ) -> Pin<Box<dyn Future<Output = Result<RunningBox, SandboxLaunchError>> + Send>> {
        Box::pin(async move {
            let (agent, controls) = QuietAgent::new();
            self.agents.lock().unwrap().push((agent, controls.clone()));
            self.launched.lock().unwrap().push(launch.clone());
            let project_path = launch.project_path.clone();
            Ok(RunningBox {
                process: controls.fake.clone(),
                to_host_path: Arc::new(move |path: &str| {
                    paths::host_path_under("/workspace", &project_path, path)
                }),
                stop: Arc::new(move || {
                    controls.end(0);
                    Box::pin(async { false }) as Pin<Box<dyn Future<Output = bool> + Send>>
                }),
            })
        })
    }

    fn list_orphans(&self) -> Pin<Box<dyn Future<Output = Vec<String>> + Send + '_>> {
        Box::pin(async { self.orphans.lock().unwrap().clone() })
    }

    fn remove_orphans<'a>(
        &'a self,
        names: &'a [String],
    ) -> Pin<Box<dyn Future<Output = usize> + Send + 'a>> {
        Box::pin(async move {
            self.removed.lock().unwrap().extend(names.iter().cloned());
            names.len()
        })
    }
}

/// Threads a test can inspect: what was created, and what was let go.
struct FakeThreads {
    created: Mutex<Vec<String>>,
    released: Mutex<Vec<String>>,
    closed: Arc<Mutex<Vec<EndReason>>>,
    refuse: Mutex<Option<String>>,
    next: Mutex<u32>,
}

impl FakeThreads {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            created: Mutex::new(Vec::new()),
            released: Mutex::new(Vec::new()),
            closed: Arc::new(Mutex::new(Vec::new())),
            refuse: Mutex::new(None),
            next: Mutex::new(1),
        })
    }

    fn view(&self) -> Arc<QuietView> {
        Arc::new(QuietView {
            closed: Arc::clone(&self.closed),
        })
    }

    fn refuse_with(&self, why: Option<&str>) {
        *self.refuse.lock().unwrap() = why.map(str::to_owned);
    }

    fn made(&self) -> Vec<String> {
        self.created.lock().unwrap().clone()
    }
}

/// A thread view that keeps only what a manager test needs to look at.
struct QuietView {
    closed: Arc<Mutex<Vec<EndReason>>>,
}

impl SessionView for QuietView {
    fn observe<'a>(
        &'a self,
        event: &'a crate::session::event::SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), ViewError>> + Send + 'a>> {
        Box::pin(async move {
            if let crate::session::event::SessionEvent::Close { reason } = event {
                self.closed.lock().unwrap().push(*reason);
            }
            Ok(())
        })
    }
}

impl ThreadFactory for FakeThreads {
    fn create(self: Arc<Self>, _message: IncomingMessage, name: String) -> MadeThread {
        Box::pin(async move {
            if let Some(refuse) = self.refuse.lock().unwrap().clone() {
                return Err(refuse);
            }
            self.created.lock().unwrap().push(name);
            let id = format!("thread-{}", *self.next.lock().unwrap());
            *self.next.lock().unwrap() += 1;
            Ok(CreatedThread {
                id,
                view: self.view(),
            })
        })
    }

    fn open(self: Arc<Self>, name: String, _opener: String) -> MadeThread {
        Box::pin(async move {
            if let Some(refuse) = self.refuse.lock().unwrap().clone() {
                return Err(refuse);
            }
            self.created.lock().unwrap().push(name);
            let id = format!("thread-{}", *self.next.lock().unwrap());
            *self.next.lock().unwrap() += 1;
            Ok(CreatedThread {
                id,
                view: self.view(),
            })
        })
    }

    fn port_for(self: Arc<Self>, thread_id: String) -> FoundView {
        Box::pin(async move {
            if thread_id.starts_with("thread-") {
                return Some(self.view() as Arc<dyn SessionView>);
            }
            None
        })
    }

    fn release(&self, thread_id: &str) {
        self.released.lock().unwrap().push(thread_id.to_owned());
    }
}

struct NoClock;

impl Clock for NoClock {
    fn now(&self) -> i64 {
        0
    }

    fn set_timeout(&self, _action: Timer, _ms: i64) -> u64 {
        0
    }

    fn clear_timeout(&self, _handle: u64) {}
}

fn message(content: &str, id: &str) -> IncomingMessage {
    IncomingMessage {
        id: id.to_owned(),
        author_id: OWNER.to_owned(),
        author_name: Some("amelia".to_owned()),
        content: content.to_owned(),
        attachments: Vec::new(),
    }
}

struct Harness {
    manager: SessionManager,
    sandbox: Arc<FakeSandbox>,
    threads: Arc<FakeThreads>,
    registry: Arc<Mutex<ThreadRegistry>>,
    scheduler: Arc<Scheduler>,
    root: tempfile::TempDir,
}

fn config_with(
    root: &std::path::Path,
    overrides: &serde_json::Value,
) -> crate::config::schema::Config {
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
        "projectRoot": root.join("projects").display().to_string(),
        "stateDir": root.join("state").display().to_string(),
    });
    if let Some(overrides) = overrides.as_object() {
        for (key, value) in overrides {
            base[key.as_str()] = value.clone();
        }
    }
    validate_config(&base).expect("the test configuration is accepted")
}

async fn with_manager(run: impl FnOnce(&Harness) -> Pin<Box<dyn Future<Output = ()> + '_>>) {
    with_manager_options(&json!({}), None, run).await;
}

async fn with_manager_options(
    overrides: &serde_json::Value,
    unavailable: Option<crate::session::session::Unavailable>,
    run: impl FnOnce(&Harness) -> Pin<Box<dyn Future<Output = ()> + '_>>,
) {
    let root = tempfile::tempdir().expect("a temp directory");
    let settings = config_with(root.path(), overrides);
    let sandbox = FakeSandbox::new();
    let threads = FakeThreads::new();
    let registry = Arc::new(Mutex::new(ThreadRegistry::new(
        crate::session::registry::ThreadRegistry::path_for(
            root.path().display().to_string().as_str(),
        ),
        silent(),
    )));
    let scheduler = Scheduler::start(
        settings.limits.clone(),
        Arc::new(NoClock),
        5_000,
        300_000,
        750,
    );
    let id = Mutex::new(0);

    let manager = SessionManager::new(ManagerOptions {
        config: settings,
        sandbox: Arc::clone(&sandbox) as Arc<dyn SandboxPool>,
        scheduler: Arc::clone(&scheduler),
        threads: Arc::clone(&threads) as Arc<dyn ThreadFactory>,
        registry: Arc::clone(&registry),
        log: silent(),
        make_id: Some(Arc::new(move || {
            let mut id = id.lock().unwrap();
            *id += 1;
            format!("s{id}")
        })),
        unavailable,
        operator_ids: None,
        memory: None,
        describe_images: None,
        public_url: None,
        available_models: Vec::new(),
        delegate_base_url: None,
        now: Some(Arc::new(|| 1_000)),
    });

    let harness = Harness {
        manager,
        sandbox,
        threads,
        registry,
        scheduler,
        root,
    };

    run(&harness).await;

    harness.manager.shutdown().await;
}

fn started(outcome: &StartOutcome) -> &SessionHandle {
    outcome.session().expect("the session started")
}

fn refused_reason(outcome: &StartOutcome) -> String {
    outcome.refused_reason().to_owned()
}

#[tokio::test]
async fn starting_a_session_creates_its_thread_project_and_state() {
    with_manager(|harness| {
        Box::pin(async move {
            let outcome = harness
                .manager
                .start(message("demo: fix the parser", "m1"))
                .await;

            assert!(outcome.is_started());
            assert_eq!(harness.threads.made(), ["demo: fix the parser"]);
            assert!(harness.root.path().join("projects").join("demo").is_dir());
            assert_eq!(
                harness.sandbox.launched.lock().unwrap()[0].project_path,
                harness
                    .root
                    .path()
                    .join("projects")
                    .join("demo")
                    .display()
                    .to_string()
            );
            assert_eq!(
                harness
                    .registry
                    .lock()
                    .unwrap()
                    .get("thread-1")
                    .map(|record| record.project_name.clone()),
                Some("demo".to_owned())
            );
        })
    })
    .await;
}

/// The prefix names the project; what follows it is what was asked.
#[tokio::test]
async fn the_project_prefix_is_stripped_from_the_prompt_the_agent_sees() {
    with_manager(|harness| {
        Box::pin(async move {
            harness
                .manager
                .start(message("demo: fix the parser", "m1"))
                .await;
            settle().await;

            let sent = harness.sandbox.written();
            assert!(sent.contains("fix the parser"));
            assert!(!sent.contains("demo: fix"));
        })
    })
    .await;
}

/// Two agents in one working tree edit the same files with neither able to
/// see the other's changes.
#[tokio::test]
async fn a_project_that_already_has_a_session_is_refused_a_second_one() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: first", "m1")).await;

            let second = harness.manager.start(message("demo: second", "m2")).await;

            assert!(!second.is_started());
            assert!(refused_reason(&second).contains("already has a live session"));
        })
    })
    .await;
}

#[tokio::test]
async fn two_different_projects_run_side_by_side() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("one: work", "m1")).await;
            let second = harness.manager.start(message("two: work", "m2")).await;

            assert!(second.is_started());
            assert_eq!(harness.manager.sessions().len(), 2);
        })
    })
    .await;
}

/// A thread nobody can see would leave an agent running that nobody can stop.
#[tokio::test]
async fn no_thread_means_no_sandbox_and_the_slot_is_given_back() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.threads.refuse_with(Some("the channel is gone"));

            let outcome = harness.manager.start(message("demo: go", "m1")).await;

            assert!(!outcome.is_started());
            assert!(harness.sandbox.launched.lock().unwrap().is_empty());
            assert!(harness.manager.sessions().is_empty());
            // The slot came back, so the next start is not refused for want
            // of one.
            harness.threads.refuse_with(None);
            assert!(
                harness
                    .manager
                    .start(message("demo: go", "m2"))
                    .await
                    .is_started()
            );
        })
    })
    .await;
}

#[tokio::test]
async fn a_spent_window_refuses_before_a_thread_or_a_sandbox_exists() {
    with_manager_options(
        &json!({}),
        Some(Arc::new(|_provider| {
            Box::pin(async { Some("come back at nine".to_owned()) })
                as Pin<Box<dyn Future<Output = Option<String>> + Send>>
        })),
        |harness| {
            Box::pin(async move {
                let outcome = harness.manager.start(message("demo: go", "m1")).await;

                assert!(!outcome.is_started());
                assert!(refused_reason(&outcome).contains("come back at nine"));
                assert!(harness.threads.made().is_empty());
                assert!(harness.sandbox.launched.lock().unwrap().is_empty());
            })
        },
    )
    .await;
}

#[tokio::test]
async fn more_sessions_than_the_host_allows_are_refused_with_a_reason() {
    with_manager_options(
        &json!({ "limits": { "maxLiveSessions": 1 } }),
        None,
        |harness| {
            Box::pin(async move {
                harness.manager.start(message("one: work", "m1")).await;

                let second = harness.manager.start(message("two: work", "m2")).await;

                assert!(!second.is_started());
                assert!(refused_reason(&second).contains("session"));
            })
        },
    )
    .await;
}

#[tokio::test]
async fn a_message_reaches_the_session_bound_to_its_thread_and_no_other() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: go", "m1")).await;

            assert!(
                harness
                    .manager
                    .deliver("thread-1", message("more", "m2"))
                    .await
            );
            assert!(
                !harness
                    .manager
                    .deliver("thread-404", message("more", "m3"))
                    .await
            );
        })
    })
    .await;
}

#[tokio::test]
async fn a_session_that_ends_lets_go_of_its_thread_and_can_be_resumed() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: go", "m1")).await;

            harness
                .manager
                .end_thread("thread-1", EndReason::Idle)
                .await;
            settle().await;

            assert!(harness.manager.for_thread("thread-1").is_none());
            assert!(harness.manager.is_finished_thread("thread-1"));
            assert_eq!(
                *harness.threads.released.lock().unwrap(),
                ["thread-1".to_owned()]
            );
            // Idling out is not being finished with: the history outlives the
            // sandbox.
            assert_eq!(
                harness
                    .registry
                    .lock()
                    .unwrap()
                    .get("thread-1")
                    .map(|record| record.session_id.clone()),
                Some("demo-s1".to_owned())
            );
            assert!(harness.manager.can_resume("thread-1"));
            assert_eq!(harness.manager.resumable().len(), 1);
        })
    })
    .await;
}

/// Stopping is how somebody says they are finished with a thread.
#[tokio::test]
async fn a_session_that_was_stopped_is_not_offered_for_resuming() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: go", "m1")).await;

            harness
                .manager
                .end_thread("thread-1", EndReason::Stopped)
                .await;
            settle().await;

            assert!(harness.registry.lock().unwrap().get("thread-1").is_none());
            assert!(!harness.manager.can_resume("thread-1"));
            assert!(harness.manager.resumable().is_empty());
        })
    })
    .await;
}

#[tokio::test]
async fn resuming_picks_the_thread_up_where_it_stopped() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: go", "m1")).await;
            harness
                .manager
                .end_thread("thread-1", EndReason::Idle)
                .await;
            settle().await;

            let outcome = harness
                .manager
                .resume("thread-1", message("carry on", "m2"))
                .await;

            assert!(outcome.is_started());
            assert_eq!(started(&outcome).id(), "demo-s1");
            let launched = harness.sandbox.launched.lock().unwrap();
            assert!(launched[1].resume);
            assert_eq!(
                launched[1].state_dir,
                harness
                    .root
                    .path()
                    .join("state")
                    .join("demo-s1")
                    .display()
                    .to_string()
            );
        })
    })
    .await;
}

/// A new exchange labelled with a number already used reads as the same one.
#[tokio::test]
async fn a_resumed_session_carries_on_the_turn_numbering() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: go", "m1")).await;
            harness
                .manager
                .end_thread("thread-1", EndReason::Idle)
                .await;
            settle().await;
            let state = harness
                .root
                .path()
                .join("state")
                .join("demo-s1")
                .display()
                .to_string();
            let record = record_dir(state.as_str());
            let transcript = std::path::Path::new(&record).join(TRANSCRIPT_FILENAME);
            let written = std::fs::read_to_string(&transcript).unwrap();
            assert!(written.contains("\"turn\":1"));

            harness
                .manager
                .resume("thread-1", message("carry on", "m2"))
                .await;

            let after = std::fs::read_to_string(&transcript).unwrap();
            assert!(after.contains("\"turn\":2"));
        })
    })
    .await;
}

#[tokio::test]
async fn a_thread_this_daemon_never_saw_is_not_resumed() {
    with_manager(|harness| {
        Box::pin(async move {
            let outcome = harness
                .manager
                .resume("thread-404", message("carry on", "m1"))
                .await;

            assert!(!outcome.is_started());
            assert!(refused_reason(&outcome).contains("not one of mine to resume"));
        })
    })
    .await;
}

#[tokio::test]
async fn a_thread_that_already_has_a_session_is_not_resumed_on_top_of_it() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: go", "m1")).await;

            let outcome = harness
                .manager
                .resume("thread-1", message("again", "m2"))
                .await;

            assert!(!outcome.is_started());
            assert!(refused_reason(&outcome).contains("already has a live session"));
        })
    })
    .await;
}

/// Somebody invited before a restart must not be silently withdrawn.
#[tokio::test]
async fn who_was_invited_survives_the_session_ending_and_coming_back() {
    with_manager(|harness| {
        Box::pin(async move {
            let started_session = harness
                .manager
                .start(message("demo: go", "m1"))
                .await
                .session()
                .cloned();
            started_session
                .as_ref()
                .expect("a session")
                .handle(IncomingMessage {
                    id: "m2".to_owned(),
                    author_id: OWNER.to_owned(),
                    author_name: None,
                    content: "!allow <@200000000000000002>".to_owned(),
                    attachments: Vec::new(),
                })
                .await;
            assert_eq!(
                harness
                    .registry
                    .lock()
                    .unwrap()
                    .get("thread-1")
                    .map(|record| record.guests.clone()),
                Some(vec!["200000000000000002".to_owned()])
            );

            harness
                .manager
                .end_thread("thread-1", EndReason::Idle)
                .await;
            settle().await;
            let resumed = harness
                .manager
                .resume("thread-1", message("carry on", "m3"))
                .await;

            let guests = match resumed.session() {
                Some(session) => session.guest_list().await,
                None => Vec::new(),
            };
            assert_eq!(guests, vec!["200000000000000002".to_owned()]);
        })
    })
    .await;
}

#[tokio::test]
async fn a_view_can_be_attached_to_a_live_session_and_detached_again() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: go", "m1")).await;
            let watcher = harness.threads.view() as Arc<dyn SessionView>;

            let attached = harness
                .manager
                .attach_view("demo-s1", watcher.clone())
                .await;
            assert!(attached.is_some());
            assert!(
                harness
                    .manager
                    .attach_view("nobody", watcher)
                    .await
                    .is_none()
            );
            if let Some(attached) = attached {
                attached.detach();
            }
        })
    })
    .await;
}

#[tokio::test]
async fn the_thread_a_session_belongs_to_is_known_live_or_remembered() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: go", "m1")).await;
            assert_eq!(
                harness.manager.thread_id_for("demo-s1"),
                Some("thread-1".to_owned())
            );

            harness
                .manager
                .end_thread("thread-1", EndReason::Idle)
                .await;
            settle().await;

            assert_eq!(
                harness.manager.thread_id_for("demo-s1"),
                Some("thread-1".to_owned())
            );
            assert_eq!(harness.manager.thread_id_for("s404"), None);
        })
    })
    .await;
}

/// A crashed daemon must not leave containers running against a project.
#[tokio::test]
async fn sandboxes_left_by_a_previous_run_are_swept_before_anything_starts() {
    with_manager(|harness| {
        Box::pin(async move {
            *harness.sandbox.orphans.lock().unwrap() =
                vec!["errand-old-1".to_owned(), "errand-old-2".to_owned()];

            assert_eq!(harness.manager.sweep_orphans().await, 2);
            assert_eq!(
                *harness.sandbox.removed.lock().unwrap(),
                ["errand-old-1".to_owned(), "errand-old-2".to_owned()]
            );
        })
    })
    .await;
}

#[tokio::test]
async fn shutting_down_ends_every_live_session() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("one: work", "m1")).await;
            harness.manager.start(message("two: work", "m2")).await;
            settle().await;

            harness.manager.shutdown().await;

            assert!(harness.manager.sessions().is_empty());
            assert_eq!(
                *harness.threads.closed.lock().unwrap(),
                [EndReason::Shutdown, EndReason::Shutdown]
            );
        })
    })
    .await;
}

/// A session begun at a keyboard is still announced where a phone will see it.
#[tokio::test]
async fn a_session_started_with_no_message_still_gets_a_thread() {
    with_manager(|harness| {
        Box::pin(async move {
            let outcome = harness
                .manager
                .start_detached(DetachedRequest {
                    project: "demo".to_owned(),
                    prompt: "fix the parser".to_owned(),
                    owner_id: "web-interface".to_owned(),
                    owner_name: Some("the interface".to_owned()),
                })
                .await;

            assert!(outcome.is_started());
            assert_eq!(harness.threads.made(), ["demo: fix the parser"]);
            assert_eq!(
                harness
                    .registry
                    .lock()
                    .unwrap()
                    .get("thread-1")
                    .map(|record| record.owner_id.clone()),
                Some("web-interface".to_owned())
            );
        })
    })
    .await;
}

#[tokio::test]
async fn a_session_started_with_no_project_named_gets_one_of_its_own() {
    with_manager(|harness| {
        Box::pin(async move {
            let outcome = harness
                .manager
                .start_detached(DetachedRequest {
                    project: String::new(),
                    prompt: "have a look at this".to_owned(),
                    owner_id: "web-interface".to_owned(),
                    owner_name: None,
                })
                .await;

            assert_eq!(
                outcome
                    .session()
                    .map(|session| session.project().name.clone()),
                Some("s1".to_owned())
            );
        })
    })
    .await;
}

/// The browser knows sessions, not threads: a thread is one of the surfaces.
#[tokio::test]
async fn a_session_can_be_written_to_by_its_own_identifier() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: go", "m1")).await;

            assert!(
                harness
                    .manager
                    .deliver_to_session("demo-s1", message("more", "m2"))
                    .await
            );
            assert!(
                !harness
                    .manager
                    .deliver_to_session("s404", message("more", "m3"))
                    .await
            );
        })
    })
    .await;
}

/// Sending to a stopped session is asking for it back.
#[tokio::test]
async fn writing_to_a_session_that_stopped_picks_it_up_again() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: go", "m1")).await;
            harness
                .manager
                .end_thread("thread-1", EndReason::Idle)
                .await;
            settle().await;

            assert!(
                harness
                    .manager
                    .deliver_to_session("demo-s1", message("carry on", "m2"))
                    .await
            );
            assert_eq!(harness.manager.sessions().len(), 1);
        })
    })
    .await;
}

/// By the time there is a thread to type `!model` in, the session has already
/// started on whichever model the configuration named.
#[tokio::test]
async fn the_opening_message_can_choose_the_model_the_session_starts_on() {
    with_manager(|harness| {
        Box::pin(async move {
            harness
                .manager
                .start(message("demo: --model glm-5.3-air look at this", "m1"))
                .await;

            let launched = harness.sandbox.launched.lock().unwrap();
            assert_eq!(launched[0].model, Some("glm-5.3-air".to_owned()));
            // The provider is untouched, because the value named no known one.
            assert_eq!(launched[0].provider, "anthropic");
        })
    })
    .await;
}

#[tokio::test]
async fn a_known_provider_in_front_of_the_model_switches_provider_too() {
    with_manager_options(
        &json!({
            "agent": {
                "provider": "anthropic",
                "credentialName": "ANTHROPIC_API_KEY",
                "credential": "secret",
                "providers": { "meta": { "baseUrl": "https://api.meta.example/v1" } },
            },
        }),
        None,
        |harness| {
            Box::pin(async move {
                harness
                    .manager
                    .start(message(
                        "demo: --model meta/muse-spark-1.3-contributor:max look at this",
                        "m1",
                    ))
                    .await;

                let launched = harness.sandbox.launched.lock().unwrap();
                assert_eq!(launched[0].provider, "meta");
                // The thinking level rides along on the model, for the agent
                // to read.
                assert_eq!(
                    launched[0].model,
                    Some("muse-spark-1.3-contributor:max".to_owned())
                );
            })
        },
    )
    .await;
}

#[tokio::test]
async fn the_flag_is_taken_off_the_prompt_the_agent_is_given() {
    with_manager(|harness| {
        Box::pin(async move {
            harness
                .manager
                .start(message("demo: --model glm-5.3-air look at this", "m1"))
                .await;
            // Nothing of the flag survives into the work the session was
            // asked to do.
            assert_eq!(
                harness.manager.sessions()[0].project().prompt,
                "look at this"
            );
        })
    })
    .await;
}

/// Coming back on a different model is a change nobody asked for, and a quiet
/// one: the answers simply start reading differently.
#[tokio::test]
async fn a_resumed_thread_comes_back_on_the_model_it_was_started_with() {
    with_manager_options(
        &json!({
            "agent": {
                "provider": "anthropic",
                "credentialName": "ANTHROPIC_API_KEY",
                "credential": "secret",
                "providers": { "meta": { "baseUrl": "https://api.meta.example/v1" } },
            },
        }),
        None,
        |harness| {
            Box::pin(async move {
                harness
                    .manager
                    .start(message(
                        "demo: --model meta/muse-spark-1.3-contributor:xhigh go",
                        "m1",
                    ))
                    .await;
                assert_eq!(harness.sandbox.launched.lock().unwrap()[0].provider, "meta");
                harness
                    .manager
                    .end_thread("thread-1", EndReason::Idle)
                    .await;
                settle().await;

                harness
                    .manager
                    .resume("thread-1", message("carry on", "m2"))
                    .await;

                let launched = harness.sandbox.launched.lock().unwrap();
                assert_eq!(launched[1].provider, "meta");
                assert_eq!(
                    launched[1].model,
                    Some("muse-spark-1.3-contributor:xhigh".to_owned())
                );
            })
        },
    )
    .await;
}

#[tokio::test]
async fn a_thread_started_on_the_configured_model_still_resumes_on_it() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: go", "m1")).await;
            harness
                .manager
                .end_thread("thread-1", EndReason::Idle)
                .await;
            settle().await;

            harness
                .manager
                .resume("thread-1", message("carry on", "m2"))
                .await;

            // Nothing was chosen, so nothing is restored over the
            // configuration.
            assert_eq!(
                harness.sandbox.launched.lock().unwrap()[1].provider,
                "anthropic"
            );
        })
    })
    .await;
}

/// Typing the whole name every time is what a short one is for.
#[tokio::test]
async fn a_short_name_starts_the_session_on_the_model_it_stands_for() {
    with_manager_options(
        &json!({
            "agent": {
                "provider": "anthropic",
                "credentialName": "ANTHROPIC_API_KEY",
                "credential": "secret",
                "providers": { "meta": { "baseUrl": "https://api.meta.example/v1" } },
                "aliases": { "muse": "meta/muse-spark-1.3-contributor" },
            },
        }),
        None,
        |harness| {
            Box::pin(async move {
                harness
                    .manager
                    .start(message("demo: --model muse:xhigh go", "m1"))
                    .await;

                let launched = harness.sandbox.launched.lock().unwrap();
                assert_eq!(launched[0].provider, "meta");
                assert_eq!(
                    launched[0].model,
                    Some("muse-spark-1.3-contributor:xhigh".to_owned())
                );
            })
        },
    )
    .await;
}

/// A turn that would not let go is not somebody saying they are done with the
/// thread, so what was being worked on is still there to pick up.
#[tokio::test]
async fn a_thread_force_stopped_for_not_answering_can_still_be_resumed() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: go", "m1")).await;

            harness
                .manager
                .end_thread("thread-1", EndReason::Unresponsive)
                .await;
            settle().await;

            // The record survives, unlike a deliberate stop.
            assert!(harness.registry.lock().unwrap().get("thread-1").is_some());
            let outcome = harness
                .manager
                .resume("thread-1", message("carry on", "m2"))
                .await;
            assert!(outcome.is_started());
        })
    })
    .await;
}

#[tokio::test]
async fn a_deliberate_stop_still_ends_the_thread_for_good() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: go", "m1")).await;

            harness
                .manager
                .end_thread("thread-1", EndReason::Stopped)
                .await;
            settle().await;

            assert!(harness.registry.lock().unwrap().get("thread-1").is_none());
            let outcome = harness
                .manager
                .resume("thread-1", message("carry on", "m2"))
                .await;
            assert!(!outcome.is_started());
        })
    })
    .await;
}

/// A thread that has been resumed can still be moved to another model, and
/// that has to be kept the same way a first run's is, or the move lasts only
/// until the next restart.
#[tokio::test]
async fn a_model_switch_inside_a_resumed_thread_is_remembered_too() {
    with_manager_options(
        &json!({
            "agent": {
                "provider": "anthropic",
                "credentialName": "ANTHROPIC_API_KEY",
                "credential": "secret",
                "providers": { "meta": { "baseUrl": "https://api.meta.example/v1" } },
            },
        }),
        None,
        |harness| {
            Box::pin(async move {
                harness
                    .manager
                    .start(message("demo: --model meta/one go", "m1"))
                    .await;
                harness
                    .manager
                    .end_thread("thread-1", EndReason::Idle)
                    .await;
                settle().await;

                let resumed = harness
                    .manager
                    .resume("thread-1", message("carry on", "m2"))
                    .await;
                assert!(resumed.is_started());
                assert_eq!(
                    harness.sandbox.launched.lock().unwrap()[1].model,
                    Some("one".to_owned())
                );

                // Moved while resumed, the way `!model` does it.
                let session = started(&resumed);
                // The carry-on turn is still running; settle it, the way
                // the agent finishing would, before `!model` is accepted.
                harness.sandbox.settle_latest();
                settle().await;

                session.handle(message("!model two", "m9")).await;
                settle().await;

                assert_eq!(
                    harness
                        .registry
                        .lock()
                        .unwrap()
                        .get("thread-1")
                        .and_then(|record| record.model.clone()),
                    Some("two".to_owned())
                );
                harness
                    .manager
                    .end_thread("thread-1", EndReason::Idle)
                    .await;
                settle().await;

                // And the next resume comes back on it, not on what it
                // started with.
                harness
                    .manager
                    .resume("thread-1", message("again", "m3"))
                    .await;
                assert_eq!(
                    harness.sandbox.launched.lock().unwrap()[2].model,
                    Some("two".to_owned())
                );
            })
        },
    )
    .await;
}

/// Hitting the limit on the configured provider must not take the others with
/// it. The window that matters is the one the prompt would actually be
/// charged to, which the opening message may name with `-m`.
#[tokio::test]
async fn a_spent_default_provider_does_not_refuse_another_provider() {
    with_manager_options(
        &json!({
            "agent": {
                "provider": "anthropic",
                "credentialName": "ANTHROPIC_API_KEY",
                "credential": "secret",
                "providers": { "meta": { "baseUrl": "https://api.meta.example/v1" } },
            },
        }),
        Some(Arc::new(|provider: &str| {
            let known = provider.to_owned();
            Box::pin(async move {
                (known == "anthropic").then(|| "anthropic's usage window is spent".to_owned())
            }) as Pin<Box<dyn Future<Output = Option<String>> + Send>>
        })),
        |harness| {
            Box::pin(async move {
                // Only the configured provider is out of window.
                let started_session = harness
                    .manager
                    .start(message("demo: -m meta/muse explain yourself", "m1"))
                    .await;
                assert!(started_session.is_started());
                assert_eq!(harness.sandbox.launched.lock().unwrap()[0].provider, "meta");
            })
        },
    )
    .await;
}

/// And the configured one is still refused when it is the one being used.
#[tokio::test]
async fn a_spent_provider_still_refuses_a_prompt_that_would_use_it() {
    with_manager_options(
        &json!({
            "agent": {
                "provider": "anthropic",
                "credentialName": "ANTHROPIC_API_KEY",
                "credential": "secret",
                "providers": { "meta": { "baseUrl": "https://api.meta.example/v1" } },
            },
        }),
        Some(Arc::new(|provider: &str| {
            let known = provider.to_owned();
            Box::pin(async move {
                (known == "anthropic").then(|| "anthropic's usage window is spent".to_owned())
            }) as Pin<Box<dyn Future<Output = Option<String>> + Send>>
        })),
        |harness| {
            Box::pin(async move {
                let refused = harness.manager.start(message("demo: just go", "m1")).await;
                assert!(!refused.is_started());
                assert!(refused_reason(&refused).contains("usage window is spent"));
            })
        },
    )
    .await;
}

/// A message deleted after its session ended is reconciled where it lies, and
/// no session is started for it.
#[tokio::test]
async fn an_ended_session_is_not_restarted_for_a_withdrawal() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: go", "m1")).await;
            settle().await;
            harness
                .manager
                .end_thread("thread-1", EndReason::Idle)
                .await;
            settle().await;
            assert!(harness.manager.sessions().is_empty());
            let launches_before = harness.sandbox.launched.lock().unwrap().len();

            // The live session recorded the opening prompt under its chat id.
            assert!(
                harness.manager.withdraw("m1", "thread-1").await,
                "the record held a copy of that message"
            );

            // Nothing was restarted for it: one launch, and the record was
            // reconciled in place.
            assert_eq!(
                harness.sandbox.launched.lock().unwrap().len(),
                launches_before
            );
            let record = record_dir(
                &harness
                    .root
                    .path()
                    .join("state")
                    .join("demo-s1")
                    .display()
                    .to_string(),
            );
            let transcript =
                std::fs::read_to_string(std::path::Path::new(&record).join("transcript.jsonl"))
                    .unwrap();
            assert!(transcript.contains(r#""withdrawn":true"#));
        })
    })
    .await;
}

/// A message nobody here sent withdraws nothing anywhere.
#[tokio::test]
async fn a_withdrawal_nobody_holds_reports_nothing() {
    with_manager(|harness| {
        Box::pin(async move {
            harness.manager.start(message("demo: go", "m1")).await;
            // Let the opening turn settle, so a withdrawal is applied at once
            // rather than held by a turn in progress.
            harness.sandbox.settle_latest();
            settle().await;

            assert!(!harness.manager.withdraw("not-a-message", "thread-1").await);
            assert!(!harness.manager.withdraw("m1", "thread-404").await);
        })
    })
    .await;
}
