//! Tests for the agent client over ACP frames.

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

/// A process a test writes frames into, standing in for the agent.
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
    let agent = AgentClient::new(
        fake,
        handlers,
        silent(),
        dialog_timeout_ms,
        None,
        "/workspace",
    );
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

/// Drives the handshake: initialize answered, session opened.
async fn handshake(setup: &TestSetup) {
    let asking = setup.agent.clone();
    let ready = tokio::spawn(async move { asking.wait_until_ready(1_000).await });
    settle().await;
    let first: Value =
        serde_json::from_str(&setup.controls.written()[0]).expect("the handshake first");
    assert_eq!(first["method"], "initialize");
    setup
        .controls
        .send(&json!({ "jsonrpc": "2.0", "id": first["id"], "result": { "protocolVersion": 1 } }));
    settle().await;
    let second: Value =
        serde_json::from_str(&setup.controls.written()[1]).expect("the handshake second");
    assert_eq!(second["method"], "session/new");
    assert_eq!(second["params"]["cwd"], "/workspace");
    setup
        .controls
        .send(&json!({ "jsonrpc": "2.0", "id": second["id"], "result": { "sessionId": "s-1" } }));
    ready.await.expect("driven").expect("ready");
    settle().await;
}

/// One turn notification, as the agent streams it.
fn update(update: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": { "sessionId": "s-1", "update": update },
    })
}

fn turn_start() -> Value {
    update(&json!({ "sessionUpdate": "_kage/turn", "phase": "start" }))
}

fn turn_end() -> Value {
    update(&json!({ "sessionUpdate": "_kage/turn", "phase": "end" }))
}

fn text_chunk(text: &str) -> Value {
    update(&json!({
        "sessionUpdate": "agent_message_chunk",
        "content": { "type": "text", "text": text },
    }))
}

/// The last frame the client wrote matching `method`.
fn last_written(controls: &FakeControls, method: &str) -> Value {
    controls
        .written()
        .iter()
        .rev()
        .map(|line| serde_json::from_str::<Value>(line).expect("JSON"))
        .find(|frame| frame.get("method").and_then(Value::as_str) == Some(method))
        .unwrap_or_else(|| panic!("a {method} frame"))
}

#[tokio::test]
async fn readiness_is_the_handshake_answering_and_it_opens_the_session() {
    let setup = client(AgentHandlers::default(), 300_000);
    handshake(&setup).await;

    assert_eq!(setup.agent.state(), super::AgentState::Ready);
    assert!(setup.agent.prompt("hello", None, None));
    settle().await;
    let sent = last_written(&setup.controls, "session/prompt");
    assert_eq!(sent["params"]["sessionId"], "s-1");
    assert_eq!(sent["params"]["prompt"][0]["text"], "hello");

    setup.finish().await;
}

