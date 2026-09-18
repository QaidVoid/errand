//! Tests for one live session, ported from `session_test.ts`.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use super::{
    DescribeImages, FetchAttachment, FetchBox, IncomingMessage, Launcher, OpenPullRequest,
    RunningBox, SessionHandle, SessionOptions, SessionTimer, Signal, Timers, Unavailable,
};
use crate::admission::scheduler::{Clock, Scheduler, Timer};
use crate::agent::client::AgentProcess;
use crate::config::schema::Config;
use crate::config::validate::validate_config;
use crate::log::{LogFields, Logger};
use crate::memory::store::{MemoryStore, Scope};
use crate::provider::models::AvailableModel;
use crate::sandbox::backend::{SandboxLaunch, SandboxLaunchError};
use crate::sandbox::paths;
use crate::session::attachments::RawAttachment;
use crate::session::event::ToolResult;
use crate::session::event::{EndReason, NoticeLevel, ReactionOutcome, SessionEvent, SessionUsage};
use crate::session::pr::{self, PullRequestError};
use crate::session::projects::ProjectSelection;
use crate::session::record::record_dir;
use crate::session::redacted::Redacting;
use crate::session::views::{SessionView, ViewError, ViewFanOut};

fn silent() -> Logger {
    Logger::new(LogFields::new(), Arc::new(|_level, _line| {}))
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(20)).await;
}

/// A process a test drives, standing in for the agent inside a sandbox.
struct FakeAgent {
    queue: Mutex<std::collections::VecDeque<u8>>,
    errors: Mutex<std::collections::VecDeque<u8>>,
    written: Mutex<Vec<String>>,
    closed: Mutex<bool>,
    writes_fail: Mutex<bool>,
    exit: watch::Receiver<Option<i32>>,
}

#[derive(Clone)]
struct FakeControls {
    fake: Arc<FakeAgent>,
    sender: Arc<Mutex<Option<watch::Sender<Option<i32>>>>>,
}

impl FakeAgent {
    fn new() -> (Arc<Self>, FakeControls) {
        let (exit_sender, exit_receiver) = watch::channel(None);
        let fake = Arc::new(Self {
            queue: Mutex::new(std::collections::VecDeque::new()),
            errors: Mutex::new(std::collections::VecDeque::new()),
            written: Mutex::new(Vec::new()),
            closed: Mutex::new(false),
            writes_fail: Mutex::new(false),
            exit: exit_receiver.clone(),
        });
        (
            Arc::clone(&fake),
            FakeControls {
                fake,
                sender: Arc::new(Mutex::new(Some(exit_sender))),
            },
        )
    }
}

impl FakeControls {
    fn send(&self, record: &Value) {
        self.chunk(&format!("{record}\n"));
    }

    fn chunk(&self, text: &str) {
        self.fake.queue.lock().unwrap().extend(text.bytes());
    }

    fn written(&self) -> Vec<String> {
        self.fake.written.lock().unwrap().clone()
    }

    fn write_count(&self) -> usize {
        self.fake.written.lock().unwrap().len()
    }

    /// Answers the most recent request, by its correlation id.
    fn answer(&self, data: &Value) {
        let written = self.written();
        let last = written.iter().rev().find(|line| line.contains("\"id\""));
        let id = last.and_then(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|parsed| parsed.get("id").cloned())
        });
        self.send(&json!({ "type": "response", "id": id, "success": true, "data": data }));
    }

    /// Reports a whole turn: it starts, speaks, costs something, and settles.
    fn run_turn_saying(&self, text: &str) {
        self.send(&json!({ "type": "agent_start" }));
        self.send(&json!({
            "type": "message_end",
            "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] },
        }));
        self.send(&json!({
            "type": "turn_end",
            "usage": { "input": 10, "output": 2, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 12, "cost": 0.01 },
        }));
        self.send(&json!({ "type": "agent_settled" }));
    }

    /// Writes to stderr, as a dying process does.
    fn complain(&self, text: &str) {
        self.fake
            .errors
            .lock()
            .unwrap()
            .extend(format!("{text}\n").bytes());
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
        self.fake.errors.lock().unwrap().clear();
        if let Some(sender) = self.sender.lock().unwrap().take() {
            let _ = sender.send(Some(code));
        }
    }
}

impl AgentProcess for FakeAgent {
    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        if *self.writes_fail.lock().unwrap() {
            return Err(std::io::Error::other("the agent has gone"));
        }
        self.written
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(bytes).trim().to_owned());
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
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = std::io::Result<usize>> + Send + 'a>> {
        Box::pin(async move {
            loop {
                {
                    let mut errors = self.errors.lock().unwrap();
                    let count = errors.len().min(buf.len());
                    if count > 0 {
                        for (index, byte) in errors.drain(..count).enumerate() {
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

    fn exited(&self) -> Pin<Box<dyn Future<Output = Option<i32>> + Send>> {
        let mut receiver = self.exit.clone();
        Box::pin(async move {
            let _ = receiver.changed().await;
            *receiver.borrow()
        })
    }
}

/// Records everything a session reports.
struct FakeThread {
    events: Mutex<Vec<SessionEvent>>,
}

impl FakeThread {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
        })
    }

    fn pick<T: 'static>(&self, take: impl Fn(&SessionEvent) -> Option<T>) -> Vec<T> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(take)
            .collect()
    }

    fn posts(&self) -> Vec<String> {
        self.pick(|event| match event {
            SessionEvent::Post { text } => Some(text.clone()),
            _ => None,
        })
    }

    fn notices(&self) -> Vec<(String, NoticeLevel)> {
        self.pick(|event| match event {
            SessionEvent::Notice { text, level } => Some((text.clone(), *level)),
            _ => None,
        })
    }

    fn replies(&self) -> Vec<(String, String)> {
        self.pick(|event| match event {
            SessionEvent::Reply { text, command } => Some((text.clone(), command.clone())),
            _ => None,
        })
    }

    fn prompts(&self) -> Vec<String> {
        self.pick(|event| match event {
            SessionEvent::Prompt { author, text, .. } => Some(format!("{author}: {text}")),
            _ => None,
        })
    }

    fn asides(&self) -> Vec<String> {
        self.pick(|event| match event {
            SessionEvent::Aside { author, text, .. } => Some(format!("{author}: {text}")),
            _ => None,
        })
    }

    fn activity(&self) -> Vec<String> {
        self.pick(|event| match event {
            SessionEvent::Activity { line, .. } => Some(line.clone()),
            _ => None,
        })
    }

    fn diffs(&self) -> Vec<(String, u64, u64)> {
        self.pick(|event| match event {
            SessionEvent::Diff {
                path,
                added,
                removed,
                ..
            } => Some((path.clone(), *added, *removed)),
            _ => None,
        })
    }

    fn reactions(&self) -> Vec<(String, ReactionOutcome)> {
        self.pick(|event| match event {
            SessionEvent::Reaction {
                message_id,
                outcome,
            } => Some((message_id.clone(), *outcome)),
            _ => None,
        })
    }

    fn uploads(&self) -> Vec<(String, usize)> {
        self.pick(|event| match event {
            SessionEvent::Upload { name, bytes, .. } => Some((name.clone(), bytes.len())),
            _ => None,
        })
    }

    fn turns(&self) -> Vec<u32> {
        self.pick(|event| match event {
            SessionEvent::BeginTurn { turn } => Some(*turn),
            _ => None,
        })
    }

    fn results(&self) -> Vec<ToolResult> {
        self.pick(|event| match event {
            SessionEvent::ToolResult { result } => Some(result.clone()),
            _ => None,
        })
    }

    fn usage(&self) -> Option<SessionUsage> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|event| match event {
                SessionEvent::Usage { usage } => Some(usage.clone()),
                _ => None,
            })
    }

    fn busy(&self) -> bool {
        self.events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|event| match event {
                SessionEvent::Busy { busy } => Some(*busy),
                _ => None,
            })
            .unwrap_or(false)
    }

    fn closed(&self) -> Option<EndReason> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|event| match event {
                SessionEvent::Close { reason } => Some(*reason),
                _ => None,
            })
    }

    /// Everything said, however it was said, for one assertion over the lot.
    fn everything(&self) -> String {
        let events = self.events.lock().unwrap();
        events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::Post { text }
                | SessionEvent::Notice { text, .. }
                | SessionEvent::Reply { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The reaction a reader is left looking at.
    fn final_reaction(&self, message_id: &str) -> Option<ReactionOutcome> {
        self.reactions()
            .into_iter()
            .rev()
            .find(|(id, _)| id == message_id)
            .map(|(_, outcome)| outcome)
    }
}

impl SessionView for FakeThread {
    fn observe<'a>(
        &'a self,
        event: &'a SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), ViewError>> + Send + 'a>> {
        Box::pin(async move {
            self.events.lock().unwrap().push(event.clone());
            Ok(())
        })
    }
}

/// A sandbox whose launches are fake agents the test drives.
struct FakeSandbox {
    agents: Mutex<Vec<(Arc<FakeAgent>, FakeControls)>>,
    launched: Mutex<Vec<SandboxLaunch>>,
    stopped: Mutex<Vec<String>>,
    launch_fails: Mutex<Option<String>>,
}

impl FakeSandbox {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            agents: Mutex::new(Vec::new()),
            launched: Mutex::new(Vec::new()),
            stopped: Mutex::new(Vec::new()),
            launch_fails: Mutex::new(None),
        })
    }

    fn latest_controls(&self) -> FakeControls {
        self.agents
            .lock()
            .unwrap()
            .last()
            .map(|(_, controls)| controls.clone())
            .expect("no agent has been launched")
    }

    fn launcher(self: &Arc<Self>) -> Launcher {
        let sandbox = Arc::clone(self);
        Arc::new(move |launch: SandboxLaunch| {
            let sandbox = Arc::clone(&sandbox);
            Box::pin(async move {
                if let Some(fails) = sandbox.launch_fails.lock().unwrap().clone() {
                    return Err(SandboxLaunchError(fails));
                }
                let (agent, controls) = FakeAgent::new();
                let stopping = controls.clone();
                sandbox.agents.lock().unwrap().push((agent, controls));
                sandbox.launched.lock().unwrap().push(launch.clone());
                let project_path = launch.project_path.clone();
                let stopped_list = Arc::clone(&sandbox);
                let session_id = launch.session_id.clone();
                Ok(RunningBox {
                    process: stopping.fake.clone(),
                    // The real containment rule, not an approximation: a
                    // double that is more permissive than production tests
                    // nothing worth knowing.
                    to_host_path: Arc::new(move |path: &str| {
                        paths::host_path_under("/workspace", &project_path, path)
                    }),
                    stop: Arc::new(move || {
                        stopped_list
                            .stopped
                            .lock()
                            .unwrap()
                            .push(session_id.clone());
                        stopping.end(0);
                        Box::pin(async { false }) as Pin<Box<dyn Future<Output = bool> + Send>>
                    }),
                })
            })
        })
    }
}

