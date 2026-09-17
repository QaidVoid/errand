//! A question the session's model asks a cheaper one about something that
//! already exists.
//!
//! A delegation names one artefact and asks one question about it. That shape
//! is the whole guarantee: the cheaper model is shown a single thing and asked
//! about it, so its answer can be checked against the same thing, and it is
//! never in a position to decide anything because it is never told what the
//! session is trying to do.

use serde_json::Value;

use crate::sandbox::paths::within;

/// What a delegation may be asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A file in the session's project.
    File { path: String },
    /// The output a tool call produced, by that call's id.
    Output { call_id: String },
    /// A file attached to the conversation.
    Attachment { name: String },
}

/// A well-formed delegation, before its source has been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delegation {
    /// What is wanted to know.
    pub question: String,
    /// What to look at.
    pub source: Source,
}

/// Why a delegation was not accepted, in words worth showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// The reason, as the thread reads it.
    pub refused: String,
}

/// What a delegation was given, once its source has been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// What is wanted to know.
    pub question: String,
    /// What the source is, for attributing the answer.
    pub describes: String,
    /// The content the question is asked about.
    pub content: String,
}

/// The outcome of reading a delegation or its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<T> {
    /// The work can go ahead.
    Ready(T),
    /// The work was refused, with words worth showing.
    Refused(Refused),
}

/// True when a result is a refusal rather than a value.
pub fn is_refused<T>(value: &Outcome<T>) -> bool {
    matches!(value, Outcome::Refused(_))
}

fn text(record: &Value, key: &str) -> Option<String> {
    match record.get(key) {
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.trim().to_owned()),
        _ => None,
    }
}

/// Reads a delegation the agent wrote.
///
/// A request that names nothing is refused rather than sent, because a
/// delegation with no artefact is a conversation with a second model, which is
/// the thing this is not.
pub fn parse_delegation(raw: &Value) -> Outcome<Delegation> {
    if raw.as_object().is_none() {
        return Outcome::Refused(Refused {
            refused: "a delegation must be an object with a question and a source".to_owned(),
        });
    }

    let Some(question) = text(raw, "question") else {
        return Outcome::Refused(Refused {
            refused: "a delegation must ask a question".to_owned(),
        });
    };

    let path = text(raw, "path");
    let call_id = text(raw, "callId");
    let attachment = text(raw, "attachment");
    let named = [&path, &call_id, &attachment]
        .iter()
        .filter(|value| value.is_some())
        .count();

    if named == 0 {
        return Outcome::Refused(Refused {
            refused:
                "a delegation must name what to look at: a file path, a call id, or an attachment"
                    .to_owned(),
        });
    }
    if named > 1 {
        return Outcome::Refused(Refused {
            refused: "a delegation names one thing to look at, not several".to_owned(),
        });
    }

    if let Some(path) = path {
        return Outcome::Ready(Delegation {
            question,
            source: Source::File { path },
        });
    }
    if let Some(call_id) = call_id {
        return Outcome::Ready(Delegation {
            question,
            source: Source::Output { call_id },
        });
    }
    Outcome::Ready(Delegation {
        question,
        source: Source::Attachment {
            name: attachment.unwrap_or_default(),
        },
    })
}

/// Where the content of a named source is found. Injected for testing.
pub trait Sources: Send + Sync {
    /// The session's project directory, which a file must stay inside.
    fn project_root(&self) -> &str;
    /// Reads a file the delegation names.
    fn read_file(&self, path: &str) -> impl Future<Output = std::io::Result<String>> + Send;
    /// The output a call produced, or nothing when there is no such call.
    fn output_of(&self, call_id: &str) -> Option<String>;
    /// An attachment's text, or nothing when it is not one this session has.
    fn attachment(&self, name: &str) -> Option<String>;
}

/// Reads what a delegation names.
///
/// A file is held to the same containment rule as the agent itself, so a
/// delegation cannot read what the session could not.
pub async fn resolve_source(delegation: &Delegation, sources: &impl Sources) -> Outcome<Resolved> {
    let question = delegation.question.clone();

    match &delegation.source {
        Source::File { path } => {
            let Some(path) = within(sources.project_root(), path) else {
                return Outcome::Refused(Refused {
                    refused: format!(
                        "{} is outside this session's project",
                        match &delegation.source {
                            Source::File { path } => path.clone(),
                            _ => String::new(),
                        }
                    ),
                });
            };
            match sources.read_file(&path).await {
                Ok(content) => Outcome::Ready(Resolved {
                    question,
                    describes: match &delegation.source {
                        Source::File { path } => path.clone(),
                        _ => String::new(),
                    },
                    content,
                }),
                Err(_) => Outcome::Refused(Refused {
                    refused: format!(
                        "{} could not be read",
                        match &delegation.source {
                            Source::File { path } => path.clone(),
                            _ => String::new(),
                        }
                    ),
                }),
            }
        }

        Source::Output { call_id } => match sources.output_of(call_id) {
            None => Outcome::Refused(Refused {
                refused: format!("no call in this session has the id {call_id}"),
            }),
            Some(content) => Outcome::Ready(Resolved {
                question,
                describes: format!("the output of {call_id}"),
                content,
            }),
        },

        Source::Attachment { name } => match sources.attachment(name) {
            None => Outcome::Refused(Refused {
                refused: format!("nothing called {name} is attached to this conversation"),
            }),
            Some(content) => Outcome::Ready(Resolved {
                question,
                describes: name.clone(),
                content,
            }),
        },
    }
}

#[cfg(test)]
mod tests;
