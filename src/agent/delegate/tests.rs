//! Tests for a turn's delegations, ported from `delegate_test.ts`.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::TurnDelegations;
use crate::admission::scheduler::{Clock, Scheduler};
use crate::agent::delegation::{Outcome, Sources, is_refused};
use crate::config::schema::LimitsConfig;
use crate::provider::ask::{Endpoint, SendRequest};

fn limits() -> LimitsConfig {
    LimitsConfig {
        max_concurrent_turns: 2,
        max_live_sessions: 3,
        max_queue_length: 3,
        max_queue_wait_ms: 1_000,
    }
}

fn endpoint() -> Endpoint {
    Endpoint {
        base_url: "https://provider.test/v4".to_owned(),
        model: "cheap-model".to_owned(),
        credential: "secret".to_owned(),
    }
}

struct TestSources;

impl Sources for TestSources {
    fn project_root(&self) -> &'static str {
        "/projects/demo"
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the trait is async; a stand-in that answers at once still has to match it"
    )]
    async fn read_file(&self, _path: &str) -> std::io::Result<String> {
        Ok("line one\nline two failed\nline three".to_owned())
    }

    fn output_of(&self, id: &str) -> Option<String> {
        (id == "c1").then(|| "3 failed".to_owned())
    }

    fn attachment(&self, _name: &str) -> Option<String> {
        None
    }
}

/// Records what was sent, and answers with whatever the test wants.
struct FakeSender {
    answer: Mutex<Value>,
    status: Mutex<u16>,
    never: bool,
    sent: Mutex<Vec<(String, Value)>>,
}

impl FakeSender {
    fn new(answer: Value) -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(answer),
            status: Mutex::new(200),
            never: false,
            sent: Mutex::new(Vec::new()),
        })
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(json!({})),
            status: Mutex::new(200),
            never: true,
            sent: Mutex::new(Vec::new()),
        })
    }

    fn sent(&self) -> Vec<(String, Value)> {
        self.sent.lock().unwrap().clone()
    }
}

const REPLY: &str = r#"{
  "choices": [{ "message": { "content": "  line two failed  " } }],
  "usage": { "total_tokens": 412 }
}"#;

impl super::super::super::provider::ask::Sender for FakeSender {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the trait is async; a stand-in that answers at once still has to match it"
    )]
    async fn send(
        &self,
        url: String,
        request: SendRequest,
    ) -> Result<(u16, Option<Value>), String> {
        if self.never {
            return Err("network is down".to_owned());
        }
        self.sent
            .lock()
            .unwrap()
            .push((url, serde_json::from_str(&request.body).expect("JSON body")));
        let answer = self.answer.lock().unwrap().clone();
        let status = *self.status.lock().unwrap();
        Ok((status, Some(answer)))
    }
}

/// A clock the scheduler never asks; these tests drive it by hand.
struct NoClock;

impl Clock for NoClock {
    fn now(&self) -> i64 {
        0
    }

    fn set_timeout(
        &self,
        _action: super::super::super::admission::scheduler::Timer,
        _ms: i64,
    ) -> u64 {
        0
    }

    fn clear_timeout(&self, _handle: u64) {}
}

fn delegations(
    per_turn: usize,
    sender: Arc<FakeSender>,
    deadline_ms: u64,
) -> (
    TurnDelegations<TestSources, FakeSender>,
    Arc<Scheduler>,
    Arc<FakeSender>,
) {
    let clock = Arc::new(NoClock);
    let scheduler = Scheduler::start(limits(), clock, 5_000, 300_000, 750);
    let turn = TurnDelegations::new(
        "s-1",
        endpoint(),
        Arc::clone(&scheduler),
        Arc::new(TestSources),
        deadline_ms,
        per_turn,
        Arc::clone(&sender),
    );
    (turn, scheduler, sender)
}

#[tokio::test]
async fn a_delegation_is_asked_of_the_cheap_model_and_answered() {
    let reply = serde_json::from_str::<Value>(REPLY).expect("reply");
    let sender = FakeSender::new(reply);
    let (mut turn, _scheduler, sent) = delegations(2, sender, 5_000);

    let answer = turn
        .run(&json!({ "question": "which test failed?", "path": "out.log" }))
        .await;

    let Outcome::Ready(answer) = answer else {
        panic!("expected an answer");
    };
    assert_eq!(answer.text, "line two failed");
    assert_eq!(answer.model, "cheap-model");
    assert_eq!(answer.describes, "out.log");
    assert_eq!(answer.tokens, Some(412));
    assert_eq!(
        answer.kept_out,
        "line one\nline two failed\nline three".chars().count()
    );
    let calls = sent.sent();
    assert_eq!(calls[0].0, "https://provider.test/v4/chat/completions");
}

/// The delegated model is shown one artefact and told nothing else.
#[tokio::test]
async fn the_request_carries_the_question_and_the_content_and_no_tools() {
    let reply = serde_json::from_str::<Value>(REPLY).expect("reply");
    let sender = FakeSender::new(reply);
    let (mut turn, _scheduler, sent) = delegations(2, sender, 5_000);
    let _ = turn
        .run(&json!({ "question": "which test failed?", "path": "out.log" }))
        .await;

    let calls = sent.sent();
    let body = &calls[0].1;
    let mut keys: Vec<String> = body
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    assert_eq!(keys, vec!["messages".to_owned(), "model".to_owned()]);
    assert_eq!(
        body.get("messages").and_then(Value::as_array).map(Vec::len),
        Some(1)
    );
    let text = body.to_string();
    assert!(text.contains("which test failed?"));
    assert!(text.contains("line two failed"));
    assert!(!text.contains("tool"));
}