/// Timers the test drives, so a deadline is a decision rather than a wait.
struct TestTimers {
    now: Mutex<i64>,
    next: Mutex<u64>,
    pending: Mutex<Vec<PendingTimer>>,
    sink: Mutex<Option<mpsc::Sender<Signal>>>,
}

struct PendingTimer {
    at: i64,
    timer: SessionTimer,
    handle: u64,
}

impl TestTimers {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(0),
            next: Mutex::new(1),
            pending: Mutex::new(Vec::new()),
            sink: Mutex::new(None),
        })
    }

    fn bind(&self, sink: mpsc::Sender<Signal>) {
        *self.sink.lock().unwrap() = Some(sink);
    }

    /// Advances time, firing everything due, in order.
    fn advance(&self, ms: i64) {
        let target = *self.now.lock().unwrap() + ms;
        loop {
            let due = {
                let mut pending = self.pending.lock().unwrap();
                let index = pending
                    .iter()
                    .enumerate()
                    .filter(|(_, timer)| timer.at <= target)
                    .min_by_key(|(_, timer)| timer.at)
                    .map(|(index, _)| index);
                index.map(|index| pending.remove(index))
            };
            let Some(due) = due else { break };
            *self.now.lock().unwrap() = due.at;
            if let Some(sink) = self.sink.lock().unwrap().as_ref() {
                sink.try_send(Signal::Timer {
                    timer: due.timer,
                    handle: due.handle,
                })
                .expect("the session takes its timers");
            }
        }
        *self.now.lock().unwrap() = target;
    }
}

impl Timers for TestTimers {
    fn set_timeout(&self, timer: SessionTimer, ms: u64) -> u64 {
        let mut next = self.next.lock().unwrap();
        let handle = *next;
        *next += 1;
        let at = *self.now.lock().unwrap() + i64::try_from(ms).unwrap_or(i64::MAX);
        self.pending
            .lock()
            .unwrap()
            .push(PendingTimer { at, timer, handle });
        handle
    }

