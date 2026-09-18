//! The agent protocol module. The command framing and the rest of the types
//! arrive with the agent layer; the shapes here now are the ones rendering
//! already consumes.

use serde_json::Value;

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
    /// The spelling the protocol uses on the wire.
    pub fn as_wire(self) -> &'static str {
        match self {
            StreamingBehavior::Steer => "steer",
            StreamingBehavior::FollowUp => "followUp",
        }
    }
}

/// An image a chat message carried, as it is handed to a describing model.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentImage {
    /// Always `image`; the field mirrors the protocol's JSON shape.
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
    /// Free text.
    Input,
    /// Free text in an editor surface, always answered by cancellation here.
    Editor,
}

impl DialogMethod {
    /// The method as the protocol spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            DialogMethod::Select => "select",
            DialogMethod::Confirm => "confirm",
            DialogMethod::Input => "input",
            DialogMethod::Editor => "editor",
        }
    }
}

/// True when a UI request expects a response rather than being informational.
pub fn is_dialog_method(method: Option<&str>) -> bool {
    matches!(method, Some("select" | "confirm" | "input" | "editor"))
}

/// True when a UI request is informational and must not be answered.
pub fn is_fire_and_forget(method: Option<&str>) -> bool {
    matches!(
        method,
        Some("notify" | "setStatus" | "setWidget" | "setTitle" | "set_editor_text")
    )
}

fn text_of(record: &Value, key: &str) -> Option<String> {
    record.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Reads a dialog request out of a raw record, or nothing when it is not one.
pub fn as_dialog_request(record: &Value) -> Option<DialogRequest> {
    if record.get("type").and_then(Value::as_str) != Some("extension_ui_request") {
        return None;
    }
    let id = text_of(record, "id")?;
    let method = record.get("method").and_then(Value::as_str);
    if !is_dialog_method(method) {
        return None;
    }

    let options = record
        .get("options")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });

    let method = match method {
        Some("select") => DialogMethod::Select,
        Some("confirm") => DialogMethod::Confirm,
        Some("editor") => DialogMethod::Editor,
        _ => DialogMethod::Input,
    };
    Some(DialogRequest {
        id,
        method,
        title: text_of(record, "title").unwrap_or_default(),
        message: text_of(record, "message"),
        options,
        placeholder: text_of(record, "placeholder"),
        prefill: text_of(record, "prefill"),
    })
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

/// Concatenates the text parts of an agent message's content.
pub fn message_text(message: Option<&Value>) -> String {
    let Some(content) = message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return String::new();
    };
    let text: String = content
        .iter()
        .filter(|part| {
            part.get("type").and_then(Value::as_str) == Some("text")
                && part.get("text").is_some_and(Value::is_string)
        })
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    text
}

/// True when a streaming update reports the agent starting to think.
///
/// The update carries an `assistantMessageEvent`, not a message: the deltas
/// arrive before any message exists to inspect.
pub fn starts_thinking(record: &Value) -> bool {
    record
        .get("assistantMessageEvent")
        .and_then(|event| event.get("type"))
        .and_then(Value::as_str)
        == Some("thinking_start")
}

/// The agent's reasoning for a block that has just finished, if this is one.
///
/// The end event carries the whole block, so there is no need to accumulate
/// the deltas that led to it.
pub fn thinking_ended(record: &Value) -> Option<String> {
    let event = record.get("assistantMessageEvent")?;
    if event.get("type").and_then(Value::as_str) != Some("thinking_end") {
        return None;
    }
    event
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The role of a message, when it has one.
pub fn message_role(message: Option<&Value>) -> Option<String> {
    message?
        .get("role")
        .and_then(Value::as_str)
        .map(str::to_owned)
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

fn number(value: Option<&Value>) -> f64 {
    value.and_then(Value::as_f64).unwrap_or(0.0)
}

/// Reads usage off an event, when it carries any.
pub fn usage_of(record: &Value) -> Option<Usage> {
    let message = record.get("message").unwrap_or(record);
    let raw = message.get("usage")?;
    let held = raw.as_object()?;

    let mut usage = Usage {
        input: number(held.get("input")),
        output: number(held.get("output")),
        cache_read: number(held.get("cacheRead")),
        cache_write: number(held.get("cacheWrite")),
        total_tokens: number(held.get("totalTokens")),
        cost: held
            .get("cost")
            .and_then(|cost| cost.get("total"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        model: None,
    };
    if let Some(model) = message.get("model").and_then(Value::as_str) {
        usage.model = Some(model.to_owned());
    }
    Some(usage)
}

#[cfg(test)]
mod tests;
