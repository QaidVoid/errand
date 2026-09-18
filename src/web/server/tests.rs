//! Tests for the interface server, ported from `server_test.ts`.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::watch;

use super::{Assets, WebServer};
use crate::admission::scheduler::{Clock, Scheduler, Timer};
use crate::agent::client::AgentProcess;
use crate::config::schema::SandboxBackend;
use crate::config::schema::WebConfig;
use crate::config::validate::validate_config;
use crate::log::{LogFields, Logger};
use crate::sandbox::backend::{
    CapabilityReport, SandboxLaunch, SandboxLaunchError, SandboxUnavailableError,
};
use crate::sandbox::paths;
use crate::session::event::EndReason;
use crate::session::event::SessionEvent;
use crate::session::manager::{
    CreatedThread, FoundView, MadeThread, ManagerOptions, SandboxPool, SessionManager,
    ThreadFactory,
};
use crate::session::registry::ThreadRegistry;
use crate::session::session::{IncomingMessage, RunningBox};
use crate::session::views::{SessionView, ViewError};

const OWNER: &str = "100000000000000001";

fn silent() -> Logger {
    Logger::new(LogFields::new(), Arc::new(|_level, _line| {}))
}

/// An agent that answers its readiness call and then says nothing.
struct QuietAgent {
    queue: Mutex<std::collections::VecDeque<u8>>,
    closed: Mutex<bool>,
    exit: watch::Receiver<Option<i32>>,
}

#[derive(Clone)]
struct QuietControls {
    fake: Arc<QuietAgent>,
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
        let controls = QuietControls {
            fake: Arc::clone(&fake),
            sender: Arc::new(Mutex::new(Some(exit_sender))),
        };
        (fake, controls)
    }
}

impl QuietControls {
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
}

