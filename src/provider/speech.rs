//! Reading a text aloud over the `OpenAI` speech shape.
//!
//! `POST {base_url}/audio/speech` with a model, the text, and the voice and
//! language asked for, answered as the clip's bytes. Beside
//! [`crate::provider::images`], which draws a picture the same way: one
//! request to one endpoint, deliberately not a session turn, so the text
//! cannot be sent to a chat model and come back as a link in prose.
//!
//! `response_format` asks for `mp3`, so a server whose default is another
//! format does not hand back a clip this client then refuses. `language` is
//! not an `OpenAI` field, and not every server takes it. A refusal naming
//! either, in `error.param` or in its own message, is asked again without
//! the fields it names, at most twice more. The answer's own type claim is
//! never used: the bytes are sniffed, and what is not MP3 or WAV is refused.

use std::collections::BTreeMap;
use std::future::Future;

use serde_json::{Value, json};

use crate::chat::render::{inert, media_caption, widen};
use crate::config::schema::AgentConfig;
use crate::provider::audio::audio_extension;
use crate::provider::models::{AvailableModel, serving};

/// How long one reading may take. The same as a picture: a server
/// synthesizing one is doing work of the same size.
pub const SPEAK_TIMEOUT_MS: u64 = 180_000;

/// The largest answer read, so a runaway body cannot be held in memory. The
/// same as a picture's, since a clip a channel can show is far smaller.
pub const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Where a text is read aloud, and what pays for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    /// The base URL, without `/audio/speech`. A trailing `/` is allowed.
    pub base_url: String,
    /// The model the server is asked for.
    pub model: String,
    /// Sent as a bearer token. Empty sends no credential, for a server that
    /// needs none.
    pub credential: String,
}

/// One text read aloud.
#[derive(Debug, Clone, PartialEq)]
pub struct Spoken {
    /// The clip's bytes.
    pub bytes: Vec<u8>,
    /// The extension the bytes themselves declare, which names the file
    /// they are posted as.
    pub extension: &'static str,
}

/// A clip ready to post: its bytes, the name to give the file, and the
/// caption to post it with.
///
/// The size limit happens here rather than at each caller, so a clip too
/// large for the surface is refused the same way wherever it was read.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    /// The bytes to post.
    pub bytes: Vec<u8>,
    /// The file name, with the extension the bytes declare.
    pub name: String,
    /// What the clip is posted with.
    pub caption: String,
}

impl Spoken {
    /// The clip to post, or why it cannot be. `limit` is the largest upload
    /// the surface takes.
    pub fn into_clip(self, limit: u64) -> Result<Clip, SpeechError> {
        let size = u64::try_from(self.bytes.len()).unwrap_or(u64::MAX);
        if size > limit {
            return Err(SpeechError(format!(
                "the clip is {size} bytes, over the {limit} byte upload limit, so it was not \
                 posted"
            )));
        }
        Ok(Clip {
            caption: media_caption(None, widen(size), None, "a clip"),
            name: format!("clip.{}", self.extension),
            bytes: self.bytes,
        })
    }
}

/// The text was not read, in words worth posting into a thread.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SpeechError(pub String);

/// Resolves where `agent.speechGen` reads a text aloud, or why it cannot.
///
/// The name is resolved by [`crate::provider::models::serving`], exactly as
/// `!gen-image` resolves one. It carries no effort level, because the speech
/// tiers are model names on the server rather than levels on one.
pub fn endpoint(
    agent: &AgentConfig,
    available: &[AvailableModel],
    named: &str,
    aliases: &BTreeMap<String, String>,
) -> Result<Endpoint, SpeechError> {
    let (provider, model) =
        serving(agent, available, named, aliases, "agent.speechGen").map_err(SpeechError)?;
    let base_url = agent.base_url_of(&provider).ok_or_else(|| {
        SpeechError(format!(
            "no endpoint is configured for `{provider}`; give it a `baseUrl`"
        ))
    })?;
    let credential = agent.credential_of(&provider).unwrap_or_default();
    Ok(Endpoint {
        base_url: base_url.to_owned(),
        model,
        credential: credential.to_owned(),
    })
}

/// Reads one text aloud, or fails with a reason worth showing.
///
/// The clip is asked for as MP3, and `language`, when the request names one,
/// is sent as a hint. A 400 is answered before any audio is made, so asking
/// again costs a round trip and no synthesis: a refusal naming
/// `response_format` or `language`, in `error.param` or in the server's own
/// message, is asked again without the fields it names, and only those that
/// were sent, at most twice more.
pub async fn speak(
    endpoint: &Endpoint,
    text: &str,
    voice: Option<&str>,
    language: Option<&str>,
    post: &impl Post,
) -> Result<Spoken, SpeechError> {
    let mut ask_language = language.is_some();
    let mut ask_format = true;
    let mut result = ask(
        endpoint,
        text,
        voice,
        language,
        ask_language,
        ask_format,
        post,
    )
    .await;
    for _ in 0..2 {
        let Err(Fail::Refused(refusal)) = &result else {
            break;
        };
        let drop_language = ask_language && names_field(refusal, "language");
        let drop_format = ask_format && names_field(refusal, "response_format");
        if !drop_language && !drop_format {
            break;
        }
        ask_language &= !drop_language;
        ask_format &= !drop_format;
        result = ask(
            endpoint,
            text,
            voice,
            language,
            ask_language,
            ask_format,
            post,
        )
        .await;
    }
    match result {
        Ok(spoken) => Ok(spoken),
        Err(Fail::Refused(refusal)) => Err(SpeechError(format!(
            "{} refused to read it: {}",
            endpoint.model,
            detail(refusal.body.as_ref())
        ))),
        Err(Fail::Other(said)) => Err(SpeechError(said)),
    }
}

