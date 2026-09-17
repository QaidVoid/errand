use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{Delegating, Reported, labelled};
use crate::admission::scheduler::{Clock, Scheduler, Timer};
use crate::agent::delegate::TurnDelegations;
use crate::agent::delegation::Sources;
use crate::config::schema::LimitsConfig;
use crate::log::Logger;
use crate::provider::ask::{Endpoint, SendRequest, Sender};

fn silent() -> Logger {
    Logger::new(Default::default(), Arc::new(|_level, _line| {}))
}

fn endpoint() -> Endpoint {
    Endpoint {
        base_url: "https://api.example/v1".to_owned(),
        model: "flash".to_owned(),
        credential: "k".to_owned(),
    }
}

struct DiskSources {
    root: String,
}

impl Sources for DiskSources {
    fn project_root(&self) -> &str {
        &self.root
    }

    async fn read_file(&self, path: &str) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn output_of(&self, id: &str) -> Option<String> {
        (id == "t1").then(|| "ENOSPC: no space left".to_owned())
    }

    fn attachment(&self, _name: &str) -> Option<String> {
        None
    }
}

/// A model that answers whatever it is asked, and records the request.
struct FakeSender {
    answer: Value,
    sent: Mutex<Vec<(String, Value)>>,
}

impl Sender for FakeSender {
    fn send(
        &self,
        url: String,
        request: SendRequest,
    ) -> impl Future<Output = Result<(u16, Option<Value>), String>> + Send {
        async move {
            self.sent
                .lock()
                .unwrap()
                .push((url, serde_json::from_str(&request.body).expect("JSON body")));
            Ok((200, Some(self.answer.clone())))
        }
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

struct Harness {
    _root: tempfile::TempDir,
    root: String,
    directory: PathBuf,
    watcher: Delegating,
    reported: Arc<Mutex<Vec<Reported>>>,
    sender: Arc<FakeSender>,
}

fn with_watcher(_per_turn: usize, answer: Value) -> Harness {
    let root = tempfile::tempdir().expect("a temp directory");
    let root_path = root.path();
    let directory = root_path.join(crate::agent::requests::DELEGATE_DIR);
    std::fs::create_dir_all(&directory).expect("the exchange directory is made");
    let project = root_path.join("project");
    std::fs::create_dir_all(&project).expect("the project is made");

    let sender = Arc::new(FakeSender {
        answer,
        sent: Mutex::new(Vec::new()),
    });
    let reported = Arc::new(Mutex::new(Vec::new()));
    let watcher = Delegating::new(root_path, silent(), {
        let reported = Arc::clone(&reported);
        Arc::new(move |outcome| reported.lock().unwrap().push(outcome))
    });

    Harness {
        _root: root,
        root: project.display().to_string(),
        directory,
        watcher,
        reported,
        sender,
    }
}

impl Harness {
    fn turn(&self, per_turn: usize) -> TurnDelegations<DiskSources, FakeSender> {
        TurnDelegations::new(
            "s1",
            endpoint(),
            Scheduler::start(
                LimitsConfig {
                    max_concurrent_turns: 4,
                    max_live_sessions: 4,
                    max_queue_length: 8,
                    max_queue_wait_ms: 1_000,
                },
                Arc::new(NoClock),
                5_000,
                300_000,
                750,
            ),
            Arc::new(DiskSources {
                root: self.root.clone(),
            }),
            5_000,
            per_turn,
            Arc::clone(&self.sender),
        )
    }

    fn request(&self, id: &str, body: Value) {
        std::fs::write(
            self.directory.join(format!("{id}.request")),
            body.to_string(),
        )
        .expect("the request is written");
    }

    fn read_of(&self, id: &str, kind: &str) -> Option<String> {
        std::fs::read_to_string(self.directory.join(format!("{id}.{kind}"))).ok()
    }

    fn answer_of(&self, id: &str) -> Option<String> {
        self.read_of(id, "answer")
    }

    fn refusal_of(&self, id: &str) -> Option<String> {
        self.read_of(id, "refused")
    }

    fn asked(&self) -> Vec<(String, Value)> {
        self.sender.sent.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn a_question_about_a_calls_output_is_asked_and_answered() {
    let harness = with_watcher(
        8,
        json!({
            "choices": [{ "message": { "content": "it says ENOSPC" } }],
            "usage": { "total_tokens": 12 }
        }),
    );
    let mut turn = harness.turn(8);
    harness.request("d1", json!({ "question": "what failed?", "callId": "t1" }));

    harness.watcher.sweep(Some(&mut turn)).await;

    let answer = harness.answer_of("d1").unwrap_or_default();
    assert!(answer.contains("it says ENOSPC"));
    assert!(answer.contains("flash was asked about"));
    assert!(answer.contains("description rather than the thing itself"));
}

/// The model is shown one artefact and nothing about the session.
#[tokio::test]
async fn only_the_question_and_the_artefact_are_sent() {
    let harness = with_watcher(
        8,
        json!({
            "choices": [{ "message": { "content": "it says ENOSPC" } }],
            "usage": { "total_tokens": 12 }
        }),
    );
    let mut turn = harness.turn(8);
    harness.request("d1", json!({ "question": "what failed?", "callId": "t1" }));

    harness.watcher.sweep(Some(&mut turn)).await;

    let asked = harness.asked();
    assert_eq!(asked.len(), 1);
    let body = &asked[0].1;
    assert_eq!(body.get("tools"), None);
    let sent = body.to_string();
    assert!(sent.contains("what failed?"));
    assert!(sent.contains("ENOSPC: no space left"));
    assert_eq!(
        body.get("messages").and_then(Value::as_array).map(Vec::len),
        Some(1)
    );
}

#[tokio::test]
async fn a_question_about_a_project_file_reads_that_file() {
    let harness = with_watcher(
        8,
        json!({
            "choices": [{ "message": { "content": "it exports x" } }],
            "usage": {}
        }),
    );
    std::fs::write(
        Path::new(&harness.root).join("main.ts"),
        "export const x = 1;\n",
    )
    .expect("the file is written");
    let mut turn = harness.turn(8);
    harness.request(
        "d1",
        json!({ "question": "what does it export?", "path": "main.ts" }),
    );

    harness.watcher.sweep(Some(&mut turn)).await;

    assert!(
        harness.asked()[0]
            .1
            .to_string()
            .contains("export const x = 1;")
    );
}

/// The same containment the agent is held to, not a second looser one.
#[tokio::test]
async fn a_file_outside_the_project_is_refused_and_nothing_is_asked() {
    let harness = with_watcher(
        8,
        json!({
            "choices": [{ "message": { "content": "x" } }],
            "usage": {}
        }),
    );
    let mut turn = harness.turn(8);
    harness.request(
        "d1",
        json!({ "question": "read this", "path": "../../../etc/passwd" }),
    );

    harness.watcher.sweep(Some(&mut turn)).await;

    assert!(harness.asked().is_empty());
    let refusal = harness.refusal_of("d1").unwrap_or_default();
    assert!(refusal.contains("outside this session's project"));
    assert!(refusal.contains("carry on yourself"));
}

/// A delegation with no artefact is a conversation, which is what this is not.
#[tokio::test]
async fn a_request_naming_nothing_to_look_at_is_refused() {
    let harness = with_watcher(
        8,
        json!({
            "choices": [{ "message": { "content": "x" } }],
            "usage": {}
        }),
    );
    let mut turn = harness.turn(8);
    harness.request(
        "d1",
        json!({ "question": "what should I do about the parser?" }),
    );

    harness.watcher.sweep(Some(&mut turn)).await;

    assert!(harness.asked().is_empty());
    assert!(
        harness
            .refusal_of("d1")
            .unwrap_or_default()
            .contains("must name what to look at")
    );
}

#[tokio::test]
async fn a_request_naming_several_things_is_refused() {
    let harness = with_watcher(
        8,
        json!({
            "choices": [{ "message": { "content": "x" } }],
            "usage": {}
        }),
    );
    let mut turn = harness.turn(8);
    harness.request(
        "d1",
        json!({ "question": "look", "path": "main.ts", "callId": "t1" }),
    );

    harness.watcher.sweep(Some(&mut turn)).await;

    assert!(
        harness
            .refusal_of("d1")
            .unwrap_or_default()
            .contains("one thing to look at, not several")
    );
}

#[tokio::test]
async fn a_request_that_is_not_a_request_at_all_is_refused_not_thrown() {
    let harness = with_watcher(
        8,
        json!({
            "choices": [{ "message": { "content": "x" } }],
            "usage": {}
        }),
    );
    let mut turn = harness.turn(8);
    std::fs::write(harness.directory.join("d1.request"), "{ not json")
        .expect("the torn request is written");

    harness.watcher.sweep(Some(&mut turn)).await;

    assert!(
        harness
            .refusal_of("d1")
            .unwrap_or_default()
            .contains("could not be read")
    );
}

/// The cap is per turn, so a loop cannot become a stream of requests.
#[tokio::test]
async fn a_turn_stops_delegating_once_it_has_used_its_allowance() {
    let harness = with_watcher(
        2,
        json!({
            "choices": [{ "message": { "content": "it says ENOSPC" } }],
            "usage": { "total_tokens": 12 }
        }),
    );
    let mut turn = harness.turn(2);
    for id in ["d1", "d2", "d3"] {
        harness.request(id, json!({ "question": "what failed?", "callId": "t1" }));
    }

    harness.watcher.sweep(Some(&mut turn)).await;

    assert_eq!(harness.asked().len(), 2);
    assert!(
        harness
            .refusal_of("d3")
            .unwrap_or_default()
            .contains("already delegated 2 times")
    );
}

#[tokio::test]
async fn a_delegation_with_no_turn_running_is_refused_rather_than_queued() {
    let harness = with_watcher(
        8,
        json!({
            "choices": [{ "message": { "content": "x" } }],
            "usage": {}
        }),
    );
    harness.request("d1", json!({ "question": "what failed?", "callId": "t1" }));

    harness.watcher.sweep::<DiskSources, FakeSender>(None).await;

    assert!(harness.asked().is_empty());
    assert!(
        harness
            .refusal_of("d1")
            .unwrap_or_default()
            .contains("no turn running")
    );
}

/// Answered twice is worse than answered late, so the request goes first.
#[tokio::test]
async fn a_request_is_taken_away_before_it_is_run() {
    let harness = with_watcher(
        8,
        json!({
            "choices": [{ "message": { "content": "it says ENOSPC" } }],
            "usage": { "total_tokens": 12 }
        }),
    );
    let mut turn = harness.turn(8);
    harness.request("d1", json!({ "question": "what failed?", "callId": "t1" }));

    harness.watcher.sweep(Some(&mut turn)).await;
    harness.watcher.sweep(Some(&mut turn)).await;

    assert_eq!(harness.asked().len(), 1);
}

#[tokio::test]
async fn what_happened_is_reported_with_what_it_cost_and_saved() {
    let harness = with_watcher(
        8,
        json!({
            "choices": [{ "message": { "content": "it says ENOSPC" } }],
            "usage": { "total_tokens": 12 }
        }),
    );
    let mut turn = harness.turn(8);
    harness.request("d1", json!({ "question": "what failed?", "callId": "t1" }));

    harness.watcher.sweep(Some(&mut turn)).await;

    let reported = harness.reported.lock().unwrap();
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].asked, "what failed?");
    let crate::agent::delegate::DelegationOutcome::Ready(answer) = &reported[0].outcome else {
        panic!("the delegation was answered");
    };
    assert_eq!(answer.model, "flash");
    assert_eq!(answer.kept_out, 21);
}

/// An agent that forgets it did not read the thing asserts a description.
#[test]
fn an_answer_says_which_model_produced_it_and_that_it_is_a_description() {
    let said = labelled(&crate::agent::delegate::Answer {
        text: "the log shows a failed write".to_owned(),
        model: "glm-5.3-flash".to_owned(),
        describes: "the output of call t1".to_owned(),
        tokens: Some(12),
        kept_out: 4_000,
    });

    assert!(said.contains("glm-5.3-flash was asked about the output of call t1"));
    assert!(said.contains("description rather than the thing itself"));
    assert!(said.contains("the log shows a failed write"));
}
