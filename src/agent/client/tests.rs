//! Tests for the agent client, ported from `client_test.ts`.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::watch;

use super::{AgentClient, AgentHandlers, AgentProcess, AnswerOutcome};
use crate::agent::protocol::DialogRequest;
use crate::agent::protocol::Usage;
use crate::log::{LogFields, Logger};

/// A process a test writes records into, standing in for the agent.
struct Fake {
    exit: watch::Receiver<Option<i32>>,
    queue: Mutex<VecDeque<u8>>,
    written: Mutex<Vec<String>>,
    closed: Mutex<bool>,
}

fn fake_process() -> (Arc<Fake>, FakeControls) {
    let (exit_sender, exit_receiver) = watch::channel(None);
    let fake = Arc::new(Fake {
        queue: Mutex::new(VecDeque::new()),
        written: Mutex::new(Vec::new()),
        closed: Mutex::new(false),
        exit: exit_receiver,
    });
    let controls = FakeControls {
        fake: Arc::clone(&fake),
        sender: Mutex::new(Some(exit_sender)),
    };
    (fake, controls)
}

struct FakeControls {
    fake: Arc<Fake>,
    sender: Mutex<Option<watch::Sender<Option<i32>>>>,
}

impl FakeControls {
    fn send(&self, record: &Value) {
        self.chunk(&format!("{record}\n"));
    }

    /// Bytes exactly as given, so a test can send something malformed.
    fn chunk(&self, text: &str) {
        self.fake.queue.lock().unwrap().extend(text.bytes());
    }

    fn written(&self) -> Vec<String> {
        self.fake.written.lock().unwrap().clone()
    }

    fn close(&self, code: i32) {
        *self.fake.closed.lock().unwrap() = true;
        self.fake.queue.lock().unwrap().clear();
        if let Some(sender) = self.sender.lock().unwrap().take() {
            let _ = sender.send(Some(code));
        }
    }
}

impl AgentProcess for Fake {
    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
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
            // An open stream with nothing buffered stays pending until a push
            // or the close, which is what a live agent's stdout does.
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

struct TestSetup {
    agent: AgentClient,
    controls: FakeControls,
    done: tokio::task::JoinHandle<()>,
}

fn client(handlers: AgentHandlers, dialog_timeout_ms: u64) -> TestSetup {
    let (fake, controls) = fake_process();
    let agent = AgentClient::new(fake, handlers, silent(), dialog_timeout_ms, None);
    let run_client = agent.clone();
    let done = tokio::spawn(async move { run_client.run().await });
    TestSetup {
        agent,
        controls,
        done,
    }
}

fn silent() -> Logger {
    Logger::new(LogFields::new(), Arc::new(|_level, _line| {}))
}

/// Lets the stream reader drain what a test just pushed.
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
}

impl TestSetup {
    async fn finish(self) {
        self.controls.close(0);
        let _ = self.done.await;
    }
}

#[tokio::test]
async fn readiness_is_the_agent_answering_and_it_carries_the_context_window() {
    let setup = client(AgentHandlers::default(), 300_000);
    let asking = setup.agent.clone();
    let ready = tokio::spawn(async move { asking.wait_until_ready(1_000).await });
    settle().await;
    let sent: Value =
        serde_json::from_str(&setup.controls.written()[0]).expect("the readiness command");
    assert_eq!(sent["type"], "get_state");

    setup
        .controls
        .send(&json!({ "type": "response", "id": sent["id"], "data": { "model": { "contextWindow": 200_000 } } }));
    let _ = ready.await.expect("readiness resolves");

    assert_eq!(setup.agent.context_window(), Some(200_000.0));
    assert_eq!(setup.agent.state(), super::AgentState::Ready);
    setup.finish().await;
}

#[tokio::test]
async fn a_turn_moves_the_agent_through_working_and_back_to_ready() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let start_seen = Arc::clone(&seen);
    let settled_seen = Arc::clone(&seen);
    let setup = client(
        AgentHandlers {
            on_turn_start: Some(Box::new(move |()| {
                start_seen.lock().unwrap().push("start".to_owned());
            })),
            on_turn_settled: Some(Box::new(move |(produced, _): (bool, Option<String>)| {
                settled_seen
                    .lock()
                    .unwrap()
                    .push(format!("settled:{produced}"));
            })),
            ..AgentHandlers::default()
        },
        300_000,
    );

    setup.controls.send(&json!({ "type": "agent_start" }));
    settle().await;
    assert_eq!(setup.agent.state(), super::AgentState::Working);
    assert!(setup.agent.is_working());

    setup.controls.send(&json!({
        "type": "message_end",
        "message": { "role": "assistant", "content": [{ "type": "text", "text": "done" }] },
    }));
    setup.controls.send(&json!({ "type": "agent_settled" }));
    settle().await;

    assert_eq!(setup.agent.state(), super::AgentState::Ready);
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["start".to_owned(), "settled:true".to_owned()]
    );
    setup.finish().await;
}