    fn clear_timeout(&self, handle: u64) {
        self.pending
            .lock()
            .unwrap()
            .retain(|timer| timer.handle != handle);
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

fn config_with(overrides: &Value) -> Arc<Config> {
    let mut base = json!({
        "chat": {
            "token": "a.token.value",
            "channelId": "chan",
            "allowedUserIds": ["100000000000000001"],
        },
        "agent": {
            "provider": "anthropic",
            "providers": {
                "anthropic": { "credentialName": "ANTHROPIC_API_KEY", "credential": "secret" },
            },
        },
        "projectRoot": "/tmp/errand-projects",
        "stateDir": "/tmp/errand-state",
    });
    if let Some(overrides) = overrides.as_object() {
        for (key, value) in overrides {
            base[key.as_str()] = value.clone();
        }
    }
    Arc::new(validate_config(&base).expect("the test configuration is accepted"))
}

const OWNER: &str = "100000000000000001";
const GUEST: &str = "200000000000000002";
const STRANGER: &str = "300000000000000003";

fn message_from(content: &str, from: &str, id: &str) -> IncomingMessage {
    IncomingMessage {
        id: id.to_owned(),
        author_id: from.to_owned(),
        author_name: if from == OWNER {
            Some("amelia".to_owned())
        } else {
            Some(from.to_owned())
        },
        content: content.to_owned(),
        attachments: Vec::new(),
    }
}

fn with_image(content: &str, id: &str) -> IncomingMessage {
    let mut sent = message_from(content, OWNER, id);
    sent.attachments = vec![RawAttachment {
        id: "a1".to_owned(),
        name: "screenshot.png".to_owned(),
        url: "https://files.example/screenshot.png".to_owned(),
        size: PNG.len() as u64,
        content_type: Some("image/png".to_owned()),
    }];
    sent
}

const PNG: &[u8] = &[0x89, 0x50, 0x4e, 0x47];

struct Harness {
    session: SessionHandle,
    thread: Arc<FakeThread>,
    sandbox: Arc<FakeSandbox>,
    timers: Arc<TestTimers>,
    scheduler: Arc<Scheduler>,
    root: tempfile::TempDir,
    state_dir: String,
    ended: Arc<Mutex<Vec<EndReason>>>,
}

impl Harness {
    fn controls(&self) -> FakeControls {
        self.sandbox.latest_controls()
    }

    fn written(&self) -> String {
        self.controls().written().join("\n")
    }

    async fn run_turn(&self) {
        self.controls().run_turn_saying("done");
        settle().await;
    }
}

struct SessionTestCase {
    config: Option<Arc<Config>>,
    first: Option<String>,
    guest_ids: Vec<String>,
    memory: Option<Arc<MemoryStore>>,
    describe_images: Option<DescribeImages>,
    fetch_attachment: Option<FetchAttachment>,
    open_pull_request: Option<OpenPullRequest>,
    unavailable: Option<Unavailable>,
    start: bool,
}

impl Default for SessionTestCase {
    fn default() -> Self {
        Self {
            config: None,
            first: None,
            guest_ids: Vec::new(),
            memory: None,
            describe_images: None,
            fetch_attachment: None,
            open_pull_request: None,
            unavailable: None,
            start: true,
        }
    }
}

/// Starts a session against fakes, and tears down whatever it created.
async fn with_session(
    case: SessionTestCase,
    run: impl FnOnce(&Harness) -> Pin<Box<dyn Future<Output = ()> + '_>>,
) {
    let root = tempfile::tempdir().expect("a temp directory");
    let project = root.path().join("project");
    let state_dir = root.path().join("state");
    std::fs::create_dir_all(&project).expect("the project is made");
    std::fs::create_dir_all(&state_dir).expect("the state directory is made");

    let config = case
        .config
        .clone()
        .unwrap_or_else(|| config_with(&json!({})));
    let thread = FakeThread::new();
    let sandbox = FakeSandbox::new();
    let timers = TestTimers::new();
    let scheduler = Scheduler::start(
        config.limits.clone(),
        Arc::new(NoClock),
        5_000,
        300_000,
        750,
    );
    let ended = Arc::new(Mutex::new(Vec::new()));

    let fanout = Arc::new(ViewFanOut::new(silent()));
    fanout
        .clone()
        .attach(thread.clone() as Arc<dyn SessionView>)
        .await;
    let views = Arc::new(Redacting::new(fanout, Vec::new()));

    let session = SessionHandle::spawn(SessionOptions {
        id: "s1".to_owned(),
        project: ProjectSelection {
            name: "demo".to_owned(),
            path: project.display().to_string(),
            prompt: "do the thing".to_owned(),
            was_explicit: true,
        },
        chosen: None,
        state_dir: state_dir.display().to_string(),
        views,
        launcher: sandbox.launcher(),
        scheduler: Arc::clone(&scheduler),
        config: (*config).clone(),
        log: silent(),
        timers: Some(timers.clone()),
        owner_id: OWNER.to_owned(),
        owner_name: Some("amelia".to_owned()),
        start_turn: None,
        open_pull_request: case.open_pull_request.clone(),
        thread_id: None,
        guild_id: None,
        public_url: None,
        available_models: Vec::new(),
        delegate_base_url: None,
        unavailable: case.unavailable.clone(),
        operator_ids: Vec::new(),
        guest_ids: case.guest_ids.clone(),
        on_guests_changed: None,
        on_model_changed: None,
        memory: case.memory.clone(),
        fetch_attachment: case.fetch_attachment.clone(),
        describe_images: case.describe_images.clone(),
        resume: false,
        on_ended: {
            let ended = Arc::clone(&ended);
            Arc::new(move |why| ended.lock().unwrap().push(why))
        },
    });
    timers.bind(session.commands.clone());

    let harness = Harness {
        session,
        thread,
        sandbox,
        timers,
        scheduler,
        root,
        state_dir: state_dir.display().to_string(),
        ended,
    };

    if case.start {
        // The start is waited on in the background, so the ready answer can
        // arrive while it runs.
        let starting = {
            let session = harness.session.clone();
            let first = message_from(case.first.as_deref().unwrap_or("do the thing"), OWNER, "m1");
            tokio::spawn(async move { session.start(first).await })
        };
        settle().await;
        harness
            .controls()
            .answer(&json!({ "model": { "contextWindow": 200_000 } }));
        let _ = starting.await.expect("start task joins");
    }

    run(&harness).await;

    if !harness.session.is_ended().await {
        harness.session.stop(EndReason::Shutdown).await;
    }
}

#[tokio::test]
async fn a_session_starts_says_so_and_sends_its_first_prompt() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            assert!(
                harness.thread.notices()[0]
                    .0
                    .contains("ready, working in demo")
            );
            assert_eq!(harness.thread.notices()[0].1, NoticeLevel::Started);
            assert_eq!(harness.thread.turns(), [1]);
            assert!(harness.written().contains("do the thing"));
            assert!(harness.thread.busy());
        })
    })
    .await;
}

/// The credential is the whole reason the agent can reach a model at all.
#[tokio::test]
async fn the_sandbox_is_launched_with_the_project_state_and_credential() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            let launched = harness.sandbox.launched.lock().unwrap();
            assert_eq!(launched[0].provider, "anthropic");
            assert_eq!(
                launched[0].env.get("ANTHROPIC_API_KEY"),
                Some(&"secret".to_owned())
            );
            assert!(!launched[0].resume);
        })
    })
    .await;
}

#[tokio::test]
async fn a_sandbox_that_will_not_start_ends_the_session_saying_why() {
    with_session(
        SessionTestCase {
            start: false,
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                *harness.sandbox.launch_fails.lock().unwrap() =
                    Some("no room on this host".to_owned());

                assert!(!harness.session.start(message_from("go", OWNER, "m1")).await);

                assert!(
                    harness
                        .thread
                        .everything()
                        .contains("could not start this session")
                );
                assert!(harness.thread.everything().contains("no room on this host"));
                assert_eq!(*harness.ended.lock().unwrap(), [EndReason::StartupFailed]);
            })
        },
    )
    .await;
}

#[tokio::test]
async fn a_turn_is_reported_priced_and_closed_by_pinging_whoever_asked() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness.controls().run_turn_saying("I did the thing");
            settle().await;

            assert!(
                harness
                    .thread
                    .posts()
                    .iter()
                    .any(|post| post == "I did the thing")
            );
            assert!(harness.thread.everything().contains(&format!("<@{OWNER}>")));
            assert_eq!(harness.thread.usage().unwrap().turns, 1);
            assert_eq!(
                harness.thread.usage().unwrap().context_window,
                Some(200_000)
            );
            assert!(!harness.thread.busy());
            assert_eq!(
                harness.thread.final_reaction("m1"),
                Some(ReactionOutcome::Succeeded)
            );
        })
    })
    .await;
}

/// Saying something to a working agent redirects it: same turn, no new slot.
#[tokio::test]
async fn a_message_during_a_running_turn_steers_it_rather_than_queueing() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness.controls().send(&json!({ "type": "agent_start" }));
            settle().await;

            harness
                .session
                .handle(message_from("actually, do it this way", OWNER, "m2"))
                .await;

            assert_eq!(harness.thread.turns(), [1]);
            assert!(harness.written().contains("\"steer\""));
            assert_eq!(
                harness.thread.final_reaction("m2"),
                Some(ReactionOutcome::Accepted)
            );
        })
    })
    .await;
}

#[tokio::test]
async fn an_aside_is_kept_for_the_thread_and_never_reaches_the_agent() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            let before = harness.controls().write_count();

            harness
                .session
                .handle(message_from(
                    "!!! ignore this, talking to you two",
                    OWNER,
                    "m2",
                ))
                .await;

            assert_eq!(
                harness.thread.asides(),
                ["amelia: ignore this, talking to you two"]
            );
            assert_eq!(harness.controls().write_count(), before);
            assert_eq!(
                harness.thread.final_reaction("m2"),
                Some(ReactionOutcome::Succeeded)
            );
        })
    })
    .await;
}

/// Deciding this later would make the marker unreliable, which defeats it.
#[tokio::test]
async fn an_aside_that_reads_like_a_command_still_runs_nothing() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness
                .session
                .handle(message_from("!!! !stop", OWNER, "m2"))
                .await;

            assert_eq!(harness.thread.asides().len(), 1);
            assert_eq!(harness.thread.closed(), None);
        })
    })
    .await;
}

/// Another bot's command is not this one's to answer, or to pay a model for.
#[tokio::test]
async fn an_unknown_command_is_left_alone_entirely() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            let before = harness.controls().write_count();

            harness
                .session
                .handle(message_from("!somebodyelses thing", OWNER, "m2"))
                .await;

            assert_eq!(harness.controls().write_count(), before);
            assert!(
                harness
                    .thread
                    .reactions()
                    .into_iter()
                    .all(|(id, _)| id != "m2")
            );
        })
    })
    .await;
}

