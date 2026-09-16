//! The agent protocol module. The command framing and the rest of the types
//! arrive with the agent layer; the shapes here now are the ones rendering
//! already consumes.

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