#[tokio::test]
async fn a_handshake_the_agent_refuses_is_not_readiness() {
    let setup = client(AgentHandlers::default(), 300_000);
    let asking = setup.agent.clone();
    let ready = tokio::spawn(async move { asking.wait_until_ready(1_000).await });
    settle().await;
    let first: Value =
        serde_json::from_str(&setup.controls.written()[0]).expect("the handshake first");
    setup
        .controls
        .send(&json!({ "jsonrpc": "2.0", "id": first["id"], "result": { "protocolVersion": 1 } }));
    settle().await;
    let second: Value =
        serde_json::from_str(&setup.controls.written()[1]).expect("the handshake second");
    setup.controls.send(&json!({
        "jsonrpc": "2.0", "id": second["id"],
        "error": { "code": -32602, "message": "unknown session" },
    }));
    let error = ready.await.expect("driven").expect_err("refused");
    assert!(error.contains("unknown session"), "{error}");
    assert_eq!(setup.agent.state(), super::AgentState::Starting);

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

    setup.controls.send(&turn_start());
    settle().await;
    assert_eq!(setup.agent.state(), super::AgentState::Working);
    assert!(setup.agent.is_working());

    setup.controls.send(&text_chunk("done"));
    setup.controls.send(&turn_end());
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
    first.controls.send(&turn_start());
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
async fn prompts_before_readiness_are_dropped_for_want_of_a_session() {
    let setup = client(AgentHandlers::default(), 300_000);

    assert!(!setup.agent.prompt("too early", None, None));
    assert!(setup.controls.written().is_empty());

    setup.finish().await;
}

#[tokio::test]
async fn steering_sends_the_prompt_with_delivery_steer() {
    let setup = client(AgentHandlers::default(), 300_000);
    handshake(&setup).await;

    assert!(setup.agent.steer("that way", None));
    settle().await;
    let sent = last_written(&setup.controls, "session/prompt");
    assert_eq!(sent["params"]["delivery"], "steer");
    assert_eq!(sent["params"]["prompt"][0]["text"], "that way");

    setup.finish().await;
}

#[tokio::test]
async fn abort_sends_cancel_and_is_true_with_nothing_to_stop() {
    let setup = client(AgentHandlers::default(), 300_000);
    assert!(setup.agent.abort());

    handshake(&setup).await;
    assert!(setup.agent.abort());
    settle().await;
    let sent = last_written(&setup.controls, "session/cancel");
    assert_eq!(sent["params"]["sessionId"], "s-1");

    setup.finish().await;
}

#[tokio::test]
async fn switching_models_sends_the_qualified_id_and_refusals_fail() {
    let setup = client(AgentHandlers::default(), 300_000);
    handshake(&setup).await;

    let switching = setup.agent.clone();
    let switched = tokio::spawn(async move { switching.set_model("mock", "other", 1_000).await });
    settle().await;
    let sent = last_written(&setup.controls, "session/set_config_option");
    assert_eq!(sent["params"]["configId"], "model");
    assert_eq!(sent["params"]["value"], "mock/other");
    setup.controls.send(&json!({
        "jsonrpc": "2.0", "id": sent["id"], "result": {},
    }));
    switched.await.expect("driven").expect("switched");

    let refusing = setup.agent.clone();
    let refused = tokio::spawn(async move { refusing.set_model("mock", "missing", 1_000).await });
    settle().await;
    let denied = last_written(&setup.controls, "session/set_config_option");
    setup.controls.send(&json!({
        "jsonrpc": "2.0", "id": denied["id"],
        "error": { "code": -32602, "message": "unknown model mock/missing" },
    }));
    let error = refused.await.expect("driven").expect_err("refused");
    assert!(error.contains("unknown model"), "{error}");

    setup.finish().await;
}

#[tokio::test]
async fn compacting_reports_the_counts_from_the_compaction_update() {
    let setup = client(AgentHandlers::default(), 300_000);
    handshake(&setup).await;

    let compacting = setup.agent.clone();
    let compacted = tokio::spawn(async move { compacting.compact(1_000).await });
    settle().await;
    let sent = last_written(&setup.controls, "_kage/session/compact");
    setup.controls.send(&update(&json!({
        "sessionUpdate": "_kage/compaction",
        "kept": 2, "before": 9000, "after": 3000,
    })));
    setup
        .controls
        .send(&json!({ "jsonrpc": "2.0", "id": sent["id"], "result": {} }));
    let answer = compacted.await.expect("driven").expect("compacted");
    assert_eq!(answer["data"]["tokensBefore"], 9000.0);
    assert_eq!(answer["data"]["estimatedTokensAfter"], 3000.0);

    setup.finish().await;
}

#[tokio::test]
async fn text_chunks_are_reported_as_they_arrive() {
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

    setup.controls.send(&text_chunk("the answer"));
    setup.controls.send(&update(&json!({
        "sessionUpdate": "agent_message_chunk",
        "content": { "type": "image", "data": "aGk=", "mimeType": "image/png" },
    })));
    settle().await;

    assert_eq!(*said.lock().unwrap(), vec!["the answer".to_owned()]);
    setup.finish().await;
}

#[tokio::test]
async fn thinking_is_reported_once_a_turn_and_each_thought_as_it_arrives() {
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

    setup.controls.send(&turn_start());
    setup.controls.send(&update(&json!({
        "sessionUpdate": "agent_thought_chunk",
        "content": { "type": "text", "text": "weighed it" },
    })));
    setup.controls.send(&update(&json!({
        "sessionUpdate": "agent_thought_chunk",
        "content": { "type": "text", "text": "then this" },
    })));
    settle().await;

    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            "thinking".to_owned(),
            "thought:weighed it".to_owned(),
            "thought:then this".to_owned()
        ]
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

    setup.controls.send(&update(&json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "t1", "title": "bash",
        "rawInput": { "command": "ls -la" },
    })));
    setup.controls.send(&update(&json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": "t1", "status": "failed",
        "content": [{ "type": "text", "text": "no such file" }],
    })));
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
async fn usage_is_reported_from_the_update_and_sizes_the_window() {
    let seen: Arc<Mutex<Vec<Usage>>> = Arc::new(Mutex::new(Vec::new()));
    let usage_seen = Arc::clone(&seen);
    let setup = client(
        AgentHandlers {
            on_usage: Some(Box::new(move |usage: Usage| {
                usage_seen.lock().unwrap().push(usage);
            })),
            ..AgentHandlers::default()
        },
        300_000,
    );

    setup.controls.send(&update(&json!({
        "sessionUpdate": "usage_update",
        "used": 900, "size": 200_000, "cost": { "amount": 0.02 },
    })));
    settle().await;

    assert_eq!(setup.agent.context_window(), Some(200_000.0));
    assert_eq!(seen.lock().unwrap().len(), 1);

    setup.finish().await;
}