/// One request, sending `language` when `ask_language` is set and
/// `response_format` when `ask_format` is.
async fn ask(
    endpoint: &Endpoint,
    text: &str,
    voice: Option<&str>,
    language: Option<&str>,
    ask_language: bool,
    ask_format: bool,
    post: &impl Post,
) -> Result<Spoken, Fail> {
    let mut body = json!({
        "model": endpoint.model,
        "input": text,
    });
    if let Some(voice) = voice {
        body["voice"] = Value::String(voice.to_owned());
    }
    if let (true, Some(language)) = (ask_language, language) {
        body["language"] = Value::String(language.to_owned());
    }
    if ask_format {
        body["response_format"] = Value::String("mp3".to_owned());
    }
    let mut headers = vec![("Content-Type".to_owned(), "application/json".to_owned())];
    if !endpoint.credential.is_empty() {
        headers.push((
            "Authorization".to_owned(),
            format!("Bearer {}", endpoint.credential),
        ));
    }

    // One or more trailing slashes come off, so the path never doubles up.
    let base = endpoint.base_url.trim_end_matches('/');
    let answer = post
        .post(
            format!("{base}/audio/speech"),
            Request {
                headers,
                body: body.to_string(),
                timeout_ms: SPEAK_TIMEOUT_MS,
            },
        )
        .await
        .map_err(|error| Fail::Other(format!("it could not be reached: {error}")))?;

    if !(200..300).contains(&answer.status) {
        return Err(Fail::Refused(Refusal {
            status: answer.status,
            body: serde_json::from_slice(&answer.bytes).ok(),
        }));
    }
    let extension = audio_extension(&answer.bytes).ok_or_else(|| {
        Fail::Other(format!(
            "{} answered something that is not MP3 or WAV, so it was not posted",
            endpoint.model
        ))
    })?;
    Ok(Spoken {
        bytes: answer.bytes,
        extension,
    })
}

/// Why a request failed, kept whole so the retry rule can read a refusal's
/// status and body before it is worded for a thread.
enum Fail {
    /// The server answered, and refused.
    Refused(Refusal),
    /// It could not be reached, or answered something unusable.
    Other(String),
}

/// A refusal, with what the server said about it.
struct Refusal {
    /// The HTTP status.
    status: u16,
    /// The parsed JSON body, or nothing when it was not JSON.
    body: Option<Value>,
}

/// Whether a 400 refusal names `field`, in `error.param` or in the server's
/// own message, which is how a server that refuses a parameter one request
/// at a time is asked again without it.
fn names_field(refusal: &Refusal, field: &str) -> bool {
    if refusal.status != 400 {
        return false;
    }
    let Some(body) = &refusal.body else {
        return false;
    };
    body.pointer("/error/param").and_then(Value::as_str) == Some(field)
        || message_in(body).is_some_and(|message| message.contains(field))
}

/// The server's own message in an error body: the `OpenAI` shape
/// `error.message`, a top-level `message`, or `errors.message`, which some
/// servers send.
fn message_in(body: &Value) -> Option<&str> {
    body.pointer("/error/message")
        .or_else(|| body.get("message"))
        .or_else(|| body.pointer("/errors/message"))
        .and_then(Value::as_str)
}

/// The server's own message about a refusal, or a plain one when it said
/// nothing readable. The message is the server's free text and is posted to
/// the thread, so it is made inert like a caption.
fn detail(body: Option<&Value>) -> String {
    let message = body.and_then(message_in);
    message.map_or_else(|| "it did not say why".to_owned(), inert)
}

/// What a speech request is asked for.
#[derive(Debug, Clone)]
pub struct Request {
    /// Headers the request carries.
    pub headers: Vec<(String, String)>,
    /// The JSON body.
    pub body: String,
    /// How long the whole call may take.
    pub timeout_ms: u64,
}

/// What a speech request answered with.
#[derive(Debug, Clone)]
pub struct Answer {
    /// The HTTP status.
    pub status: u16,
    /// The body, whether a refusal's JSON or the clip's bytes.
    pub bytes: Vec<u8>,
}

/// Performs the request. Injected so tests need no network.
pub trait Post: Send + Sync {
    /// Performs the request, or says why it could not.
    fn post(
        &self,
        url: String,
        request: Request,
    ) -> impl Future<Output = Result<Answer, String>> + Send;
}

/// Performs a speech request over HTTPS, as the daemon does.
#[derive(Clone, Copy, Default)]
pub struct HttpPost;

impl Post for HttpPost {
    async fn post(&self, url: String, request: Request) -> Result<Answer, String> {
        let client = reqwest::Client::new();
        let mut sent = client.post(url);
        for (name, value) in &request.headers {
            sent = sent.header(name.as_str(), value.as_str());
        }
        let mut answer = sent
            .body(request.body)
            .timeout(std::time::Duration::from_millis(request.timeout_ms))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = answer.status().as_u16();
        // Read a chunk at a time and stopped at the cap, so a server
        // answering a wall of bytes cannot fill memory before it is refused.
        let mut bytes = Vec::new();
        while let Some(chunk) = answer.chunk().await.map_err(|error| error.to_string())? {
            if (bytes.len() + chunk.len()) as u64 > MAX_RESPONSE_BYTES {
                return Err(format!(
                    "it answered more than {MAX_RESPONSE_BYTES} bytes, which is more than a clip"
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Answer { status, bytes })
    }
}

#[cfg(test)]
mod tests;
