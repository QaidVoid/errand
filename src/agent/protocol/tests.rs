//! Tests for the protocol readers and frame builders.

use serde_json::json;

use super::{
    AcpUpdate, FrameKind, as_permission_ask, ask_option_ids, classify_frame, classify_update,
    detail_of, stop_failure, tool_target, usage_of,
};

#[expect(
    clippy::float_cmp,
    reason = "the fixtures hold values a f64 holds exactly"
)]
#[test]
fn usage_is_read_off_an_update_with_cost_beside_it() {
    let usage = usage_of(&json!({
        "sessionUpdate": "usage_update",
        "used": 900,
        "size": 200_000,
        "cost": { "amount": 0.02, "currency": "USD" },
    }))
    .expect("usage");

    assert_eq!(usage.input, 900.0);
    assert_eq!(usage.total_tokens, 900.0);
    assert_eq!(usage.cost, 0.02);
    assert_eq!(usage.output, 0.0);
}

#[test]
fn an_update_with_no_used_reports_none_rather_than_zeroes() {
    assert_eq!(usage_of(&json!({ "sessionUpdate": "usage_update" })), None);
    assert_eq!(
        usage_of(&json!({ "sessionUpdate": "agent_message_chunk" })),
        None
    );
}

#[test]
fn frames_sort_into_requests_notifications_answers_and_unknowns() {
    assert_eq!(
        classify_frame(&json!({
            "jsonrpc": "2.0", "id": 7,
            "method": "session/request_permission", "params": {},
        })),
        FrameKind::Request {
            id: 7,
            method: "session/request_permission".to_owned(),
        }
    );
    assert_eq!(
        classify_frame(&json!({
            "jsonrpc": "2.0",
            "method": "session/update", "params": {},
        })),
        FrameKind::Notification {
            method: "session/update".to_owned(),
        }
    );
    assert_eq!(
        classify_frame(&json!({ "jsonrpc": "2.0", "id": 3, "result": {} })),
        FrameKind::Success { id: 3 }
    );
    assert_eq!(
        classify_frame(&json!({
            "jsonrpc": "2.0", "id": 4, "error": { "code": -32601, "message": "no" },
        })),
        FrameKind::Failure { id: 4 }
    );
    assert_eq!(
        classify_frame(&json!({ "jsonrpc": "2.0", "id": "rq-1", "result": {} })),
        FrameKind::Unknown
    );
    assert_eq!(
        classify_frame(&json!({ "jsonrpc": "2.0" })),
        FrameKind::Unknown
    );
}

#[test]
fn a_permission_ask_becomes_a_select_over_named_options_with_ids() {
    let params = json!({
        "sessionId": "s1",
        "toolCall": { "toolCallId": "c1", "title": "rm", "rawInput": { "command": "rm -rf /tmp/x" } },
        "options": [
            { "optionId": "allow", "name": "Allow", "kind": "allowOnce" },
            { "optionId": "deny", "name": "Deny", "kind": "rejectOnce" },
            { "name": "Nameless" },
        ],
    });
    let request = as_permission_ask(9, &params).expect("a dialog");

    assert_eq!(request.id, "9");
    assert_eq!(request.method.as_str(), "select");
    assert_eq!(
        request.options,
        Some(vec!["Allow".to_owned(), "Deny".to_owned()])
    );
    assert_eq!(
        ask_option_ids(&params),
        vec!["allow".to_owned(), "deny".to_owned()]
    );
}

#[test]
fn a_plan_review_and_an_optionless_ask_are_not_dialogs() {
    assert_eq!(
        as_permission_ask(
            1,
            &json!({
                "sessionId": "s1",
                "toolCall": { "toolCallId": "c1" },
                "options": [{ "optionId": "a", "name": "A", "kind": "allowOnce" }],
                "_meta": { "kage": { "planReview": { "plan": "do it" } } },
            })
        ),
        None
    );
    assert_eq!(
        as_permission_ask(
            2,
            &json!({
                "sessionId": "s1",
                "toolCall": { "toolCallId": "c1" },
                "options": [],
            })
        ),
        None
    );
}