fn permission_ask(id: u64, options: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/request_permission",
        "params": {
            "sessionId": "s-1",
            "toolCall": {
                "toolCallId": "c1", "title": "rm",
                "rawInput": { "command": "rm -rf /tmp/x" },
            },
            "options": options,
        },
    })
}

fn allow_deny() -> Value {
    json!([
        { "optionId": "allow", "name": "Allow", "kind": "allowOnce" },
        { "optionId": "deny", "name": "Deny", "kind": "rejectOnce" },
    ])
}

#[tokio::test]
async fn a_select_dialog_is_answered_by_number_or_by_name() {
    let setup = client(AgentHandlers::default(), 300_000);
    setup.controls.send(&permission_ask(41, &allow_deny()));
    settle().await;

    assert_eq!(
        setup.agent.pending_dialog().map(|dialog| dialog.title),
        Some("rm".to_owned())
    );
    assert_eq!(
        setup.agent.answer_dialog("41", "2"),
        AnswerOutcome::Accepted
    );
    let last: Value =
        serde_json::from_str(setup.controls.written().last().expect("a written command"))
            .expect("JSON");
    assert_eq!(last["id"], 41);
    assert_eq!(last["result"]["outcome"]["optionId"], "deny");

    setup.controls.send(&permission_ask(42, &allow_deny()));
    settle().await;
    assert_eq!(
        setup.agent.answer_dialog("42", "ALLOW"),
        AnswerOutcome::Accepted
    );
    let last: Value =
        serde_json::from_str(setup.controls.written().last().expect("a written command"))
            .expect("JSON");
    assert_eq!(last["result"]["outcome"]["optionId"], "allow");

    setup.finish().await;
}

#[tokio::test]
async fn a_reply_that_answers_nothing_leaves_the_dialog_standing() {
    let setup = client(AgentHandlers::default(), 300_000);
    setup.controls.send(&permission_ask(41, &allow_deny()));
    settle().await;

    assert_eq!(
        setup.agent.answer_dialog("41", "maybe later"),
        AnswerOutcome::Unrecognized
    );
    assert_eq!(
        setup.agent.pending_dialog().map(|dialog| dialog.id),
        Some("41".to_owned())
    );
    assert_eq!(
        setup.agent.answer_dialog("99", "Allow"),
        AnswerOutcome::Unknown
    );
    assert_eq!(
        setup.agent.answer_dialog("41", "Allow"),
        AnswerOutcome::Accepted
    );

    setup.finish().await;
}