impl AgentProcess for QuietAgent {
    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let line = String::from_utf8_lossy(bytes).trim().to_owned();
        let id = serde_json::from_str::<Value>(&line)
            .ok()
            .and_then(|parsed| parsed.get("id").cloned());
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

struct FakeSandbox;

impl SandboxPool for FakeSandbox {
    fn probe(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<CapabilityReport, SandboxUnavailableError>> + Send + '_>>
    {
        Box::pin(async move {
            Ok(CapabilityReport {
                backend: SandboxBackend::Bailey,
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
            let (_agent, controls) = QuietAgent::new();
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
        Box::pin(async { Vec::new() })
    }

    fn remove_orphans<'a>(
        &'a self,
        names: &'a [String],
    ) -> Pin<Box<dyn Future<Output = usize> + Send + 'a>> {
        Box::pin(async move { names.len() })
    }
}

struct QuietThreadView;

impl SessionView for QuietThreadView {
    fn observe<'a>(
        &'a self,
        _event: &'a SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), ViewError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

struct FakeThreads {
    next: Mutex<u32>,
}

impl ThreadFactory for FakeThreads {
    fn create(self: Arc<Self>, _message: IncomingMessage, name: String) -> MadeThread {
        let _ = name;
        Box::pin(async move {
            let id = {
                let mut next = self.next.lock().unwrap();
                let id = format!("thread-{next}");
                *next += 1;
                id
            };
            Ok(CreatedThread {
                id,
                view: Arc::new(QuietThreadView),
            })
        })
    }

    fn open(self: Arc<Self>, name: String, _opener: String) -> MadeThread {
        Box::pin(async move {
            let id = {
                let mut next = self.next.lock().unwrap();
                let id = format!("thread-{next}");
                *next += 1;
                id
            };
            let _ = name;
            Ok(CreatedThread {
                id,
                view: Arc::new(QuietThreadView),
            })
        })
    }

    fn port_for(self: Arc<Self>, thread_id: String) -> FoundView {
        Box::pin(async move {
            thread_id
                .starts_with("thread-")
                .then(|| Arc::new(QuietThreadView) as Arc<dyn SessionView>)
        })
    }

    fn release(&self, _thread_id: &str) {}
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

struct Harness {
    server: Arc<WebServer>,
    manager: Arc<SessionManager>,
    root: tempfile::TempDir,
    base: String,
}

/// Stands in for the built interface, with the two files a test asks for.
static BUILT_FOR_TESTS: Assets = &[
    ("app.js", b"export const ready = true;\n"),
    ("index.html", b"<!doctype html><title>errand</title>"),
];

async fn with_server_where(
    observer: bool,
    assets: bool,
    run: impl FnOnce(&Harness) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>,
) {
    let root = tempfile::tempdir().expect("a temp directory");
    // The interface a test serves, so what is asserted does not depend on
    // whether the real bundle happened to be built on this machine.
    let bundle: Assets = if assets { BUILT_FOR_TESTS } else { &[] };

    // A port nothing else is likely to be on, reserved and then released.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("a probe port");
    let port = probe.local_addr().expect("the probe address").port();
    drop(probe);

    let settings = validate_config(&json!({
        "chat": {
            "token": "a.token.value",
            "channelId": "chan",
            "allowedUserIds": [OWNER],
        },
        "agent": {
            "provider": "anthropic",
            "providers": {
                "anthropic": { "credentialName": "ANTHROPIC_API_KEY", "credential": "secret" },
            },
        },
        "projectRoot": root.path().join("projects").display().to_string(),
        "stateDir": root.path().join("state").display().to_string(),
        "web": {
            "host": "127.0.0.1",
            "port": port,
            "observer": observer,
        },
    }))
    .expect("the test configuration is accepted");
    let web_config = settings.web.clone().expect("the web settings");

    let registry = Arc::new(Mutex::new(ThreadRegistry::new(
        ThreadRegistry::path_for(root.path().join("state").to_str().expect("a state path")),
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

    let manager = Arc::new(SessionManager::new(ManagerOptions {
        config: settings,
        sandbox: Arc::new(FakeSandbox) as Arc<dyn SandboxPool>,
        scheduler: Arc::clone(&scheduler),
        threads: Arc::new(FakeThreads {
            next: Mutex::new(1),
        }) as Arc<dyn ThreadFactory>,
        registry,
        log: silent(),
        make_id: Some(Arc::new(move || {
            let mut id = id.lock().unwrap();
            *id += 1;
            format!("s{id}")
        })),
        unavailable: None,
        operator_ids: None,
        memory: None,
        describe_images: None,
        public_url: None,
        available_models: Vec::new(),
        delegate_base_url: None,
        now: Some(Arc::new(|| 1_000)),
    }));

    let server = WebServer::new(
        web_config,
        Arc::clone(&manager),
        bundle,
        silent(),
        Some("guild-1".to_owned()),
        Some(Arc::new(|id: &str| {
            (id == OWNER).then(|| "amelia".to_owned())
        })),
    );
    if assets {
        server.start().await.expect("the interface starts");
    }
    let base = match server.bound_port() {
        Some(port) => format!("http://127.0.0.1:{port}"),
        None => String::new(),
    };

    let harness = Harness {
        server,
        manager,
        root,
        base,
    };
    run(&harness).await;

    harness.server.stop();
    harness.manager.shutdown().await;
}

async fn message(client: &reqwest::Client, base: &str, path: &str) -> reqwest::Response {
    client
        .get(format!("{base}{path}"))
        .send()
        .await
        .expect("the request answers")
}

async fn post(client: &reqwest::Client, base: &str, path: &str, body: &Value) -> reqwest::Response {
    client
        .post(format!("{base}{path}"))
        .json(body)
        .send()
        .await
        .expect("the request answers")
}

async fn started_session(harness: &Harness, _client: &reqwest::Client) {
    let outcome = harness
        .manager
        .start(IncomingMessage {
            id: "m1".to_owned(),
            author_id: OWNER.to_owned(),
            author_name: Some("amelia".to_owned()),
            content: "demo: go".to_owned(),
            attachments: Vec::new(),
        })
        .await;
    assert!(outcome.is_started(), "the session starts");
}

#[tokio::test]
async fn the_interface_says_what_it_will_and_will_not_allow() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            let client = reqwest::Client::new();
            let answer = message(&client, &harness.base, "/api/interface").await;

            assert_eq!(
                answer.json::<Value>().await.unwrap(),
                json!({ "observer": false, "guildId": "guild-1" })
            );
        })
    })
    .await;
}

#[tokio::test]
async fn a_session_that_is_running_is_listed_with_what_it_was_asked() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            started_session(harness, &reqwest::Client::new()).await;
            let client = reqwest::Client::new();

            let listed = message(&client, &harness.base, "/api/sessions")
                .await
                .json::<Value>()
                .await
                .unwrap();

            assert_eq!(listed.as_array().map(Vec::len), Some(1));
            assert_eq!(listed[0]["id"], "demo-s1");
            assert_eq!(listed[0]["project"], "demo");
            assert_eq!(listed[0]["live"], true);
            assert_eq!(listed[0]["opening"], "go");
            assert_eq!(listed[0]["threadId"], "thread-1");
        })
    })
    .await;
}