#[tokio::test]
async fn a_stranger_is_refused_once_but_answered_every_time() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness
                .session
                .handle(message_from("let me in", STRANGER, "m2"))
                .await;
            harness
                .session
                .handle(message_from("please", STRANGER, "m3"))
                .await;

            assert_eq!(
                harness
                    .thread
                    .replies()
                    .into_iter()
                    .filter(|(_, command)| command == "refused")
                    .count(),
                1
            );
            assert_eq!(
                harness.thread.final_reaction("m2"),
                Some(ReactionOutcome::Failed)
            );
            assert_eq!(
                harness.thread.final_reaction("m3"),
                Some(ReactionOutcome::Failed)
            );
        })
    })
    .await;
}

#[tokio::test]
async fn a_guest_may_prompt_and_only_the_owner_may_end_the_session() {
    with_session(
        SessionTestCase {
            guest_ids: vec![GUEST.to_owned()],
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .session
                    .handle(message_from("have a look at the tests", GUEST, "m2"))
                    .await;
                harness
                    .session
                    .handle(message_from("!stop", GUEST, "m3"))
                    .await;

                assert!(
                    harness
                        .thread
                        .prompts()
                        .iter()
                        .any(|line| line.contains("have a look"))
                );
                assert!(
                    harness
                        .thread
                        .everything()
                        .contains("who started this session")
                );
                assert_eq!(harness.thread.closed(), None);
            })
        },
    )
    .await;
}

#[tokio::test]
async fn the_owner_can_invite_somebody_and_withdraw_them_again() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness
                .session
                .handle(message_from(&format!("!allow <@{GUEST}>"), OWNER, "m2"))
                .await;
            let allowed = harness.session.guest_list().await;
            harness
                .session
                .handle(message_from(&format!("!deny {GUEST}"), OWNER, "m3"))
                .await;
            let denied = harness.session.guest_list().await;

            assert_eq!(allowed, [GUEST.to_owned()]);
            assert!(denied.is_empty());
            assert!(
                harness
                    .thread
                    .everything()
                    .contains("can now prompt this session")
            );
        })
    })
    .await;
}

#[tokio::test]
async fn inviting_somebody_who_is_not_named_is_refused_not_guessed_at() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness
                .session
                .handle(message_from("!allow amelia", OWNER, "m2"))
                .await;

            assert!(harness.session.guest_list().await.is_empty());
            assert!(harness.thread.everything().contains("say who"));
        })
    })
    .await;
}

#[tokio::test]
async fn a_commands_answer_is_marked_as_that_commands_reply() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness
                .session
                .handle(message_from("!pwd", OWNER, "m2"))
                .await;

            assert!(
                harness
                    .thread
                    .replies()
                    .iter()
                    .any(|(_, command)| command == "!pwd")
            );
            assert!(
                harness
                    .thread
                    .posts()
                    .iter()
                    .all(|post| !post.contains("demo"))
            );
        })
    })
    .await;
}

#[tokio::test]
async fn the_project_can_be_listed_and_read_without_asking_the_agent() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            std::fs::write(
                harness.root.path().join("project").join("readme.md"),
                "hello\n",
            )
            .unwrap();
            let before = harness.controls().write_count();

            harness
                .session
                .handle(message_from("!ls", OWNER, "m2"))
                .await;
            harness
                .session
                .handle(message_from("!cat readme.md", OWNER, "m3"))
                .await;

            let said = harness
                .thread
                .replies()
                .into_iter()
                .map(|(text, _)| text)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(said.contains("readme.md"));
            assert!(said.contains("hello"));
            assert_eq!(harness.controls().write_count(), before);
        })
    })
    .await;
}

/// The same containment the sandbox applies, so nothing else can be read.
#[tokio::test]
async fn a_path_outside_the_project_is_refused() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness
                .session
                .handle(message_from("!cat ../../etc/passwd", OWNER, "m2"))
                .await;

            assert!(
                harness
                    .thread
                    .everything()
                    .contains("is not inside this session's project")
            );
        })
    })
    .await;
}

#[tokio::test]
async fn a_file_can_be_uploaded_from_the_project_on_request() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            std::fs::write(
                harness.root.path().join("project").join("notes.txt"),
                "some notes\n",
            )
            .unwrap();

            harness
                .session
                .handle(message_from("!file notes.txt", OWNER, "m2"))
                .await;

            assert_eq!(harness.thread.uploads()[0].0, "notes.txt");
            assert_eq!(harness.thread.uploads()[0].1, 11);
        })
    })
    .await;
}

#[tokio::test]
async fn stopping_ends_the_session_once_and_tears_the_sandbox_down() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness
                .session
                .handle(message_from("!stop", OWNER, "m2"))
                .await;
            harness.session.stop(EndReason::Stopped).await;

            assert_eq!(*harness.ended.lock().unwrap(), [EndReason::Stopped]);
            assert_eq!(harness.thread.closed(), Some(EndReason::Stopped));
            assert_eq!(*harness.sandbox.stopped.lock().unwrap(), ["s1".to_owned()]);
            assert!(harness.session.is_ended().await);
        })
    })
    .await;
}

#[tokio::test]
async fn a_session_that_hears_nothing_for_long_enough_ends_itself() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness.timers.advance(1_800_001);
            settle().await;

            assert_eq!(*harness.ended.lock().unwrap(), [EndReason::Idle]);
            assert_eq!(*harness.sandbox.stopped.lock().unwrap(), ["s1".to_owned()]);
        })
    })
    .await;
}

/// 137 is how a container killed for passing a limit ends.
#[tokio::test]
async fn an_agent_killed_for_a_resource_limit_names_the_limit() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness.controls().end(137);
            settle().await;

            assert_eq!(*harness.ended.lock().unwrap(), [EndReason::ResourceLimit]);
            assert!(harness.thread.everything().contains("resource limit"));
        })
    })
    .await;
}

#[tokio::test]
async fn an_agent_that_dies_unexpectedly_ends_the_session_as_a_crash() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness.controls().end(1);
            settle().await;

            assert_eq!(*harness.ended.lock().unwrap(), [EndReason::Crashed]);
            assert!(harness.thread.everything().contains("exit code 1"));
        })
    })
    .await;
}

/// A prompt can wait in the queue for as long as the queue allows, and the
/// window can be spent in that time. A slot held by a turn that never ran
/// shrinks the concurrency cap for good.
#[tokio::test]
async fn a_spent_provider_window_refuses_a_prompt_before_it_takes_a_slot() {
    with_session(
        SessionTestCase {
            unavailable: Some(Arc::new(|_provider| {
                Box::pin(async { Some("come back at nine".to_owned()) })
                    as Pin<Box<dyn Future<Output = Option<String>> + Send>>
            })),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .session
                    .handle(message_from("do more work", OWNER, "m2"))
                    .await;

                assert!(harness.thread.everything().contains("come back at nine"));
                assert_eq!(
                    harness.thread.final_reaction("m2"),
                    Some(ReactionOutcome::Failed)
                );
                assert_eq!(harness.scheduler.turns_in_flight(), 0);
            })
        },
    )
    .await;
}

#[tokio::test]
async fn what_is_remembered_is_written_into_the_agents_own_prompt() {
    let memory = Arc::new(MemoryStore::open(":memory:").expect("the store opens"));
    memory.remember_user(OWNER, "amelia", 0).unwrap();
    memory
        .remember(Scope::User, OWNER, "prefers jj over git", "earlier", 0)
        .unwrap();
    memory
        .remember(
            Scope::Project,
            "demo",
            "the build is deno task check",
            "earlier",
            0,
        )
        .unwrap();

    with_session(
        SessionTestCase {
            memory: Some(Arc::clone(&memory)),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                let launched = harness.sandbox.launched.lock().unwrap();
                let path = launched[0]
                    .system_prompt_path
                    .as_ref()
                    .expect("a prompt path");
                let written = std::fs::read_to_string(path).unwrap();

                assert_eq!(
                    path,
                    &std::path::Path::new(&harness.state_dir)
                        .join("memory.md")
                        .display()
                        .to_string()
                );
                assert!(written.contains("You are talking to amelia."));
                assert!(written.contains("prefers jj over git"));
                assert!(written.contains("the build is deno task check"));
            })
        },
    )
    .await;
}

