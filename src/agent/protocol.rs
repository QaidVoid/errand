//! The agent protocol: JSON-RPC frames on the wire and the shapes rendering
//! consumes.
//!
//! The agent is `kage rpc`, spoken to over newline-delimited JSON-RPC 2.0 on
//! its stdin and stdout. Requests the daemon sends carry a numeric id;
//! answers repeat it. Server requests (permission asks) arrive with an id and
//! must be answered; `session/update` notifications carry turn progress.

use serde_json::{Value, json};

/// A record read from the agent, before it is classified.
pub type AgentRecord = Value;

/// How a prompt behaves when a turn is already running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingBehavior {
    /// Redirect the running turn.
    #[allow(
        dead_code,
        reason = "the protocol names both spellings; steering has its own command"
    )]
    Steer,
    /// Queue for after the running turn.
    FollowUp,
}

impl StreamingBehavior {
    /// The `session/prompt` delivery, or nothing when the prompt queues.
    pub fn delivery(self) -> Option<&'static str> {
        match self {
            StreamingBehavior::Steer => Some("steer"),
            StreamingBehavior::FollowUp => None,
        }
    }
}

/// An image a chat message carried, as it is handed to the agent.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentImage {
    /// Always `image`; the field mirrors the content-block shape.
    pub r#type: String,
    /// The image bytes, base64 encoded.
    pub data: String,
    /// What the chat service said the bytes are, such as `image/png`.
    pub mime_type: String,
}

/// A dialog request the agent is waiting on.
#[derive(Debug, Clone, PartialEq)]
pub struct DialogRequest {
    /// The request id, which the reply names.
    pub id: String,
    /// What kind of answer is wanted.
    pub method: DialogMethod,
    /// The question, shown as the dialog's title.
    pub title: String,
    /// Anything said below the title, or nothing.
    pub message: Option<String>,
    /// The choices, for a select.
    pub options: Option<Vec<String>>,
    /// The placeholder a text surface shows, or nothing.
    pub placeholder: Option<String>,
    /// The prefill a text surface starts from, or nothing.
    pub prefill: Option<String>,
}

/// The kinds of dialog the agent can block a turn on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogMethod {
    /// Pick one of a list.
    Select,
    /// Yes or no.
    Confirm,
    /// Free text. None arrive over this protocol; the thread still renders
    /// them, so the shape stays.
    #[allow(
        dead_code,
        reason = "no input dialogs arrive over ACP; the thread still renders them"
    )]
    Input,
    /// A surface this chat cannot serve; always answered by cancellation.
    Editor,
}

impl DialogMethod {
    /// The method as threads and logs name it.
    pub fn as_str(self) -> &'static str {
        match self {
            DialogMethod::Select => "select",
            DialogMethod::Confirm => "confirm",
            DialogMethod::Input => "input",
            DialogMethod::Editor => "editor",
        }
    }
}

/// One JSON-RPC frame, classified by which keys it carries.
#[derive(Debug, Clone, PartialEq)]
pub enum FrameKind {
    /// A server request: an id, a method, and params. Answer it.
    Request { id: u64, method: String },
    /// A server notification: a method and params. No answer is wanted.
    Notification { method: String },
    /// An answer to a request this client sent.
    Success { id: u64 },
    /// A refusal of a request this client sent.
    Failure { id: u64 },
    /// Neither a frame shape nor anything worth answering.
    Unknown,
}

/// Sorts one parsed line into its frame shape.
///
/// Ids are numbers on this wire; a string id is not a frame the agent speaks,
/// so it reads as unknown rather than as somebody's answer.
pub fn classify_frame(record: &Value) -> FrameKind {
    let id = record.get("id").and_then(Value::as_u64);
    let method = record.get("method").and_then(Value::as_str);
    match (id, method) {
        (Some(id), Some(method)) => FrameKind::Request {
            id,
            method: method.to_owned(),
        },
        (None, Some(method)) => FrameKind::Notification {
            method: method.to_owned(),
        },
        (Some(id), None) if record.get("result").is_some() => FrameKind::Success { id },
        (Some(id), None) if record.get("error").is_some() => FrameKind::Failure { id },
        _ => FrameKind::Unknown,
    }
}