/// A session that stopped is not finished: sending to it picks it back up.
#[tokio::test]
async fn a_session_whose_sandbox_has_gone_is_still_listed() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            started_session(harness, &reqwest::Client::new()).await;
            harness
                .manager
                .end_thread("thread-1", EndReason::Idle)
                .await;
            let client = reqwest::Client::new();

            let listed = message(&client, &harness.base, "/api/sessions")
                .await
                .json::<Value>()
                .await
                .unwrap();

            assert_eq!(listed.as_array().map(Vec::len), Some(1));
            assert_eq!(listed[0]["live"], false);
            assert_eq!(listed[0]["id"], "demo-s1");
        })
    })
    .await;
}

#[tokio::test]
async fn a_session_can_be_started_from_the_interface() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            let client = reqwest::Client::new();
            let answer = post(
                &client,
                &harness.base,
                "/api/sessions",
                &json!({ "project": "demo", "prompt": "fix the parser" }),
            )
            .await;

            assert_eq!(answer.status(), 200);
            assert_eq!(answer.json::<Value>().await.unwrap()["id"], "demo-s1");
            assert_eq!(harness.manager.sessions().len(), 1);
        })
    })
    .await;
}

#[tokio::test]
async fn a_session_with_nothing_asked_of_it_is_refused() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            let client = reqwest::Client::new();
            let answer = post(
                &client,
                &harness.base,
                "/api/sessions",
                &json!({ "project": "demo", "prompt": "  " }),
            )
            .await;

            assert_eq!(answer.status(), 400);
            assert!(
                answer.json::<Value>().await.unwrap()["error"]
                    .as_str()
                    .unwrap()
                    .contains("a prompt is required")
            );
        })
    })
    .await;
}

#[tokio::test]
async fn a_message_reaches_the_session_it_names() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            started_session(harness, &reqwest::Client::new()).await;
            let client = reqwest::Client::new();

            let answer = post(
                &client,
                &harness.base,
                "/api/sessions/demo-s1/send",
                &json!({ "text": "carry on" }),
            )
            .await;

            assert_eq!(answer.status(), 200);
            assert_eq!(answer.json::<Value>().await.unwrap()["accepted"], true);
        })
    })
    .await;
}

#[tokio::test]
async fn a_message_to_a_session_that_never_existed_says_so() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            let client = reqwest::Client::new();
            let answer = post(
                &client,
                &harness.base,
                "/api/sessions/nobody/send",
                &json!({ "text": "hello" }),
            )
            .await;

            assert_eq!(answer.status(), 404);
        })
    })
    .await;
}

/// An observer that drew a composer would offer what it always refuses.
#[tokio::test]
async fn an_observing_interface_changes_nothing_and_says_why() {
    with_server_where(true, true, |harness| {
        Box::pin(async move {
            let client = reqwest::Client::new();
            let describe = message(&client, &harness.base, "/api/interface").await;
            assert_eq!(describe.json::<Value>().await.unwrap()["observer"], true);

            let started = post(
                &client,
                &harness.base,
                "/api/sessions",
                &json!({ "project": "demo", "prompt": "go" }),
            )
            .await;
            let sent = post(
                &client,
                &harness.base,
                "/api/sessions/demo-s1/send",
                &json!({ "text": "hello" }),
            )
            .await;

            assert_eq!(started.status(), 403);
            assert_eq!(sent.status(), 403);
            assert!(
                sent.json::<Value>().await.unwrap()["error"]
                    .as_str()
                    .unwrap()
                    .contains("cannot change anything")
            );
        })
    })
    .await;
}