/// Facts are about the person talking, not about whoever opened the thread.
#[tokio::test]
async fn what_the_agent_writes_down_is_remembered_when_the_turn_settles() {
    let memory = Arc::new(MemoryStore::open(":memory:").expect("the store opens"));

    with_session(
        SessionTestCase {
            memory: Some(Arc::clone(&memory)),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                std::fs::write(
                    std::path::Path::new(&harness.state_dir).join("remember.md"),
                    "- likes short commits\n",
                )
                .unwrap();
                std::fs::write(
                    std::path::Path::new(&harness.state_dir).join("project-notes.md"),
                    "- the tests live in src\n",
                )
                .unwrap();

                harness.run_turn().await;

                let facts: Vec<String> = memory
                    .facts_for(Scope::User, OWNER, 100)
                    .unwrap()
                    .into_iter()
                    .map(|fact| fact.fact)
                    .collect();
                assert_eq!(facts, ["likes short commits".to_owned()]);
                let project_facts: Vec<String> = memory
                    .facts_for(Scope::Project, "demo", 100)
                    .unwrap()
                    .into_iter()
                    .map(|fact| fact.fact)
                    .collect();
                assert_eq!(project_facts, ["the tests live in src".to_owned()]);
                // Emptied, so the same line is never ingested twice.
                assert_eq!(
                    std::fs::read_to_string(
                        std::path::Path::new(&harness.state_dir).join("remember.md")
                    )
                    .unwrap(),
                    ""
                );
            })
        },
    )
    .await;
}

#[tokio::test]
async fn a_pull_request_the_agent_asked_for_opens_when_somebody_asked_too() {
    let opened = Arc::new(Mutex::new(Vec::new()));
    with_session(
        SessionTestCase {
            config: Some(config_with(&json!({
                "github": { "token": "ghp", "userName": "errand-bot", "userEmail": "bot@example.com" },
            }))),
            open_pull_request: {
                let opened = Arc::clone(&opened);
                Some(Arc::new(move |request: pr::Request| {
                    let opened = Arc::clone(&opened);
                    Box::pin(async move {
                        opened
                            .lock()
                            .unwrap()
                            .push((request.title.clone(), request.requested_by.clone()));
                        Ok::<String, PullRequestError>("https://github.com/x/y/pull/1".to_owned())
                    })
                        as Pin<Box<dyn Future<Output = Result<String, PullRequestError>> + Send>>
                }) as OpenPullRequest)
            },
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .session
                    .handle(message_from(
                        "open a pull request when you are done",
                        OWNER,
                        "m2",
                    ))
                    .await;
                std::fs::write(
                    std::path::Path::new(&harness.state_dir).join("pull-request.txt"),
                    "Fix the parser\n",
                )
                .unwrap();

                harness.run_turn().await;

                let opened = opened.lock().unwrap();
                assert_eq!(opened[0].0, "Fix the parser");
                assert_eq!(opened[0].1, "amelia");
                assert!(
                    harness
                        .thread
                        .everything()
                        .contains("https://github.com/x/y/pull/1")
                );
                // Cleared, so it is not reopened after every turn that follows.
                assert!(
                    !std::path::Path::new(&harness.state_dir)
                        .join("pull-request.txt")
                        .exists()
                );
            })
        },
    )
    .await;
}

/// An unasked-for pull request spends somebody else's review time.
#[tokio::test]
async fn a_pull_request_nobody_asked_for_is_refused_and_cleared() {
    let opened = Arc::new(Mutex::new(0));
    with_session(
        SessionTestCase {
            config: Some(config_with(&json!({
                "github": { "token": "ghp", "userName": "errand-bot", "userEmail": "bot@example.com" },
            }))),
            open_pull_request: {
                let opened = Arc::clone(&opened);
                Some(Arc::new(move |_request: pr::Request| {
                    let opened = Arc::clone(&opened);
                    Box::pin(async move {
                        *opened.lock().unwrap() += 1;
                        Ok::<String, PullRequestError>("https://github.com/x/y/pull/1".to_owned())
                    })
                        as Pin<Box<dyn Future<Output = Result<String, PullRequestError>> + Send>>
                }) as OpenPullRequest)
            },
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                std::fs::write(
                    std::path::Path::new(&harness.state_dir).join("pull-request.txt"),
                    "Unasked for\n",
                )
                .unwrap();

                harness.run_turn().await;

                assert_eq!(*opened.lock().unwrap(), 0);
                assert!(harness.thread.everything().contains("not by anyone here"));
                assert!(
                    !std::path::Path::new(&harness.state_dir)
                        .join("pull-request.txt")
                        .exists()
                );
            })
        },
    )
    .await;
}

#[tokio::test]
async fn a_session_with_no_github_identity_says_so_rather_than_failing() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness
                .session
                .handle(message_from("!pr Do the thing", OWNER, "m2"))
                .await;

            assert!(
                harness
                    .thread
                    .everything()
                    .contains("no GitHub identity is configured")
            );
            assert_eq!(
                harness.thread.final_reaction("m2"),
                Some(ReactionOutcome::Failed)
            );
        })
    })
    .await;
}

#[tokio::test]
async fn the_git_identity_and_the_gh_wrapper_are_in_place_before_the_launch() {
    with_session(
        SessionTestCase {
            config: Some(config_with(&json!({
                "github": { "token": "ghp", "userName": "errand-bot", "userEmail": "bot@example.com" },
            }))),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                let config = std::fs::read_to_string(
                    std::path::Path::new(&harness.state_dir)
                        .join("home")
                        .join(".gitconfig"),
                )
                .unwrap();
                let shim = std::fs::read_to_string(
                    std::path::Path::new(&harness.state_dir)
                        .join("home")
                        .join("bin")
                        .join("gh"),
                )
                .unwrap();

                assert!(config.contains("errand-bot"));
                assert!(shim.contains("pull requests here are opened by the daemon"));
                let launched = harness.sandbox.launched.lock().unwrap();
                assert_eq!(
                    launched[0].env.get("GIT_AUTHOR_NAME"),
                    Some(&"errand-bot".to_owned())
                );
                assert_eq!(launched[0].env.get("GH_TOKEN"), Some(&"ghp".to_owned()));
            })
        },
    )
    .await;
}

#[tokio::test]
async fn what_a_tool_did_is_reported_and_its_output_truncated() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness.controls().send(&json!({
                "type": "tool_execution_start",
                "toolCallId": "t1",
                "toolName": "bash",
                "args": { "command": "ls -la" },
            }));
            harness.controls().send(&json!({
                "type": "tool_execution_end",
                "toolCallId": "t1",
                "toolName": "bash",
                "result": { "content": [{ "type": "text", "text": "x".repeat(5_000) }] },
            }));
            settle().await;

            assert!(harness.thread.activity().join("\n").contains("`bash`"));
            assert!(harness.thread.results()[0].output.contains("truncated"));
        })
    })
    .await;
}

#[tokio::test]
async fn an_edit_is_shown_as_a_diff_of_what_actually_changed() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            let file = harness.root.path().join("project").join("main.ts");
            std::fs::write(&file, "const x = 1;\n").unwrap();

            harness.controls().send(&json!({
                "type": "tool_execution_start",
                "toolCallId": "t1",
                "toolName": "edit",
                "args": { "path": "/workspace/main.ts" },
            }));
            settle().await;
            std::fs::write(&file, "const x = 2;\n").unwrap();
            harness.controls().send(&json!({
                "type": "tool_execution_end",
                "toolCallId": "t1",
                "toolName": "edit",
                "result": { "content": [{ "type": "text", "text": "written" }] },
            }));
            settle().await;

            assert_eq!(harness.thread.diffs()[0].0, "main.ts");
            assert_eq!(harness.thread.diffs()[0].1, 1);
            assert_eq!(harness.thread.diffs()[0].2, 1);
        })
    })
    .await;
}