/// The `initialize` handshake: protocol version 1, like every other
/// client, with the capability that says the session runs unattended.
/// A tool with no `[permissions]` rule then runs instead of asking,
/// which a chat thread could not answer anyway.
pub fn initialize_frame(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": 1,
            "clientCapabilities": {
                "_meta": { "kage": { "unconfiguredTools": "allow" } },
            },
        },
    })
}

/// Opens the agent's session in `cwd`, with no MCP servers attached.
pub fn new_session_frame(id: u64, cwd: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/new",
        "params": { "cwd": cwd, "mcpServers": [] },
    })
}

/// Reopens a session the agent recorded earlier, continuing its history.
pub fn resume_session_frame(id: u64, session_id: &str, cwd: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/resume",
        "params": { "sessionId": session_id, "cwd": cwd, "mcpServers": [] },
    })
}

/// One text block, the only prompt content this chat sends.
pub fn text_block(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

/// An image block in the shape the agent reads.
pub fn image_block(data: &str, mime_type: &str) -> Value {
    json!({ "type": "image", "data": data, "mimeType": mime_type })
}

/// Runs one prompt on the session, steering when `delivery` says so.
pub fn prompt_frame(id: u64, session_id: &str, blocks: &[Value], delivery: Option<&str>) -> Value {
    let mut params = json!({
        "sessionId": session_id,
        "prompt": blocks,
    });
    if let Some(delivery) = delivery {
        params["delivery"] = Value::String(delivery.to_owned());
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/prompt",
        "params": params,
    })
}

/// Stops the run in flight. A notification: no answer comes back.
pub fn cancel_frame(session_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": { "sessionId": session_id },
    })
}

/// Changes one session option, such as the model.
pub fn set_config_option_frame(id: u64, session_id: &str, key: &str, value: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/set_config_option",
        "params": {
            "sessionId": session_id,
            "configId": key,
            "value": value,
        },
    })
}

/// Asks the agent to summarise the conversation so far.
pub fn compact_frame(id: u64, session_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "_kage/session/compact",
        "params": { "sessionId": session_id },
    })
}

/// Answers a permission ask by picking one offered option.
pub fn select_option_frame(id: u64, option_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "outcome": { "outcome": "selected", "optionId": option_id } },
    })
}

/// Answers a permission ask by cancelling the tool call.
pub fn cancel_ask_frame(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "outcome": { "outcome": "cancelled" } },
    })
}

/// Refuses a server request this client does not serve, so the agent is never
/// left waiting on an answer that will not come.
pub fn method_not_found_frame(id: u64, method: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32601, "message": format!("method not found: {method}") },
    })
}

/// One `session/update` notification, classified for the turn it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub enum AcpUpdate {
    /// Assistant text, to report as it arrives.
    AgentText(String),
    /// Reasoning text, to report as it arrives.
    ThoughtText(String),
    /// A tool call started, with its raw input for the target line.
    ToolStart {
        id: String,
        title: String,
        raw_input: Option<Value>,
    },
    /// A tool call began running, its input now complete. The start line
    /// waits for this: a streaming call names nothing before it.
    ToolExecute {
        id: String,
        raw_input: Option<Value>,
    },
    /// A tool call finished, with whether it failed and what it said.
    ToolEnd {
        id: String,
        title: Option<String>,
        failed: bool,
        output: String,
    },
    /// Context usage, for the usage line and the window size.
    UsageInfo { usage: Usage, size: f64 },
    /// The running prompt began.
    TurnStart,
    /// The running prompt ended.
    TurnEnd,
    /// Older turns were summarised, with tokens before and after.
    CompactionInfo { before: f64, after: f64 },
    /// A message for the user outside the conversation.
    Notice(String),
    /// Anything else: plans, modes, notices, subagent and swarm traffic, and
    /// shapes this build does not know. Subagents arrive in a later change.
    Ignored,
}

/// Reads the text of a content block, or nothing when it carries no words.
fn block_text(block: &Value) -> Option<String> {
    if block.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    block.get("text").and_then(Value::as_str).map(str::to_owned)
}