#[tokio::test]
async fn a_live_session_is_streamed_starting_with_a_reset() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            started_session(harness, &reqwest::Client::new()).await;
            let client = reqwest::Client::new();

            let mut answer = message(&client, &harness.base, "/api/sessions/demo-s1/stream").await;
            assert_eq!(
                answer.headers().get("content-type").unwrap(),
                "text/event-stream"
            );

            let chunk = answer.chunk().await.unwrap().expect("a first chunk");
            assert!(String::from_utf8_lossy(&chunk).contains("event: reset"));
        })
    })
    .await;
}

#[tokio::test]
async fn a_stream_for_a_session_that_is_not_live_is_refused() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            let client = reqwest::Client::new();
            let answer = message(&client, &harness.base, "/api/sessions/nobody/stream").await;

            assert_eq!(answer.status(), 404);
        })
    })
    .await;
}

/// A stopped session has no stream, so its history is read back from disk.
#[tokio::test]
async fn a_stopped_sessions_transcript_is_read_from_where_it_was_written() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            started_session(harness, &reqwest::Client::new()).await;
            harness
                .manager
                .end_thread("thread-1", EndReason::Idle)
                .await;
            let client = reqwest::Client::new();

            let answer = message(&client, &harness.base, "/api/sessions/demo-s1/transcript")
                .await
                .json::<Value>()
                .await
                .unwrap();

            assert!(answer["entries"].is_array());
            assert_eq!(answer["grouped"], true);
            assert!(
                answer["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["kind"] == "prompt")
            );
            assert_eq!(answer["dropped"], 0);
        })
    })
    .await;
}

#[tokio::test]
async fn a_transcript_for_a_session_nobody_has_heard_of_is_not_found() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            let client = reqwest::Client::new();
            let answer = message(&client, &harness.base, "/api/sessions/nobody/transcript").await;

            assert_eq!(answer.status(), 404);
        })
    })
    .await;
}

#[tokio::test]
async fn a_sessions_project_can_be_listed_and_read() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            started_session(harness, &reqwest::Client::new()).await;
            std::fs::write(
                harness
                    .root
                    .path()
                    .join("projects")
                    .join("demo")
                    .join("readme.md"),
                "hello\n",
            )
            .expect("the file");
            let client = reqwest::Client::new();

            let tree = message(&client, &harness.base, "/api/sessions/demo-s1/tree")
                .await
                .json::<Value>()
                .await
                .unwrap();
            let file = message(
                &client,
                &harness.base,
                "/api/sessions/demo-s1/file?path=readme.md",
            )
            .await
            .json::<Value>()
            .await
            .unwrap();

            assert!(
                tree.as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["name"] == "readme.md")
            );
            assert_eq!(file["text"], "hello\n");
            assert_eq!(file["language"], "md");
        })
    })
    .await;
}

/// The same containment the sandbox applies, not a second looser one.
#[tokio::test]
async fn a_path_outside_the_project_is_refused() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            started_session(harness, &reqwest::Client::new()).await;
            let client = reqwest::Client::new();

            let answer = message(
                &client,
                &harness.base,
                "/api/sessions/demo-s1/file?path=../../etc/passwd",
            )
            .await;

            assert_eq!(answer.status(), 403);
            assert!(
                answer.json::<Value>().await.unwrap()["error"]
                    .as_str()
                    .unwrap()
                    .contains("outside this session's project")
            );
        })
    })
    .await;
}

#[tokio::test]
async fn a_file_can_be_downloaded_as_itself() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            started_session(harness, &reqwest::Client::new()).await;
            std::fs::write(
                harness
                    .root
                    .path()
                    .join("projects")
                    .join("demo")
                    .join("notes.txt"),
                "some notes\n",
            )
            .expect("the file");
            let client = reqwest::Client::new();

            let answer = message(
                &client,
                &harness.base,
                "/api/sessions/demo-s1/download?path=notes.txt",
            )
            .await;

            assert!(
                answer
                    .headers()
                    .get("content-disposition")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains("filename=\"notes.txt\"")
            );
            assert_eq!(answer.text().await.unwrap(), "some notes\n");
        })
    })
    .await;
}

