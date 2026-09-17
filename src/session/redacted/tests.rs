use super::redact_event;
use crate::config::redact::REDACTION;
use crate::session::event::{
    Delegated, EndReason, NoticeLevel, ReactionOutcome, SessionEvent, SessionUsage, ToolActivity,
    ToolResult,
};

const SECRET: &str = "sk-live-9f3c7a11d4e6";

fn usage() -> SessionUsage {
    SessionUsage {
        input: 1,
        output: 1,
        cache_read: 0,
        cache_write: 0,
        total_tokens: 2,
        cost: 0.0,
        context_tokens: 1,
        context_window: None,
        turns: 1,
        model: None,
    }
}

/// Every way a session reports something, with the secret in every string the
/// agent can influence.
///
/// A message id is not one of them: it is minted by the chat service and never
/// carries agent output, and neither does a turn number, a reaction, or a
/// usage total.
fn calls() -> Vec<SessionEvent> {
    let with_secret = |text: &str| format!("{text}{SECRET}");
    vec![
        SessionEvent::Post {
            text: with_secret("the key is "),
        },
        SessionEvent::Notice {
            text: with_secret("starting with "),
            level: NoticeLevel::Started,
        },
        SessionEvent::Reply {
            text: with_secret("output "),
            command: with_secret("!cat "),
        },
        SessionEvent::ToolResult {
            result: ToolResult {
                id: "1".to_owned(),
                name: "bash".to_owned(),
                failed: false,
                output: with_secret("env: "),
            },
        },
        SessionEvent::Delegation {
            delegated: Delegated {
                question: with_secret("what is "),
                model: Some("flash".to_owned()),
                answer: Some(with_secret("it is ")),
                ..Default::default()
            },
        },
        SessionEvent::BeginTurn { turn: 3 },
        SessionEvent::Thinking {
            text: with_secret("I should use "),
        },
        SessionEvent::Prompt {
            author: "amelia".to_owned(),
            text: with_secret("use "),
            id: None,
            withdrawn: false,
        },
        SessionEvent::Aside {
            author: "amelia".to_owned(),
            text: with_secret("never mind "),
            id: None,
            withdrawn: false,
        },
        SessionEvent::Activity {
            line: with_secret("ran "),
            tool: Some(ToolActivity {
                id: None,
                name: "bash".to_owned(),
                target: with_secret("echo ").into(),
                failed: None,
            }),
        },
        SessionEvent::Diff {
            path: format!("{SECRET}.ts"),
            added: 1,
            removed: 0,
            body: with_secret("+ "),
            cause: with_secret("write ").into(),
        },
        SessionEvent::Waiting {
            text: with_secret("waiting on ").into(),
        },
        SessionEvent::Reaction {
            message_id: "123456789".to_owned(),
            outcome: ReactionOutcome::Succeeded,
        },
        SessionEvent::Usage { usage: usage() },
        SessionEvent::Busy { busy: true },
        SessionEvent::Upload {
            name: format!("{SECRET}.txt"),
            bytes: vec![1, 2],
            caption: with_secret("here is "),
        },
        SessionEvent::Close {
            reason: EndReason::Idle,
        },
    ]
}

#[test]
fn nothing_a_session_reports_carries_the_credential_through() {
    let secrets = [SECRET.to_owned()];

    let all: Vec<String> = calls()
        .into_iter()
        .map(|event| {
            serde_json::to_string(&redact_event(event, &secrets)).expect("an event serializes")
        })
        .collect();
    let all = all.join("\n");

    assert!(!all.contains(SECRET), "{all}");
    assert!(all.contains(REDACTION));
}

/// The point of the table above. A field added to an event and forgotten here
/// would otherwise be scrubbed by nobody and tested by nobody.
#[test]
fn every_way_of_reporting_something_is_covered() {
    let secrets = [SECRET.to_owned()];

    let covered = calls()
        .into_iter()
        .map(|event| redact_event(event, &secrets))
        .collect::<Vec<_>>();

    assert_eq!(covered.len(), 17);
    // The fields a report is made of are all present in the table, so the
    // redacted forms scrub what was secret and the untouched forms are the
    // frame: id, level, outcome, usage, and the rest.
    assert!(covered.iter().any(|event| matches!(
        event,
        SessionEvent::ToolResult { result } if result.output.contains(REDACTION)
    )));
    assert!(
        covered
            .iter()
            .any(|event| matches!(event, SessionEvent::Usage { .. }))
    );
}

/// With nothing to scrub the pass through the table is not worth the work.
#[test]
fn with_no_secrets_everything_is_passed_through_untouched() {
    for event in calls() {
        assert_eq!(redact_event(event.clone(), &[]), event);
    }
}

#[test]
fn what_is_reported_is_otherwise_unchanged() {
    let secrets = [SECRET.to_owned()];

    let said = redact_event(
        SessionEvent::Post {
            text: "nothing secret here".to_owned(),
        },
        &secrets,
    );
    assert_eq!(
        said,
        SessionEvent::Post {
            text: "nothing secret here".to_owned()
        }
    );

    let busy = redact_event(SessionEvent::Busy { busy: true }, &secrets);
    assert_eq!(busy, SessionEvent::Busy { busy: true });
}

/// The house rules reach the agent as a file it can read, so a session that
/// reads its own prompt back must not put the operator's rules in the channel.
/// Everything else in the same output still goes through.
#[test]
fn the_operators_house_rules_are_scrubbed_out_of_tool_output() {
    let rules = "You are Talaria, the errand-runner.\n\nNever push unless asked.";
    let secrets = [SECRET.to_owned(), rules.to_owned()];

    let reported = redact_event(
        SessionEvent::ToolResult {
            result: ToolResult {
                id: "1".to_owned(),
                name: "bash".to_owned(),
                failed: false,
                output: format!(
                    "$ cat /state/memory.md\n## House rules\n\n{rules}\n\ntotal 4 files"
                ),
            },
        },
        &secrets,
    );

    let said = serde_json::to_string(&reported).expect("an event serializes");
    assert!(!said.contains("errand-runner"));
    assert!(!said.contains("Never push unless asked"));
    assert!(said.contains(REDACTION));
    // The tool result itself still reports; only the rules went.
    assert!(said.contains("total 4 files"));
    assert!(said.contains("cat /state/memory.md"));
}
