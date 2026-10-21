//! Tests for the protocol readers and frame builders.

use serde_json::json;

use super::{
    AcpUpdate, FrameKind, ProducedImage, as_permission_ask, ask_option_ids, classify_frame,
    classify_update, detail_of, stop_failure, tool_target, usage_of,
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
        "_meta": { "kage": { "input": 120, "output": 40, "cacheRead": 700, "cacheWrite": 80 } },
    }))
    .expect("usage");

    assert_eq!(usage.context, 900.0);
    assert_eq!(usage.cache_read, 700.0);
    assert_eq!(usage.total_tokens, 940.0);
    assert_eq!(usage.cost, 0.02);
}

/// An agent that reports only how full the context is has not said what was
/// charged, and the fill must not be read as spend.
#[expect(
    clippy::float_cmp,
    reason = "the fixtures hold values a f64 holds exactly"
)]
#[test]
fn an_update_without_kage_totals_charges_nothing() {
    let usage = usage_of(&json!({ "sessionUpdate": "usage_update", "used": 900, "size": 10 }))
        .expect("usage");

    assert_eq!(usage.context, 900.0);
    assert_eq!(usage.total_tokens, 0.0);
    assert_eq!(usage.input, 0.0);
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
            images: Vec::new(),
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

/// A PNG's signature, and the same as base64.
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const PNG_BASE64: &str = "iVBORw0KGgo=";

/// The images a completed call's update carries, as the session receives them.
fn images_of(update: &serde_json::Value) -> Vec<ProducedImage> {
    match classify_update(update) {
        AcpUpdate::ToolEnd { images, .. } => images,
        other => panic!("not a tool end: {other:?}"),
    }
}

/// A completion in the shape kage sends for a generated image: the text, an
/// `image` block, and the tool's details in `rawOutput`.
fn generated_image_end() -> serde_json::Value {
    json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": "c9", "status": "completed",
        "content": [
            { "type": "content", "content": { "type": "text", "text": "Generated an image" } },
            { "type": "content", "content": {
                "type": "image", "data": PNG_BASE64, "mimeType": "image/png",
            } },
        ],
        "rawOutput": {
            "model": "image", "path": "/workspace/generated/a.png",
            "note": " image:max was rate-limited; this image was generated by image:xhigh ",
        },
    })
}

#[test]
fn a_completed_call_hands_over_the_images_it_returned() {
    let end = generated_image_end();
    let wanted = ProducedImage {
        bytes: PNG.to_vec(),
        note: Some(
            "image:max was rate-limited; this image was generated by image:xhigh".to_owned(),
        ),
        path: Some("/workspace/generated/a.png".to_owned()),
    };
    assert_eq!(images_of(&end), [wanted]);
    match classify_update(&end) {
        AcpUpdate::ToolEnd { output, .. } => assert_eq!(output, "Generated an image"),
        other => panic!("not a tool end: {other:?}"),
    }

    // The block is the image: a `rawOutput` that only names a path, as any
    // tool could, hands over nothing.
    let mut blockless = end;
    blockless["content"].as_array_mut().unwrap().truncate(1);
    assert!(images_of(&blockless).is_empty());
}

#[test]
fn only_decodable_images_of_a_completed_call_are_handed_over() {
    let mut garbled = generated_image_end();
    garbled["content"][1]["content"]["data"] = json!("not base64!");
    assert!(images_of(&garbled).is_empty());

    let mut failed = generated_image_end();
    failed["status"] = json!("failed");
    assert!(images_of(&failed).is_empty());

    let plain = json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": "c1", "status": "completed",
        "content": [{ "type": "content", "content": { "type": "text", "text": "ok" } }],
        "rawOutput": "ok",
    });
    assert!(images_of(&plain).is_empty());
}

#[test]
fn an_image_in_the_reply_is_handed_over_on_its_own() {
    assert_eq!(
        classify_update(&json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "image", "data": "iVBORw0KGgo", "mimeType": "image/png" },
        })),
        AcpUpdate::AgentImage(ProducedImage {
            bytes: PNG.to_vec(),
            note: None,
            path: None,
        })
    );
    assert_eq!(
        classify_update(&json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "image", "data": "a", "mimeType": "image/png" },
        })),
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

/// The handshake asks for nothing of its own: `--yolo` is what keeps an
/// unattended session from asking.
#[test]
fn the_handshake_speaks_protocol_one() {
    let frame = super::initialize_frame(7);
    assert_eq!(frame["id"], 7);
    assert_eq!(frame["params"]["protocolVersion"], 1);
    assert_eq!(frame["params"]["clientCapabilities"], json!({}));
}