/// Reads the text carried by tool output content: text chunks in order, with
/// diffs named rather than pasted.
fn tool_output_text(content: Option<&Value>) -> String {
    let Some(parts) = content.and_then(Value::as_array) else {
        return String::new();
    };
    let mut out = String::new();
    for part in parts {
        if let Some(text) = part.get("content").and_then(|chunk| {
            if chunk.get("type").and_then(Value::as_str) == Some("text") {
                chunk.get("text").and_then(Value::as_str)
            } else {
                None
            }
        }) {
            out.push_str(text);
            continue;
        }
        if let Some(text) = block_text(part) {
            out.push_str(&text);
            continue;
        }
        if part.get("sessionUpdate").and_then(Value::as_str) == Some("diff")
            && let Some(path) = part.get("path").and_then(Value::as_str)
        {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("[diff {path}]"));
        }
    }
    out
}

/// Classifies one `session/update` notification body.
pub fn classify_update(update: &Value) -> AcpUpdate {
    let kind = update.get("sessionUpdate").and_then(Value::as_str);
    match kind {
        Some("agent_message_chunk") => update
            .get("content")
            .and_then(block_text)
            .filter(|text| !text.trim().is_empty())
            .map_or(AcpUpdate::Ignored, AcpUpdate::AgentText),
        Some("agent_thought_chunk") => update
            .get("content")
            .and_then(block_text)
            .filter(|text| !text.trim().is_empty())
            .map_or(AcpUpdate::Ignored, AcpUpdate::ThoughtText),
        Some("tool_call") => {
            let id = update
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if id.is_empty() {
                return AcpUpdate::Ignored;
            }
            AcpUpdate::ToolStart {
                id,
                title: update
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_owned(),
                raw_input: update.get("rawInput").cloned(),
            }
        }
        Some("tool_call_update") => {
            let id = update
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if id.is_empty() {
                return AcpUpdate::Ignored;
            }
            let status = update.get("status").and_then(Value::as_str);
            if status == Some("in_progress") {
                return AcpUpdate::ToolExecute {
                    id,
                    raw_input: update.get("rawInput").cloned(),
                };
            }
            let finished = matches!(status, Some("completed" | "failed"));
            if !finished {
                return AcpUpdate::Ignored;
            }
            let failed = status == Some("failed");
            AcpUpdate::ToolEnd {
                id,
                title: update
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                failed,
                output: tool_output_text(update.get("content"))
                    + update
                        .get("rawOutput")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
            }
        }
        Some("usage_update") => match usage_of(update) {
            Some(usage) => {
                let size = update.get("size").and_then(Value::as_f64).unwrap_or(0.0);
                AcpUpdate::UsageInfo { usage, size }
            }
            None => AcpUpdate::Ignored,
        },
        Some("_kage/turn") => match update.get("phase").and_then(Value::as_str) {
            Some("start") => AcpUpdate::TurnStart,
            Some("end") => AcpUpdate::TurnEnd,
            _ => AcpUpdate::Ignored,
        },
        Some("_kage/compaction") => {
            let before = update.get("before").and_then(Value::as_f64);
            let after = update.get("after").and_then(Value::as_f64);
            match (before, after) {
                (Some(before), Some(after)) => AcpUpdate::CompactionInfo { before, after },
                _ => AcpUpdate::Ignored,
            }
        }
        Some("_kage/notice") => update
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map_or(AcpUpdate::Ignored, |text| {
                AcpUpdate::Notice(text.to_owned())
            }),
        _ => AcpUpdate::Ignored,
    }
}