#[tokio::test]
async fn a_turn_may_delegate_only_so_many_times() {
    let reply = serde_json::from_str::<Value>(REPLY).expect("reply");
    let sender = FakeSender::new(reply);
    let (mut turn, _scheduler, sender) = delegations(1, sender, 5_000);

    let first = turn.run(&json!({ "question": "q", "path": "a.log" })).await;
    assert!(!is_refused(&first));
    let second = turn.run(&json!({ "question": "q", "path": "b.log" })).await;

    assert!(is_refused(&second));
    match second {
        Outcome::Refused(refused) => {
            assert!(refused.refused.contains("already delegated"));
        }
        Outcome::Ready(_) => panic!("expected a refusal"),
    }
    let _ = sender;
}

/// A malformed request is a mistake, not a spent allowance.
#[tokio::test]
async fn a_refused_request_does_not_spend_the_turns_allowance() {
    let reply = serde_json::from_str::<Value>(REPLY).expect("reply");
    let sender = FakeSender::new(reply);
    let (mut turn, _scheduler, sender) = delegations(1, sender, 5_000);

    let refused = turn.run(&json!({ "question": "nothing to look at" })).await;
    assert!(is_refused(&refused));
    assert_eq!(turn.remaining(), 1);

    let fine = turn.run(&json!({ "question": "q", "path": "a.log" })).await;
    assert!(!is_refused(&fine));
    assert_eq!(turn.remaining(), 0);
    let _ = sender;
}

#[tokio::test]
async fn nothing_is_asked_while_the_provider_is_being_backed_off() {
    let (mut turn, scheduler, sent) =
        delegations(2, FakeSender::new(json!({ "choices": [] })), 5_000);
    scheduler.note_rate_limit();

    let answer = turn.run(&json!({ "question": "q", "path": "a.log" })).await;

    match answer {
        Outcome::Refused(refused) => {
            assert!(refused.refused.contains("backed off"));
        }
        Outcome::Ready(_) => panic!("expected a refusal"),
    }
    assert_eq!(sent.sent().len(), 0);
}

#[tokio::test]
async fn no_free_slot_means_the_work_stays_with_the_sessions_own_model() {
    let (mut turn, scheduler, sent) =
        delegations(2, FakeSender::new(json!({ "choices": [] })), 5_000);
    let _first = scheduler.try_admit();
    let _second = scheduler.try_admit();

    let answer = turn.run(&json!({ "question": "q", "path": "a.log" })).await;

    match answer {
        Outcome::Refused(refused) => {
            assert!(refused.refused.contains("no free slot"));
        }
        Outcome::Ready(_) => panic!("expected a refusal"),
    }
    assert_eq!(sent.sent().len(), 0);
}

#[tokio::test]
async fn the_slot_is_given_back_whether_the_model_answered_or_not() {
    let reply = serde_json::from_str::<Value>(REPLY).expect("reply");
    let sender = FakeSender::new(reply);
    let (mut turn, scheduler, _sent) = delegations(2, sender, 5_000);
    let _ = turn.run(&json!({ "question": "q", "path": "a.log" })).await;
    assert_eq!(scheduler.turns_in_flight(), 0);

    let (mut failing, failing_scheduler, _failing_sent) =
        delegations(2, FakeSender::failing(), 5_000);
    let refused = failing
        .run(&json!({ "question": "q", "path": "a.log" }))
        .await;
    assert!(is_refused(&refused));
    assert_eq!(failing_scheduler.turns_in_flight(), 0);
}

#[tokio::test]
async fn a_provider_refusal_is_reported_not_thrown() {
    let sender = FakeSender::new(json!({ "error": { "message": "rate limited" } }));
    *sender.status.lock().unwrap() = 429;
    let (mut turn, _scheduler, _sent) = delegations(2, sender, 5_000);

    let answer = turn.run(&json!({ "question": "q", "path": "a.log" })).await;

    match answer {
        Outcome::Refused(refused) => {
            assert!(refused.refused.contains("refused the question"));
        }
        Outcome::Ready(_) => panic!("expected a refusal"),
    }
}

#[tokio::test]
async fn an_empty_answer_is_treated_as_no_answer() {
    let (mut turn, _scheduler, _sent) = delegations(
        2,
        FakeSender::new(json!({ "choices": [{ "message": { "content": "   " } }] })),
        5_000,
    );

    let answer = turn.run(&json!({ "question": "q", "path": "a.log" })).await;

    match answer {
        Outcome::Refused(refused) => assert!(refused.refused.contains("no answer")),
        Outcome::Ready(_) => panic!("expected a refusal"),
    }
}

#[tokio::test]
async fn a_delegation_that_never_answers_is_abandoned_rather_than_awaited() {
    let (mut turn, scheduler, _sent) = delegations(2, FakeSender::failing(), 20);

    let answer = turn.run(&json!({ "question": "q", "path": "a.log" })).await;

    // A failing send is refused; the deadline path says the same thing in
    // different words, and either way the slot comes back.
    assert!(is_refused(&answer));
    assert_eq!(scheduler.turns_in_flight(), 0);
}