/// The caller has to release what it was holding for a turn that will never
/// settle, so it is told whether one was running.
#[tokio::test]
async fn an_exit_during_a_turn_says_so_and_an_idle_exit_does_not() {
    let during: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));

    let first_during = Arc::clone(&during);
    let first = client(
        AgentHandlers {
            on_exit: Some(Box::new(move |(_, during_turn): (i64, bool)| {
                first_during.lock().unwrap().push(during_turn);
            })),
            ..AgentHandlers::default()
        },
        300_000,
    );
    first.controls.send(&json!({ "type": "agent_start" }));
    settle().await;
    first.controls.close(1);
    let _ = first.done.await;

    let second_during = Arc::clone(&during);
    let second = client(
        AgentHandlers {
            on_exit: Some(Box::new(move |(_, during_turn): (i64, bool)| {
                second_during.lock().unwrap().push(during_turn);
            })),
            ..AgentHandlers::default()
        },
        300_000,
    );
    second.controls.close(0);
    let _ = second.done.await;

    assert_eq!(*during.lock().unwrap(), vec![true, false]);
}

#[tokio::test]
async fn the_exit_is_reported_once_with_the_code() {
    let codes: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let exit_codes = Arc::clone(&codes);
    let setup = client(
        AgentHandlers {
            on_exit: Some(Box::new(move |(code, _): (i64, bool)| {
                exit_codes.lock().unwrap().push(code);
            })),
            ..AgentHandlers::default()
        },
        300_000,
    );

    setup.controls.close(3);
    let _ = setup.done.await;

    assert_eq!(*codes.lock().unwrap(), vec![3]);
}

#[tokio::test]
async fn commands_are_dropped_once_the_agent_has_ended() {
    let setup = client(AgentHandlers::default(), 300_000);
    setup.controls.close(0);
    let _ = setup.done.await;

    assert!(!setup.agent.prompt("anything", None, None));
    assert!(!setup.agent.is_alive());
}

#[tokio::test]
async fn only_the_assistants_words_are_reported_not_the_prompt_echoed_back() {
    let said: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let text_seen = Arc::clone(&said);
    let setup = client(
        AgentHandlers {
            on_assistant_text: Some(Box::new(move |text: String| {
                text_seen.lock().unwrap().push(text);
            })),
            ..AgentHandlers::default()
        },
        300_000,
    );

    setup.controls.send(&json!({
        "type": "message_end",
        "message": { "role": "user", "content": [{ "type": "text", "text": "the prompt" }] },
    }));
    setup.controls.send(&json!({
        "type": "message_end",
        "message": { "role": "assistant", "content": [{ "type": "text", "text": "  the answer  " }] },
    }));
    settle().await;

    assert_eq!(*said.lock().unwrap(), vec!["the answer".to_owned()]);
    setup.finish().await;
}

#[tokio::test]
async fn thinking_is_reported_once_a_turn_and_each_finished_thought_once() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let thinking_seen = Arc::clone(&seen);
    let thought_seen = Arc::clone(&seen);
    let setup = client(
        AgentHandlers {
            on_thinking: Some(Box::new(move |()| {
                thinking_seen.lock().unwrap().push("thinking".to_owned());
            })),
            on_thought: Some(Box::new(move |text| {
                thought_seen.lock().unwrap().push(format!("thought:{text}"));
            })),
            ..AgentHandlers::default()
        },
        300_000,
    );

    setup.controls.send(&json!({ "type": "agent_start" }));
    setup.controls.send(
        &json!({ "type": "message_update", "assistantMessageEvent": { "type": "thinking_start" } }),
    );
    setup.controls.send(
        &json!({ "type": "message_update", "assistantMessageEvent": { "type": "thinking_start" } }),
    );
    setup.controls.send(&json!({
        "type": "message_update",
        "assistantMessageEvent": { "type": "thinking_end", "content": "weighed it" },
    }));
    settle().await;

    assert_eq!(
        *seen.lock().unwrap(),
        vec!["thinking".to_owned(), "thought:weighed it".to_owned()]
    );
    setup.finish().await;
}