#[tokio::test]
async fn a_question_from_the_agent_is_asked_in_the_thread_and_answered_back() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness.controls().send(&json!({
                "type": "extension_ui_request",
                "id": "d1",
                "method": "confirm",
                "title": "Delete the branch?",
            }));
            settle().await;

            harness
                .session
                .handle(message_from("yes", OWNER, "m2"))
                .await;

            assert!(harness.thread.everything().contains("Delete the branch?"));
            assert!(harness.written().contains("extension_ui_response"));
            assert_eq!(
                harness.thread.final_reaction("m2"),
                Some(ReactionOutcome::Accepted)
            );
        })
    })
    .await;
}

#[tokio::test]
async fn interrupting_with_nothing_running_says_so_and_changes_nothing() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness.run_turn().await;

            harness
                .session
                .handle(message_from("!interrupt", OWNER, "m2"))
                .await;

            assert!(
                harness
                    .thread
                    .everything()
                    .contains("nothing running to interrupt")
            );
        })
    })
    .await;
}

#[tokio::test]
async fn compacting_is_refused_while_a_turn_is_running() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness
                .session
                .handle(message_from("!compact", OWNER, "m2"))
                .await;

            assert!(harness.thread.everything().contains("a turn is running"));
            assert_eq!(
                harness.thread.final_reaction("m2"),
                Some(ReactionOutcome::Failed)
            );
        })
    })
    .await;
}

#[tokio::test]
async fn the_status_says_what_the_session_and_the_queue_are_doing() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness
                .session
                .handle(message_from("!status", OWNER, "m2"))
                .await;

            let said = harness
                .thread
                .replies()
                .into_iter()
                .map(|(text, _)| text)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(said.contains("project: demo"));
            assert!(said.contains("running a turn"));
        })
    })
    .await;
}

#[tokio::test]
async fn help_is_listed_without_troubling_the_agent() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            let before = harness.controls().write_count();

            harness
                .session
                .handle(message_from("!help", OWNER, "m2"))
                .await;

            assert!(
                harness
                    .thread
                    .replies()
                    .into_iter()
                    .map(|(text, _)| text)
                    .collect::<Vec<_>>()
                    .join("\n")
                    .contains("!steer")
            );
            assert_eq!(harness.controls().write_count(), before);
        })
    })
    .await;
}

/// Reported once as "exit code 1", for a host that had simply run out of
/// space. That is a thing somebody can fix and a thing they cannot guess.
#[tokio::test]
async fn an_agent_that_died_for_want_of_disk_says_so_not_just_a_code() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness
                .controls()
                .complain("Error: ENOSPC: no space left on device, write");
            settle().await;
            harness.controls().end(1);
            settle().await;

            assert!(
                harness
                    .thread
                    .everything()
                    .contains("run out of disk space")
            );
            assert!(!harness.thread.everything().contains("exit code 1"));
            assert_eq!(*harness.ended.lock().unwrap(), [EndReason::ResourceLimit]);
        })
    })
    .await;
}

#[tokio::test]
async fn an_agent_that_died_for_want_of_memory_says_that_instead() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness
                .controls()
                .complain("FATAL ERROR: Cannot allocate memory");
            settle().await;
            harness.controls().end(1);
            settle().await;

            assert!(harness.thread.everything().contains("run out of memory"));
        })
    })
    .await;
}

/// A number on its own is not something a reader can act on.
#[tokio::test]
async fn an_unexplained_crash_still_carries_the_agents_last_words() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness
                .controls()
                .complain("TypeError: cannot read properties of undefined");
            settle().await;
            harness.controls().end(1);
            settle().await;

            assert!(harness.thread.everything().contains("exit code 1"));
            assert!(harness.thread.everything().contains("TypeError"));
            assert_eq!(*harness.ended.lock().unwrap(), [EndReason::Crashed]);
        })
    })
    .await;
}

/// A thread archived when its session ends drops out of the sidebar, and the
/// people who were in it then have to go hunting for it.
#[tokio::test]
async fn a_session_that_ends_leaves_its_thread_where_people_can_find_it() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness.timers.advance(1_800_001);
            settle().await;

            assert!(harness.session.is_ended().await);
            assert_eq!(harness.thread.closed(), Some(EndReason::Idle));
        })
    })
    .await;
}

/// An explicit stop is somebody saying they are finished with the thread.
#[tokio::test]
async fn stopping_says_so_so_the_thread_can_be_archived() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness
                .session
                .handle(message_from("!stop", OWNER, "m2"))
                .await;

            assert_eq!(harness.thread.closed(), Some(EndReason::Stopped));
        })
    })
    .await;
}

/// A fetcher that answers every url with the same small PNG.
fn png_fetch() -> FetchAttachment {
    Arc::new(|_url: String| Box::pin(async { Ok(PNG.to_vec()) }) as FetchBox)
}

#[tokio::test]
async fn an_attached_file_is_saved_and_the_agent_is_told_where_it_went() {
    with_session(
        SessionTestCase {
            fetch_attachment: Some(png_fetch()),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .session
                    .handle(with_image("what does this say?", "m2"))
                    .await;

                let saved = harness
                    .root
                    .path()
                    .join("project")
                    .join("attachments")
                    .join("screenshot.png");
                assert_eq!(std::fs::read(&saved).unwrap().len(), PNG.len());
                assert!(harness.written().contains("attachments/screenshot.png"));
            })
        },
    )
    .await;
}

/// A message carrying only a file still says something: that a file arrived.
#[tokio::test]
async fn a_message_with_no_text_but_a_file_still_starts_a_turn() {
    with_session(
        SessionTestCase {
            fetch_attachment: Some(png_fetch()),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness.run_turn().await;

                harness.session.handle(with_image("", "m2")).await;
                settle().await;

                assert_eq!(harness.thread.turns(), [1, 2]);
            })
        },
    )
    .await;
}

/// A model chosen for code is often text only. Handing it an image would fail
/// the turn, so one that can see is asked to describe it instead.
#[tokio::test]
async fn an_image_is_described_for_a_model_that_cannot_see_it() {
    with_session(
        SessionTestCase {
            fetch_attachment: Some(png_fetch()),
            describe_images: Some(Arc::new(|_images, question| {
                Box::pin(async move { Ok(format!("described: it says ENOSPC ({question})")) })
            })),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .session
                    .handle(with_image("what does this error say?", "m2"))
                    .await;

                let sent = harness.written();
                assert!(sent.contains("it says ENOSPC"));
                // The image itself is not handed over, which is the whole
                // point.
                assert!(!sent.contains("\"images\""));
            })
        },
    )
    .await;
}

/// Losing the description must not lose the message it came with.
#[tokio::test]
async fn a_description_that_fails_leaves_the_path_and_says_what_went_wrong() {
    with_session(
        SessionTestCase {
            fetch_attachment: Some(png_fetch()),
            describe_images: Some(Arc::new(|_images, _question| {
                Box::pin(async { Err("the describing model refused".to_owned()) })
            })),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .session
                    .handle(with_image("look at this", "m2"))
                    .await;

                assert!(
                    harness
                        .thread
                        .everything()
                        .contains("the describing model refused")
                );
                assert!(harness.written().contains("attachments/screenshot.png"));
            })
        },
    )
    .await;
}

/// With no describer the images go over as they are, which is the normal case.
#[tokio::test]
async fn a_model_that_can_see_is_handed_the_image_itself() {
    with_session(
        SessionTestCase {
            fetch_attachment: Some(png_fetch()),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .session
                    .handle(with_image("what is this?", "m2"))
                    .await;

                assert!(harness.written().contains("\"images\""));
            })
        },
    )
    .await;
}

/// The next message picks the session up and the resumed session says so,
/// which leaves nothing for a notice to add except a line in every thread.
#[tokio::test]
async fn a_session_that_idles_out_says_nothing_about_it() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            let before = harness.thread.notices().len();

            harness.timers.advance(1_800_001);
            settle().await;

            assert_eq!(*harness.ended.lock().unwrap(), [EndReason::Idle]);
            assert_eq!(harness.thread.notices().len(), before);
        })
    })
    .await;
}