#[test]
fn updates_sort_into_text_thoughts_tools_usage_turns_and_compactions() {
    assert_eq!(
        classify_update(&json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "hello" },
        })),
        AcpUpdate::AgentText("hello".to_owned())
    );
    assert_eq!(
        classify_update(&json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": { "type": "text", "text": "hmm" },
        })),
        AcpUpdate::ThoughtText("hmm".to_owned())
    );
    assert_eq!(
        classify_update(&json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "   " },
        })),
        AcpUpdate::Ignored
    );
    assert_eq!(
        classify_update(&json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "c1", "title": "shell",
            "rawInput": { "command": "ls" },
        })),
        AcpUpdate::ToolStart {
            id: "c1".to_owned(),
            title: "shell".to_owned(),
            raw_input: Some(json!({ "command": "ls" })),
        }
    );
    assert_eq!(
        classify_update(&json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "c1", "status": "inProgress",
        })),
        AcpUpdate::Ignored
    );
    assert_eq!(
        classify_update(&json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "c1", "status": "failed",
            "content": [{ "type": "text", "text": "nope" }],
        })),
        AcpUpdate::ToolEnd {
            id: "c1".to_owned(),
            title: None,
            failed: true,
            output: "nope".to_owned(),
        }
    );
    assert_eq!(
        classify_update(&json!({
            "sessionUpdate": "_kage/turn", "phase": "start",
        })),
        AcpUpdate::TurnStart
    );
    assert_eq!(
        classify_update(&json!({
            "sessionUpdate": "_kage/turn", "phase": "end",
        })),
        AcpUpdate::TurnEnd
    );
    assert_eq!(
        classify_update(&json!({
            "sessionUpdate": "_kage/compaction",
            "kept": 2, "before": 9000, "after": 3000,
        })),
        AcpUpdate::CompactionInfo {
            before: 9000.0,
            after: 3000.0,
        }
    );
    assert_eq!(
        classify_update(&json!({ "sessionUpdate": "plan" })),
        AcpUpdate::Ignored
    );
    assert_eq!(
        classify_update(&json!({ "sessionUpdate": "subagent_update" })),
        AcpUpdate::Ignored
    );
}

#[test]
fn a_tool_call_is_named_by_the_argument_a_reader_would_recognise() {
    let read = |args| tool_target(Some(&args));
    assert_eq!(
        read(json!({ "command": "ls -la" })),
        Some("ls -la".to_owned())
    );
    assert_eq!(
        read(json!({ "file_path": "/workspace/main.ts" })),
        Some("/workspace/main.ts".to_owned())
    );
    assert_eq!(read(json!({ "pattern": "TODO" })), Some("TODO".to_owned()));
    assert_eq!(read(json!({ "unrelated": "x" })), None);
    assert_eq!(read(json!({ "command": "   " })), None);
    assert_eq!(tool_target(Some(&json!("not an object"))), None);
    assert_eq!(tool_target(None), None);
}

#[test]
fn failures_name_the_error_then_the_method() {
    assert_eq!(
        detail_of(&json!({ "error": { "code": -32601, "message": "no such session" } })),
        "no such session".to_owned()
    );
    assert_eq!(
        detail_of(&json!({ "method": "session/prompt" })),
        "session/prompt".to_owned()
    );
}

#[test]
fn only_a_bad_stop_reason_fails_the_turn() {
    assert_eq!(stop_failure(&json!({ "stopReason": "end_turn" })), None);
    assert_eq!(stop_failure(&json!({ "stopReason": "max_tokens" })), None);
    assert_eq!(
        stop_failure(&json!({ "stopReason": "cancelled" })),
        Some("the turn was cancelled".to_owned())
    );
    assert_eq!(
        stop_failure(&json!({ "stopReason": "refusal" })),
        Some("the model refused".to_owned())
    );
    assert_eq!(stop_failure(&json!({})), None);
}