#[tokio::test]
async fn tool_calls_report_their_target_and_whether_they_failed() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let start_seen = Arc::clone(&seen);
    let end_seen = Arc::clone(&seen);
    let setup = client(
        AgentHandlers {
            on_tool_start: Some(Box::new(
                move |(id, name, target): (String, String, Option<String>)| {
                    start_seen
                        .lock()
                        .unwrap()
                        .push(format!("start:{id}:{name}:{}", target.unwrap_or_default()));
                },
            )),
            on_tool_end: Some(Box::new(
                move |(id, _name, failed, output): (String, String, bool, String)| {
                    end_seen
                        .lock()
                        .unwrap()
                        .push(format!("end:{id}:{failed}:{output}"));
                },
            )),
            ..AgentHandlers::default()
        },
        300_000,
    );

    setup.controls.send(&json!({
        "type": "tool_execution_start",
        "toolCallId": "t1",
        "toolName": "bash",
        "args": { "command": "ls -la" },
    }));
    setup.controls.send(&json!({
        "type": "tool_execution_end",
        "toolCallId": "t1",
        "toolName": "bash",
        "isError": true,
        "result": { "content": [{ "type": "text", "text": "no such file" }] },
    }));
    settle().await;

    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            "start:t1:bash:ls -la".to_owned(),
            "end:t1:true:no such file".to_owned(),
        ]
    );
    setup.finish().await;
}

#[tokio::test]
async fn usage_is_reported_when_a_turn_ends() {
    let models: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let usage_models = Arc::clone(&models);
    let setup = client(
        AgentHandlers {
            on_usage: Some(Box::new(move |usage: Usage| {
                usage_models
                    .lock()
                    .unwrap()
                    .push(usage.model.clone().unwrap_or_default());
            })),
            ..AgentHandlers::default()
        },
        300_000,
    );

    setup.controls.send(&json!({
        "type": "turn_end",
        "message": { "model": "cheap-model",
                     "usage": { "input": 10, "output": 2, "totalTokens": 12 } },
    }));
    settle().await;

    assert_eq!(*models.lock().unwrap(), vec!["cheap-model".to_owned()]);
    setup.finish().await;
}

#[tokio::test]
async fn a_select_dialog_is_answered_by_number_or_by_name() {
    let setup = client(AgentHandlers::default(), 300_000);
    setup.controls.send(&json!({
        "type": "extension_ui_request",
        "id": "d-1",
        "method": "select",
        "title": "Which branch?",
        "options": ["main", "dev"],
    }));
    settle().await;

    assert_eq!(
        setup.agent.pending_dialog().map(|dialog| dialog.title),
        Some("Which branch?".to_owned())
    );
    assert_eq!(
        setup.agent.answer_dialog("d-1", "2"),
        AnswerOutcome::Accepted
    );
    let last: Value =
        serde_json::from_str(setup.controls.written().last().expect("a written command"))
            .expect("JSON");
    assert_eq!(last["value"], "dev");

    setup.controls.send(&json!({
        "type": "extension_ui_request",
        "id": "d-2",
        "method": "select",
        "title": "Again?",
        "options": ["main", "dev"],
    }));
    settle().await;
    assert_eq!(
        setup.agent.answer_dialog("d-2", "MAIN"),
        AnswerOutcome::Accepted
    );
    let last: Value =
        serde_json::from_str(setup.controls.written().last().expect("a written command"))
            .expect("JSON");
    assert_eq!(last["value"], "main");

    setup.finish().await;
}

#[tokio::test]
async fn a_reply_that_answers_nothing_leaves_the_dialog_standing() {
    let setup = client(AgentHandlers::default(), 300_000);
    setup.controls.send(&json!({
        "type": "extension_ui_request",
        "id": "d-1",
        "method": "confirm",
        "title": "Proceed?",
    }));
    settle().await;

    assert_eq!(
        setup.agent.answer_dialog("d-1", "maybe later"),
        AnswerOutcome::Unrecognized
    );
    assert_eq!(
        setup.agent.pending_dialog().map(|dialog| dialog.id),
        Some("d-1".to_owned())
    );
    assert_eq!(
        setup.agent.answer_dialog("d-9", "yes"),
        AnswerOutcome::Unknown
    );
    assert_eq!(
        setup.agent.answer_dialog("d-1", "yes"),
        AnswerOutcome::Accepted
    );
    let last: Value =
        serde_json::from_str(setup.controls.written().last().expect("a written command"))
            .expect("JSON");
    assert_eq!(last["confirmed"], true);

    setup.finish().await;
}