/// A chat thread cannot serve a plan review, so it is cancelled rather than
/// hung on.
#[tokio::test]
async fn a_plan_review_is_cancelled_immediately() {
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
        "jsonrpc": "2.0",
        "id": 41,
        "method": "session/request_permission",
        "params": {
            "sessionId": "s-1",
            "toolCall": { "toolCallId": "c1" },
            "options": [{ "optionId": "a", "name": "A", "kind": "allowOnce" }],
            "_meta": { "kage": { "planReview": { "plan": "do it" } } },
        },
    }));
    settle().await;

    assert_eq!(*unsupported.lock().unwrap(), vec!["editor".to_owned()]);
    assert_eq!(setup.agent.pending_dialog(), None);
    let last: Value =
        serde_json::from_str(setup.controls.written().last().expect("a written command"))
            .expect("JSON");
    assert_eq!(last["id"], 41);
    assert_eq!(last["result"]["outcome"]["outcome"], "cancelled");

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

    setup.controls.send(&permission_ask(41, &allow_deny()));
    settle().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(*timed_out.lock().unwrap(), vec!["41".to_owned()]);
    assert_eq!(setup.agent.pending_dialog(), None);
    let last: Value =
        serde_json::from_str(setup.controls.written().last().expect("a written command"))
            .expect("JSON");
    assert_eq!(last["result"]["outcome"]["outcome"], "cancelled");

    setup.finish().await;
}

#[tokio::test]
async fn an_ask_the_agent_withdraws_releases_the_caller() {
    let timed_out: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let timeout_seen = Arc::clone(&timed_out);
    let setup = client(
        AgentHandlers {
            on_dialog_timeout: Some(Box::new(move |request: DialogRequest| {
                timeout_seen.lock().unwrap().push(request.id);
            })),
            ..AgentHandlers::default()
        },
        300_000,
    );

    setup.controls.send(&permission_ask(41, &allow_deny()));
    settle().await;
    assert!(setup.agent.pending_dialog().is_some());
    setup.controls.send(&json!({
        "jsonrpc": "2.0",
        "method": "$/cancel_request",
        "params": { "requestId": 41 },
    }));
    settle().await;

    assert_eq!(*timed_out.lock().unwrap(), vec!["41".to_owned()]);
    assert_eq!(setup.agent.pending_dialog(), None);

    setup.finish().await;
}

#[tokio::test]
async fn an_unknown_method_is_refused_so_the_agent_never_waits() {
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
        "jsonrpc": "2.0",
        "id": 7,
        "method": "session/tell_joke",
        "params": {},
    }));
    setup.controls.send(&json!({
        "jsonrpc": "2.0",
        "method": "window/logMessage",
        "params": {},
    }));
    settle().await;

    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(setup.agent.pending_dialog(), None);
    let last: Value =
        serde_json::from_str(setup.controls.written().last().expect("a written command"))
            .expect("JSON");
    assert_eq!(last["id"], 7);
    assert_eq!(last["error"]["code"], -32601);

    setup.finish().await;
}

