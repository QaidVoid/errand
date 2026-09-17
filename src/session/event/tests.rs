use super::*;

fn usage() -> SessionUsage {
    SessionUsage {
        input: 10,
        output: 2,
        cache_read: 90,
        cache_write: 0,
        total_tokens: 102,
        cost: 0.01,
        context_tokens: 100,
        context_window: None,
        turns: 1,
        model: None,
    }
}

#[test]
fn a_prompt_is_written_the_way_the_transcript_writes_it() {
    let event = SessionEvent::Prompt {
        author: "amelia".to_owned(),
        text: "cache the secret".to_owned(),
        id: None,
        withdrawn: false,
    };

    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"call":"prompt","author":"amelia","text":"cache the secret"}"#
    );
}

#[test]
fn a_prompt_keeps_its_id_and_its_withdrawal_when_it_has_them() {
    let event = SessionEvent::Prompt {
        author: "amelia".to_owned(),
        text: String::new(),
        id: Some("m-1".to_owned()),
        withdrawn: true,
    };
    let written = serde_json::to_string(&event).unwrap();

    assert_eq!(
        written,
        r#"{"call":"prompt","author":"amelia","text":"","id":"m-1","withdrawn":true}"#
    );
    assert_eq!(
        serde_json::from_str::<SessionEvent>(&written).unwrap(),
        event
    );
}

#[test]
fn a_prompt_written_before_withdrawal_existed_still_reads() {
    let event: SessionEvent =
        serde_json::from_str(r#"{"call":"prompt","author":"amelia","text":"hello"}"#).unwrap();

    assert_eq!(
        event,
        SessionEvent::Prompt {
            author: "amelia".to_owned(),
            text: "hello".to_owned(),
            id: None,
            withdrawn: false,
        }
    );
}

#[test]
fn usage_omits_what_the_agent_never_said() {
    let event = SessionEvent::Usage { usage: usage() };

    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"call":"usage","usage":{"input":10,"output":2,"cacheRead":90,"cacheWrite":0,"totalTokens":102,"cost":0.01,"contextTokens":100,"turns":1}}"#
    );
}

#[test]
fn a_usage_that_names_the_model_carries_it_and_reads_back() {
    let mut carried = usage();
    carried.context_window = Some(200_000);
    carried.model = Some("glm-5.3".to_owned());
    let written = serde_json::to_string(&SessionEvent::Usage {
        usage: carried.clone(),
    })
    .unwrap();

    assert_eq!(
        serde_json::from_str::<SessionEvent>(&written).unwrap(),
        SessionEvent::Usage { usage: carried }
    );
}

#[test]
fn the_call_names_are_the_ones_the_record_already_uses() {
    let named = |event: &SessionEvent| {
        serde_json::to_value(event)
            .unwrap()
            .get("call")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };

    assert_eq!(
        named(&SessionEvent::ToolResult {
            result: ToolResult {
                id: "t1".to_owned(),
                name: "bash".to_owned(),
                failed: false,
                output: String::new(),
            },
        }),
        "toolResult"
    );
    assert_eq!(named(&SessionEvent::BeginTurn { turn: 1 }), "beginTurn");
    assert_eq!(
        named(&SessionEvent::Post {
            text: String::new()
        }),
        "post"
    );
}

#[test]
fn a_delegated_answer_uses_the_recorded_field_names() {
    let event = SessionEvent::Delegation {
        delegated: Delegated {
            question: "what failed?".to_owned(),
            model: Some("flash".to_owned()),
            tokens: Some(412),
            kept_out: Some(96),
            ..Default::default()
        },
    };

    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"call":"delegation","delegated":{"question":"what failed?","model":"flash","tokens":412,"keptOut":96}}"#
    );
}

#[test]
fn an_activity_carries_its_tool_only_when_it_has_one() {
    let with_tool: SessionEvent = serde_json::from_str(
        r#"{"call":"activity","line":"ran something","tool":{"id":"t1","name":"bash"}}"#,
    )
    .unwrap();

    assert_eq!(
        with_tool,
        SessionEvent::Activity {
            line: "ran something".to_owned(),
            tool: Some(ToolActivity {
                id: Some("t1".to_owned()),
                name: "bash".to_owned(),
                target: None,
                failed: None,
            }),
        }
    );

    let without: SessionEvent =
        serde_json::from_str(r#"{"call":"activity","line":"ran something"}"#).unwrap();
    assert_eq!(
        without,
        SessionEvent::Activity {
            line: "ran something".to_owned(),
            tool: None,
        }
    );
}

#[test]
fn a_notice_and_an_end_reason_keep_their_written_names() {
    let notice = SessionEvent::Notice {
        text: "ready".to_owned(),
        level: NoticeLevel::Started,
    };
    assert_eq!(
        serde_json::to_string(&notice).unwrap(),
        r#"{"call":"notice","text":"ready","level":"started"}"#
    );
    assert_eq!(
        serde_json::from_str::<SessionEvent>(r#"{"call":"notice","text":"done","level":"ended"}"#)
            .unwrap(),
        SessionEvent::Notice {
            text: "done".to_owned(),
            level: NoticeLevel::Ended,
        }
    );

    assert_eq!(
        serde_json::to_string(&EndReason::ThreadArchived).unwrap(),
        r#""thread archived""#
    );
    assert_eq!(
        serde_json::to_string(&EndReason::ProtocolViolation).unwrap(),
        r#""protocol violation""#
    );
    assert_eq!(
        serde_json::from_str::<EndReason>(r#""resource limit""#).unwrap(),
        EndReason::ResourceLimit
    );
}
