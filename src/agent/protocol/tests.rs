//! Tests for the protocol readers, ported from `protocol_test.ts`.

use serde_json::json;

use super::{
    as_dialog_request, is_fire_and_forget, message_role, message_text, starts_thinking,
    thinking_ended, tool_target, usage_of,
};

#[expect(
    clippy::float_cmp,
    reason = "the fixtures hold values a f64 holds exactly"
)]
#[test]
fn usage_is_read_off_a_record_counting_cache_reads_apart_from_input() {
    let usage = usage_of(&json!({
        "message": {
            "model": "glm-5.3",
            "usage": { "input": 900, "output": 120, "cacheRead": 8000,
                       "cacheWrite": 0, "totalTokens": 9020 },
        },
    }))
    .expect("usage");

    assert_eq!(usage.input, 900.0);
    assert_eq!(usage.cache_read, 8000.0);
    assert_eq!(usage.model.as_deref(), Some("glm-5.3"));
}

#[test]
fn a_record_with_no_usage_reports_none_rather_than_zeroes() {
    assert_eq!(usage_of(&json!({ "type": "agent_settled" })), None);
}

#[test]
fn a_dialog_the_agent_waits_on_is_recognised_with_its_options() {
    let request = as_dialog_request(&json!({
        "type": "extension_ui_request",
        "id": "d-1",
        "method": "select",
        "title": "Which branch?",
        "options": ["main", "dev", 7],
    }))
    .expect("a dialog");

    assert_eq!(request.method.as_str(), "select");
    assert_eq!(request.title, "Which branch?");
    assert_eq!(
        request.options,
        Some(vec!["main".to_owned(), "dev".to_owned()])
    );
}

/// Answering one of these would be answering a question nobody asked.
#[test]
fn an_informational_request_is_not_a_dialog() {
    assert!(is_fire_and_forget(Some("notify")));
    assert!(!is_fire_and_forget(Some("select")));
    assert_eq!(
        as_dialog_request(&json!({
            "type": "extension_ui_request", "id": "n-1", "method": "notify",
        })),
        None
    );
}

#[test]
fn a_record_that_is_not_a_dialog_request_is_not_mistaken_for_one() {
    assert_eq!(as_dialog_request(&json!({ "type": "agent_settled" })), None);
    assert_eq!(
        as_dialog_request(&json!({ "type": "extension_ui_request", "method": "select" })),
        None
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
fn the_text_of_a_message_is_its_text_parts_in_order() {
    let text = message_text(Some(&json!({
        "content": [
            { "type": "text", "text": "first " },
            { "type": "tool_use", "id": "t1" },
            { "type": "text", "text": "second" },
        ],
    })));

    assert_eq!(text, "first second");
    assert_eq!(message_text(Some(&json!({ "content": [] }))), "");
    assert_eq!(message_text(None), "");
}

#[test]
fn thinking_is_read_from_the_event_which_arrives_before_any_message() {
    assert!(starts_thinking(&json!({
        "assistantMessageEvent": { "type": "thinking_start" },
    })));
    assert!(!starts_thinking(&json!({
        "assistantMessageEvent": { "type": "text_delta" },
    })));
    assert!(!starts_thinking(&json!({ "type": "agent_settled" })));

    assert_eq!(
        thinking_ended(&json!({
            "assistantMessageEvent": { "type": "thinking_end", "content": "weighed it" },
        })),
        Some("weighed it".to_owned())
    );
    assert_eq!(
        thinking_ended(&json!({
            "assistantMessageEvent": { "type": "thinking_start" },
        })),
        None
    );
}

#[test]
fn the_role_of_a_message_is_read_when_it_has_one() {
    assert_eq!(
        message_role(Some(&json!({ "role": "assistant" }))),
        Some("assistant".to_owned())
    );
    assert_eq!(message_role(Some(&json!({}))), None);
    assert_eq!(message_role(None), None);
}