/// A permission ask as a dialog the thread can answer.
///
/// Every ask becomes a select over the offered option names: the thread
/// already answers selects by number or by name, and the chosen name maps
/// back to its option id on reply. Only options carrying both a name and an
/// option id are offered, so every offered answer has a reply. A plan review
/// carries a document rather than options and has no thread equivalent, so it
/// reads as nothing here and the caller takes the unsupported path.
pub fn as_permission_ask(request_id: u64, params: &Value) -> Option<DialogRequest> {
    if params
        .get("_meta")
        .and_then(|meta| meta.get("kage"))
        .and_then(|kage| kage.get("planReview"))
        .is_some()
    {
        return None;
    }
    let call = params.get("toolCall")?;
    let title = call
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("the agent wants to run a tool")
        .to_owned();
    let message = subject_of(call).map(|subject| format!("about {subject}"));
    let options = params.get("options").and_then(Value::as_array)?;
    let names: Vec<String> = options
        .iter()
        .filter(|option| {
            option.get("name").and_then(Value::as_str).is_some()
                && option.get("optionId").and_then(Value::as_str).is_some()
        })
        .filter_map(|option| option.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    if names.is_empty() {
        return None;
    }
    Some(DialogRequest {
        id: request_id.to_string(),
        method: DialogMethod::Select,
        title,
        message,
        options: Some(names),
        placeholder: None,
        prefill: None,
    })
}

/// The option ids behind a permission ask, in the same order as its names.
///
/// Only options carrying both a name and an option id are listed, matching
/// [`as_permission_ask`], so zipping the two answers the right option.
pub fn ask_option_ids(params: &Value) -> Vec<String> {
    params
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter(|option| {
                    option.get("name").and_then(Value::as_str).is_some()
                        && option.get("optionId").and_then(Value::as_str).is_some()
                })
                .filter_map(|option| option.get("optionId").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The primary argument of a tool call, for the dialog's message line.
fn subject_of(call: &Value) -> Option<String> {
    let input = call.get("rawInput")?;
    tool_target(Some(input))
}

/// Pulls a readable target out of a tool call's arguments, when there is one.
pub fn tool_target(args: Option<&Value>) -> Option<String> {
    let record = args?.as_object()?;
    for key in [
        "command",
        "path",
        "file_path",
        "filePath",
        "pattern",
        "query",
        "url",
    ] {
        if let Some(Value::String(value)) = record.get(key)
            && !value.trim().is_empty()
        {
            return Some(value.trim().to_owned());
        }
    }
    None
}

/// What a turn cost, as the agent reports it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Usage {
    /// Input tokens charged uncached.
    pub input: f64,
    /// Output tokens.
    pub output: f64,
    /// Input tokens served from the provider's cache.
    pub cache_read: f64,
    /// Input tokens written to the provider's cache.
    pub cache_write: f64,
    /// Everything the turn charged.
    pub total_tokens: f64,
    /// What the turn cost.
    pub cost: f64,
    /// The model that charged it, when the agent named it.
    pub model: Option<String>,
}

/// Reads usage off a `usage_update` body, when it carries any.
///
/// The update reports context fill rather than per-kind tokens, so the fill
/// lands on input and the total: the usage line renders what was charged out
/// of what the window holds, which is what those two numbers say.
pub fn usage_of(record: &Value) -> Option<Usage> {
    let used = record.get("used")?.as_f64()?;
    let cost = record
        .get("cost")
        .and_then(|cost| cost.get("amount"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    Some(Usage {
        input: used,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
        total_tokens: used,
        cost,
        model: None,
    })
}

/// Why a command failed, in one line.
pub fn detail_of(record: &Value) -> String {
    if let Some(error) = record.get("error")
        && let Some(message) = error.get("message").and_then(Value::as_str)
        && !message.is_empty()
    {
        return message.to_owned();
    }
    for key in ["errorMessage", "error", "message", "reason"] {
        if let Some(Value::String(detail)) = record.get(key)
            && !detail.is_empty()
        {
            return detail.clone();
        }
    }
    record
        .get("method")
        .and_then(Value::as_str)
        .or_else(|| record.get("type").and_then(Value::as_str))
        .unwrap_or("unknown")
        .to_owned()
}

/// True when a failure means the provider is busy rather than refusing: the
/// turn may still settle on its own, so the caller backs off instead of
/// settling it as dead.
pub fn is_retryable(detail: &str) -> bool {
    let lowered = detail.to_lowercase();
    [
        "429",
        "503",
        "529",
        "rate limit",
        "rate_limit",
        "overloaded",
        "retry",
        "try again",
        "temporar",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
}

/// Why a prompt turn ended, for the settled report.
pub fn stop_failure(record: &Value) -> Option<String> {
    let reason = record
        .get("stopReason")
        .and_then(Value::as_str)
        .or_else(|| record.get("stop_reason").and_then(Value::as_str))?;
    match reason {
        "end_turn" | "max_tokens" | "max_turn_requests" => None,
        "cancelled" => Some("the turn was cancelled".to_owned()),
        "refusal" => Some("the model refused".to_owned()),
        _ => Some(format!("the turn stopped: {reason}")),
    }
}

#[cfg(test)]
mod tests;