/// A chat thread cannot serve an editor, so it is cancelled rather than hung
/// on.
#[tokio::test]
async fn an_editor_request_is_cancelled_immediately() {
    let unsupported: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let unsupported_seen = Arc::clone(&unsupported);
    let setup = client(
        AgentHandlers {
            on_unsupported_dialog: Some(Box::new(move |method: String| {
                unsupported_seen.lock().unwrap().push(method);
            })),
            ..AgentHandlers::default()
        },
        300_000,
    );

    setup.controls.send(&json!({
        "type": "extension_ui_request",
        "id": "d-1",
        "method": "editor",
        "title": "Edit",
    }));
    settle().await;

    assert_eq!(*unsupported.lock().unwrap(), vec!["editor".to_owned()]);
    assert_eq!(setup.agent.pending_dialog(), None);
    let last: Value =
        serde_json::from_str(setup.controls.written().last().expect("a written command"))
            .expect("JSON");
    assert_eq!(last["cancelled"], true);

    setup.finish().await;
}

#[tokio::test]
async fn a_dialog_nobody_answers_is_cancelled_and_the_caller_told() {
    let timed_out: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let timeout_seen = Arc::clone(&timed_out);
    let setup = client(
        AgentHandlers {
            on_dialog_timeout: Some(Box::new(move |request: DialogRequest| {
                timeout_seen.lock().unwrap().push(request.id);
            })),
            ..AgentHandlers::default()
        },
        10,
    );

    setup.controls.send(&json!({
        "type": "extension_ui_request",
        "id": "d-1",
        "method": "confirm",
        "title": "Proceed?",
    }));
    settle().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(*timed_out.lock().unwrap(), vec!["d-1".to_owned()]);
    assert_eq!(setup.agent.pending_dialog(), None);

    setup.finish().await;
}

#[tokio::test]
async fn an_informational_request_is_neither_answered_nor_reported() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let dialog_seen = Arc::clone(&seen);
    let setup = client(
        AgentHandlers {
            on_dialog: Some(Box::new(move |request: DialogRequest| {
                dialog_seen.lock().unwrap().push(request.id);
            })),
            ..AgentHandlers::default()
        },
        300_000,
    );

    setup.controls.send(&json!({
        "type": "extension_ui_request",
        "id": "n-1",
        "method": "notify",
        "title": "hello",
    }));
    settle().await;

    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(setup.agent.pending_dialog(), None);
    assert!(setup.controls.written().is_empty());

    setup.finish().await;
}

#[tokio::test]
async fn a_command_the_agent_refuses_is_reported_id_or_no_id() {
    let rejected: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let rejected_seen = Arc::clone(&rejected);
    let setup = client(
        AgentHandlers {
            on_command_rejected: Some(Box::new(move |(command, detail): (String, String)| {
                rejected_seen
                    .lock()
                    .unwrap()
                    .push(format!("{command}:{detail}"));
            })),
            ..AgentHandlers::default()
        },
        300_000,
    );

    setup.controls.send(&json!({
        "type": "response",
        "command": "prompt",
        "success": false,
        "error": "no api key",
    }));
    settle().await;

    assert_eq!(
        *rejected.lock().unwrap(),
        vec!["prompt:no api key".to_owned()]
    );
    setup.finish().await;
}

#[tokio::test]
async fn a_request_resolves_on_its_answer_and_rejects_on_its_deadline() {
    let setup = client(AgentHandlers::default(), 300_000);

    let asking = setup.agent.clone();
    let answered =
        tokio::spawn(async move { asking.request(json!({ "type": "compact" }), 1_000).await });
    settle().await;
    let sent: Value =
        serde_json::from_str(setup.controls.written().last().expect("a written command"))
            .expect("JSON");
    setup
        .controls
        .send(&json!({ "type": "response", "id": sent["id"], "data": { "freed": 1_200 } }));
    let record = answered.await.expect("answered").expect("answered");
    assert_eq!(record["data"]["freed"], 1_200);

    let deadline_ask = setup.agent.clone();
    let deadline =
        tokio::spawn(async move { deadline_ask.request(json!({ "type": "compact" }), 10).await });
    let error = deadline.await.expect("driven").expect_err("refused");
    assert!(error.contains("did not answer"), "{error}");

    setup.finish().await;
}

#[tokio::test]
async fn a_request_outstanding_when_the_agent_dies_is_failed_not_left_hanging() {
    let setup = client(AgentHandlers::default(), 300_000);
    let pending_ask = setup.agent.clone();
    let pending = tokio::spawn(async move {
        pending_ask
            .request(json!({ "type": "compact" }), 60_000)
            .await
    });
    settle().await;

    setup.controls.close(1);
    let _ = setup.done.await;

    let outcome = pending.await.expect("driven");
    // The pending request is failed rather than left hanging; the exit code
    // names why.
    let failed = match outcome {
        Ok(record) => record["failed"]
            .as_str()
            .expect("a reason")
            .contains("exited with code 1"),
        Err(error) => error.contains("exited with code 1"),
    };
    assert!(failed);
}