/// Somebody asked for this one, so it is answered.
#[tokio::test]
async fn a_session_that_was_stopped_says_so() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness
                .session
                .handle(message_from("!stop", OWNER, "m2"))
                .await;

            let notices = harness.thread.notices();
            assert!(notices.last().unwrap().0.contains("stopped"));
        })
    })
    .await;
}

/// It stopped part way through something, which is worth knowing.
#[tokio::test]
async fn a_crash_says_what_happened() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            harness.controls().end(1);
            settle().await;

            let last = harness.thread.notices().last().unwrap().0.clone();
            assert!(last.contains("exit code 1"));
            assert!(!last.contains("pick it up"));
        })
    })
    .await;
}

/// Posting into a thread opens it again, which is the opposite of what
/// whoever archived it asked for.
#[tokio::test]
async fn a_thread_archived_from_outside_is_not_posted_into() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            let before = harness.thread.notices().len();

            harness.session.stop(EndReason::ThreadArchived).await;

            assert_eq!(*harness.ended.lock().unwrap(), [EndReason::ThreadArchived]);
            assert_eq!(harness.thread.notices().len(), before);
        })
    })
    .await;
}

#[tokio::test]
async fn facts_reads_back_what_the_agent_was_told_about_somebody() {
    let memory = Arc::new(MemoryStore::open(":memory:").expect("the store opens"));
    memory.remember_user(OWNER, "amelia", 0).unwrap();
    memory
        .remember(Scope::User, OWNER, "prefers jj over git", "earlier", 0)
        .unwrap();
    memory
        .remember(
            Scope::Project,
            "demo",
            "the build is deno task check",
            "earlier",
            0,
        )
        .unwrap();

    with_session(
        SessionTestCase {
            memory: Some(Arc::clone(&memory)),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                // No argument means whoever asked, which is what "about me"
                // means.
                harness
                    .session
                    .handle(message_from("!facts", OWNER, "m2"))
                    .await;
                assert!(harness.thread.everything().contains("prefers jj over git"));

                harness
                    .session
                    .handle(message_from("!facts project", OWNER, "m3"))
                    .await;
                assert!(
                    harness
                        .thread
                        .everything()
                        .contains("the build is deno task check")
                );

                // A subject nothing is held for says so rather than staying
                // silent.
                harness
                    .session
                    .handle(message_from(&format!("!facts <@{GUEST}>"), OWNER, "m4"))
                    .await;
                assert!(
                    harness
                        .thread
                        .everything()
                        .contains("nothing is remembered about")
                );
            })
        },
    )
    .await;
}

#[tokio::test]
async fn forget_drops_what_is_remembered_and_says_how_much_went() {
    let memory = Arc::new(MemoryStore::open(":memory:").expect("the store opens"));
    memory.remember_user(OWNER, "amelia", 0).unwrap();
    memory
        .remember(Scope::User, OWNER, "prefers jj over git", "earlier", 0)
        .unwrap();

    with_session(
        SessionTestCase {
            memory: Some(Arc::clone(&memory)),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .session
                    .handle(message_from("!forget", OWNER, "m2"))
                    .await;
                // Forgetting changes every later session, so it is never
                // guessed at.
                assert!(harness.thread.everything().contains("say who"));
                assert_eq!(memory.facts_for(Scope::User, OWNER, 100).unwrap().len(), 1);

                harness
                    .session
                    .handle(message_from(&format!("!forget <@{OWNER}>"), OWNER, "m3"))
                    .await;
                assert!(harness.thread.everything().contains("forgot 1 fact about"));
                assert_eq!(memory.facts_for(Scope::User, OWNER, 100).unwrap().len(), 0);
            })
        },
    )
    .await;
}

/// Forgetting is the owner's: it outlives this thread.
#[tokio::test]
async fn a_guest_may_read_facts_but_may_not_forget_them() {
    let memory = Arc::new(MemoryStore::open(":memory:").expect("the store opens"));
    memory
        .remember(Scope::User, GUEST, "works on the packaging", "earlier", 0)
        .unwrap();

    with_session(
        SessionTestCase {
            memory: Some(Arc::clone(&memory)),
            ..Default::default()
        },
        |harness| {
            Box::pin(async move {
                harness
                    .session
                    .handle(message_from(&format!("!allow <@{GUEST}>"), OWNER, "m2"))
                    .await;

                harness
                    .session
                    .handle(message_from("!facts", GUEST, "m3"))
                    .await;
                assert!(
                    harness
                        .thread
                        .everything()
                        .contains("works on the packaging")
                );

                harness
                    .session
                    .handle(message_from(&format!("!forget <@{GUEST}>"), GUEST, "m4"))
                    .await;
                assert_eq!(memory.facts_for(Scope::User, GUEST, 100).unwrap().len(), 1);
            })
        },
    )
    .await;
}

/// Hurrying an interruption along used to end the session: each ask started
/// its own wait, and the first to run out force stopped it.
#[tokio::test]
async fn asking_to_interrupt_again_does_not_start_a_second_wait() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            // A turn that starts and does not settle, so the session
            // stays busy.
            harness.controls().send(&json!({ "type": "agent_start" }));
            settle().await;

            harness
                .session
                .handle(message_from("!interrupt", OWNER, "m2"))
                .await;

            // These return at once, because one interruption is already
            // in flight.
            harness
                .session
                .handle(message_from("!interrupt", OWNER, "m3"))
                .await;
            harness
                .session
                .handle(message_from("!interrupt", OWNER, "m4"))
                .await;
            settle().await;
            assert!(harness.thread.everything().contains("already interrupting"));

            // Past the deadline the single wait gives up and force stops
            // once, rather than once per ask.
            harness.timers.advance(20_000);
            settle().await;

            // One marker per force stop: the phrase itself appears both
            // in what is said and in the reason the session ends with.
            let forced = harness
                .thread
                .everything()
                .matches("did not confirm the interruption")
                .count();
            assert_eq!(forced, 1, "gave up {forced} times, not once");
        })
    })
    .await;
}

#[tokio::test]
async fn an_interruption_the_agent_confirms_does_not_force_stop() {
    with_session(
        SessionTestCase::default(),
        |harness| {
            Box::pin(async move {
                harness.controls().send(&json!({ "type": "agent_start" }));
                settle().await;

                harness
                    .session
                    .handle(message_from("!interrupt", OWNER, "m2"))
                    .await;

                // The agent answers, so the turn settles well inside the
                // deadline.
                harness.controls().send(&json!({
                    "type": "turn_end",
                    "usage": { "input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2, "cost": 0 },
                }));
                harness.controls().send(&json!({ "type": "agent_settled" }));
                settle().await;
                harness.timers.advance(200);
                settle().await;

                assert!(!harness.thread.everything().contains("force stopped"));
            })
        },
    )
    .await;
}

/// What a session's record holds, as the daemon writes it.
fn seed_withdrawn_record(harness: &Harness, said: &str) {
    let record = record_dir(&harness.state_dir);
    std::fs::create_dir_all(&record).expect("the record directory is made");
    std::fs::write(
        std::path::Path::new(&record).join(TRANSCRIPT_NAME),
        [
            json!({
                "at": 1,
                "entry": { "call": "prompt", "author": "amelia", "text": said, "id": "m2" },
            })
            .to_string(),
            json!({
                "at": 2,
                "entry": { "call": "post", "text": "sure, noted" },
            })
            .to_string(),
        ]
        .join("\n"),
    )
    .expect("the transcript is written");
}

/// The transcript file name the record module prefers.
const TRANSCRIPT_NAME: &str = "transcript.jsonl";

/// A live session is told, in words that are not a new instruction.
#[tokio::test]
async fn a_live_session_is_told_of_a_withdrawal() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            let said = "my token is ghp_TOPSECRET123";
            seed_withdrawn_record(harness, said);

            // The opening turn is still running; let it settle, the way
            // the agent finishing would, so the withdrawal is applied
            // rather than held.
            harness.run_turn().await;

            assert!(harness.session.withdraw("m2".to_owned()).await);
            settle().await;

            // The agent is told, not corrected; the notice says what
            // happened rather than instructing it anew.
            assert!(
                harness
                    .written()
                    .contains("has been withdrawn by the person who sent it")
            );
            // The record keeps the turn and loses the words.
            let record = record_dir(&harness.state_dir);
            let transcript =
                std::fs::read_to_string(std::path::Path::new(&record).join(TRANSCRIPT_NAME))
                    .unwrap();
            assert!(!transcript.contains("ghp_TOPSECRET123"));
            assert!(transcript.contains(r#""withdrawn":true"#));
            // What came after it is left alone.
            assert!(transcript.contains("sure, noted"));
        })
    })
    .await;
}