#[tokio::test]
async fn the_built_interface_is_served_and_a_deep_path_lands_on_it() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            let client = reqwest::Client::new();
            let index = message(&client, &harness.base, "/").await;
            let asset = message(&client, &harness.base, "/app.js").await;
            let deep = message(&client, &harness.base, "/session/s1/whatever").await;

            assert!(
                index
                    .text()
                    .await
                    .unwrap()
                    .contains("<title>errand</title>")
            );
            assert!(
                asset
                    .headers()
                    .get("content-type")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains("text/javascript")
            );
            assert!(deep.text().await.unwrap().contains("<title>errand</title>"));
        })
    })
    .await;
}

/// A traversal in a request must not reach outside the built assets.
#[tokio::test]
async fn an_asset_path_that_climbs_out_is_not_served() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            std::fs::write(harness.root.path().join("secret.txt"), "not for you")
                .expect("the file");
            let client = reqwest::Client::new();

            let answer = message(&client, &harness.base, "/../secret.txt").await;

            assert!(!answer.text().await.unwrap().contains("not for you"));
        })
    })
    .await;
}

#[tokio::test]
async fn an_unknown_api_route_is_not_the_interface() {
    with_server_where(false, true, |harness| {
        Box::pin(async move {
            let client = reqwest::Client::new();
            let answer = message(&client, &harness.base, "/api/nothing-here").await;

            assert_eq!(answer.status(), 404);
            assert!(
                answer.json::<Value>().await.unwrap()["error"]
                    .as_str()
                    .unwrap()
                    .contains("no such route")
            );
        })
    })
    .await;
}

/// Serving something broken is worse than saying it is not there.
#[tokio::test]
async fn an_interface_that_was_never_built_refuses_to_serve() {
    with_server_where(false, false, |harness| {
        Box::pin(async move {
            let error = harness.server.start().await.unwrap_err();
            assert!(error.to_string().contains("carries no interface"));
        })
    })
    .await;
}

/// A warning in a log is not a control, so a public bind is a refusal.
#[tokio::test]
async fn an_interface_asked_to_bind_publicly_refuses_to_start() {
    let root = tempfile::tempdir().expect("a temp directory");
    std::fs::write(root.path().join("index.html"), "<!doctype html>").expect("the file");
    let settings = validate_config(&json!({
        "chat": {
            "token": "a.token.value",
            "channelId": "chan",
            "allowedUserIds": [OWNER],
        },
        "agent": {
            "provider": "anthropic",
            "providers": {
                "anthropic": { "credentialName": "ANTHROPIC_API_KEY", "credential": "secret" },
            },
        },
        "projectRoot": root.path().join("projects").display().to_string(),
        "stateDir": root.path().join("state").display().to_string(),
    }))
    .expect("the test configuration is accepted");
    let registry = Arc::new(Mutex::new(ThreadRegistry::new(
        ThreadRegistry::path_for(root.path().join("state").to_str().expect("a state path")),
        silent(),
    )));
    let scheduler = Scheduler::start(
        settings.limits.clone(),
        Arc::new(NoClock),
        5_000,
        300_000,
        750,
    );
    let manager = Arc::new(SessionManager::new(ManagerOptions {
        config: settings,
        sandbox: Arc::new(FakeSandbox) as Arc<dyn SandboxPool>,
        scheduler: Arc::clone(&scheduler),
        threads: Arc::new(FakeThreads {
            next: Mutex::new(1),
        }) as Arc<dyn ThreadFactory>,
        registry,
        log: silent(),
        make_id: None,
        unavailable: None,
        operator_ids: None,
        memory: None,
        describe_images: None,
        public_url: None,
        available_models: Vec::new(),
        delegate_base_url: None,
        now: None,
    }));

    let server = WebServer::new(
        WebConfig {
            host: "0.0.0.0".to_owned(),
            port: 39_999,
            observer: false,
            public_url: None,
        },
        manager,
        &[],
        silent(),
        None,
        None,
    );

    let error = server.start().await.unwrap_err();
    assert!(error.to_string().contains("every interface"));
}