#[tokio::test]
async fn a_prompt_the_agent_refuses_is_reported() {
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
    handshake(&setup).await;
    assert!(setup.agent.prompt("hello", None, None));
    settle().await;

    let sent = last_written(&setup.controls, "session/prompt");
    setup.controls.send(&json!({
        "jsonrpc": "2.0", "id": sent["id"],
        "error": { "code": -32602, "message": "no api key" },
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
    let answered = tokio::spawn(async move {
        asking
            .request(
                json!({ "jsonrpc": "2.0", "method": "_kage/session/compact", "params": {} }),
                1_000,
            )
            .await
    });
    settle().await;
    let sent: Value =
        serde_json::from_str(setup.controls.written().last().expect("a written command"))
            .expect("JSON");
    setup.controls.send(&json!({
        "jsonrpc": "2.0", "id": sent["id"], "result": { "freed": 1_200 },
    }));
    let record = answered.await.expect("answered").expect("answered");
    assert_eq!(record["result"]["freed"], 1_200);

    let deadline_ask = setup.agent.clone();
    let deadline = tokio::spawn(async move {
        deadline_ask
            .request(
                json!({ "jsonrpc": "2.0", "method": "_kage/session/compact", "params": {} }),
                10,
            )
            .await
    });
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
            .request(
                json!({ "jsonrpc": "2.0", "method": "_kage/session/compact", "params": {} }),
                60_000,
            )
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

/// Handlers that record each settled turn's failure, and where they go.
fn settled_failures() -> (AgentHandlers, Arc<Mutex<Vec<Option<String>>>>) {
    let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let handlers = AgentHandlers {
        on_turn_settled: Some(Box::new(move |(_, failure): (bool, Option<String>)| {
            recorded.lock().unwrap().push(failure);
        })),
        ..AgentHandlers::default()
    };
    (handlers, seen)
}

/// The provider's own words reach whoever is told the turn failed.
#[tokio::test]
async fn a_refused_turn_carries_the_refusal() {
    let (handlers, seen) = settled_failures();
    let setup = client(handlers, 300_000);
    handshake(&setup).await;
    assert!(setup.agent.prompt("hello", None, None));
    settle().await;

    setup.controls.send(&turn_start());
    let sent = last_written(&setup.controls, "session/prompt");
    setup.controls.send(&text_chunk("never mind"));
    setup.controls.send(&json!({
        "jsonrpc": "2.0", "id": sent["id"], "result": { "stopReason": "refusal" },
    }));
    setup.controls.send(&turn_end());
    settle().await;

    assert_eq!(
        *seen.lock().unwrap(),
        vec![Some("the model refused".to_owned())]
    );
    setup.finish().await;
}

/// A failure the agent will not retry past ends the run, so the turn fails
/// with it and still backs the admission off.
#[tokio::test]
async fn a_retryable_prompt_failure_backs_off_and_fails_the_turn() {
    let (mut handlers, seen) = settled_failures();
    let retries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let retry_seen = Arc::clone(&retries);
    handlers.on_retry = Some(Box::new(move |detail: String| {
        retry_seen.lock().unwrap().push(detail);
    }));
    let setup = client(handlers, 300_000);
    handshake(&setup).await;
    assert!(setup.agent.prompt("hello", None, None));
    settle().await;

    setup.controls.send(&turn_start());
    let sent = last_written(&setup.controls, "session/prompt");
    setup.controls.send(&json!({
        "jsonrpc": "2.0", "id": sent["id"],
        "error": { "code": -32000, "message": "429 overloaded_error" },
    }));
    setup.controls.send(&text_chunk("done"));
    setup.controls.send(&turn_end());
    settle().await;

    assert_eq!(
        *retries.lock().unwrap(),
        vec!["429 overloaded_error".to_owned()]
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec![Some("429 overloaded_error".to_owned())]
    );
    setup.finish().await;
}

#[tokio::test]
async fn a_notice_from_the_agent_is_reported_as_an_error() {
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let error_seen = Arc::clone(&errors);
    let setup = client(
        AgentHandlers {
            on_error: Some(Box::new(move |detail: String| {
                error_seen.lock().unwrap().push(detail);
            })),
            ..AgentHandlers::default()
        },
        300_000,
    );

    setup.controls.send(&update(&json!({
        "sessionUpdate": "_kage/notice",
        "tone": "error",
        "text": "the provider dropped the connection",
    })));
    settle().await;

    assert_eq!(
        *errors.lock().unwrap(),
        vec!["the provider dropped the connection".to_owned()]
    );
    setup.finish().await;
}