/// A turn in progress holds the reconciliation until it settles.
#[tokio::test]
async fn a_turn_in_progress_survives_a_withdrawal() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            let said = "take this back";
            seed_withdrawn_record(harness, said);

            // A turn starts and does not settle, so the session is busy.
            harness.controls().send(&json!({ "type": "agent_start" }));
            settle().await;

            // Held, because the agent is appending to its conversation.
            assert!(harness.session.withdraw("m2".to_owned()).await);
            assert!(
                !harness
                    .written()
                    .contains("has been withdrawn by the person")
            );

            // The turn finishes normally, and what it held back is
            // applied after it.
            harness.run_turn().await;
            assert!(
                harness
                    .written()
                    .contains("has been withdrawn by the person who sent it")
            );
            assert_eq!(
                harness.thread.final_reaction("m1"),
                Some(ReactionOutcome::Succeeded)
            );
        })
    })
    .await;
}

/// The run continues when one copy cannot be reached, and the reconciliation
/// does not end the session.
#[tokio::test]
async fn an_unreachable_copy_does_not_cost_the_session() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            let said = "words worth taking back";
            seed_withdrawn_record(harness, said);

            // Let the opening turn settle, so the withdrawal is applied
            // at once rather than held by a turn in progress.
            harness.run_turn().await;

            // No stored agent conversation exists at all, which is the
            // ordinary shape of a session that never settled a turn.
            assert!(harness.session.withdraw("m2".to_owned()).await);
            settle().await;

            // The session carries on regardless.
            assert!(!harness.session.is_ended().await);
            assert!(harness.written().contains("has been withdrawn"));
        })
    })
    .await;
}

/// The daemon reads a session's project from outside the sandbox, so a link
/// planted inside it must not be a way to reach the host's own files. The
/// spelling check alone cannot see a link, and between the check and the open
/// a plain file can become one.
#[tokio::test]
async fn a_link_planted_in_the_project_reads_nothing_through_cat() {
    let outside = tempfile::tempdir().expect("a temporary directory");
    let secret = outside.path().join("id_ed25519");
    std::fs::write(&secret, "the host's own key").expect("written");

    with_session(SessionTestCase::default(), |harness| {
        let secret = secret.clone();
        Box::pin(async move {
            let project = harness.session.project().path.clone();
            std::fs::write(std::path::Path::new(&project).join("ordinary"), "mine")
                .expect("written");
            std::os::unix::fs::symlink(&secret, std::path::Path::new(&project).join("escape"))
                .expect("linked");

            harness
                .session
                .handle(message_from("!cat ordinary", OWNER, "m2"))
                .await;
            assert!(
                harness.thread.everything().contains("mine"),
                "an ordinary file still reads"
            );

            harness
                .session
                .handle(message_from("!cat escape", OWNER, "m3"))
                .await;
            let said = harness.thread.everything();
            assert!(
                !said.contains("the host's own key"),
                "the link must not read the host's file: {said}"
            );
        })
    })
    .await;
}

/// A message from somebody the owner never invited, carrying a file.
fn from_stranger_with_a_file(content: &str, id: &str) -> IncomingMessage {
    let mut sent = message_from(content, STRANGER, id);
    sent.attachments = vec![RawAttachment {
        id: "a1".to_owned(),
        name: "screenshot.png".to_owned(),
        url: "https://files.example/screenshot.png".to_owned(),
        size: PNG.len() as u64,
        content_type: Some("image/png".to_owned()),
    }];
    sent
}

/// Answering a question the agent is blocked on decides what it does next,
/// and fetching an attachment writes bytes into the project. Both are taking
/// part in somebody else's session.
#[tokio::test]
async fn a_stranger_answers_no_dialog_and_leaves_no_file_behind() {
    with_session(SessionTestCase::default(), |harness| {
        Box::pin(async move {
            let project = harness.session.project().path.clone();
            harness.controls().send(&json!({
                "type": "extension_ui_request",
                "id": "d1",
                "method": "select",
                "title": "which one?",
                "options": ["a", "b"],
            }));
            settle().await;
            assert!(
                harness.thread.everything().contains("which one?"),
                "the agent is waiting on a question"
            );

            harness
                .session
                .handle(from_stranger_with_a_file("a", "m9"))
                .await;

            let said = harness.thread.everything();
            assert!(said.contains("has not invited you"), "{said}");
            // Nothing was fetched into the project on their say-so.
            let attachments = std::path::Path::new(&project).join("attachments");
            assert!(
                !attachments.exists()
                    || std::fs::read_dir(&attachments)
                        .into_iter()
                        .flatten()
                        .count()
                        == 0,
                "a stranger's file reached the project"
            );
            // The agent is still waiting, so the dialog was not answered.
            assert!(
                !harness
                    .controls()
                    .written()
                    .iter()
                    .any(|line| line.contains("\"d1\"")),
                "a stranger answered the dialog"
            );
        })
    })
    .await;
}

/// A model id is unique only within its provider. Sending one to whatever
/// provider the session happens to be on is why switching away and back used
/// to fail: the second switch named a model the new provider never had.
#[test]
fn a_model_carries_the_provider_that_serves_it() {
    let available = vec![
        AvailableModel {
            provider: "zai".to_owned(),
            id: "glm-5.3".to_owned(),
        },
        AvailableModel {
            provider: "muse".to_owned(),
            id: "musecringe".to_owned(),
        },
    ];

    // Away from the configured provider.
    let away = super::answering::choose(&available, "musecringe", "zai");
    assert!(matches!(&away, super::answering::Chosen::One(m) if m.provider == "muse"));

    // And back again, from the provider we switched to.
    let back = super::answering::choose(&available, "glm-5.3", "muse");
    assert!(
        matches!(&back, super::answering::Chosen::One(m) if m.provider == "zai" && m.id == "glm-5.3"),
        "switching back must go to the provider that serves it"
    );
}

/// A name several providers serve is refused with the qualified options
/// rather than sent to whichever was looked at first.
#[test]
fn a_name_two_providers_serve_is_not_guessed_at() {
    let available = vec![
        AvailableModel {
            provider: "alpha".to_owned(),
            id: "shared".to_owned(),
        },
        AvailableModel {
            provider: "beta".to_owned(),
            id: "shared".to_owned(),
        },
    ];

    match super::answering::choose(&available, "shared", "gamma") {
        super::answering::Chosen::Several(options) => {
            assert_eq!(options, ["alpha/shared", "beta/shared"]);
        }
        other => panic!(
            "expected the ambiguity to be reported, got {:?}",
            matches!(other, super::answering::Chosen::None)
        ),
    }

    // Naming the provider settles it.
    assert!(matches!(
        super::answering::choose(&available, "beta/shared", "gamma"),
        super::answering::Chosen::One(m) if m.provider == "beta"
    ));

    // And the session's own provider wins a bare name without asking.
    assert!(matches!(
        super::answering::choose(&available, "shared", "beta"),
        super::answering::Chosen::One(m) if m.provider == "beta"
    ));
}

/// The listing is grouped, and the session's own provider comes first so the
/// models it can switch to without qualifying are the ones at the top.
#[test]
fn the_listing_puts_this_sessions_provider_first() {
    let available = vec![
        AvailableModel {
            provider: "openrouter".to_owned(),
            id: "a".to_owned(),
        },
        AvailableModel {
            provider: "zai".to_owned(),
            id: "glm".to_owned(),
        },
    ];

    let lines = super::answering::grouped_by_provider(&available, "zai");

    assert_eq!(lines, ["  zai", "    glm", "  openrouter", "    a"]);
}
