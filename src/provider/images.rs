//! Drawing one picture over the `OpenAI` Images API.
//!
//! `POST {base_url}/images/generations` with a model and a prompt, answered
//! as base64. Deliberately not a session turn: a picture is one request to one
//! endpoint, and sending the prompt to a chat model instead is what made
//! `!gen-image` reach the wrong URL and answer with a link in prose rather
//! than with an image.
//!
//! A server that answers with a link instead of bytes is refused rather than
//! followed, since fetching it would be a second request to a host nobody
//! configured.

use std::collections::BTreeMap;
use std::future::Future;

use serde_json::{Value, json};

use crate::config::schema::AgentConfig;
use crate::provider::models::{AvailableModel, Matched, match_model};
use crate::session::model::{expand_alias, known_providers, resolve_model, split_level};

/// The largest answer read, so a runaway body cannot be held in memory. A
/// 1.5 MB PNG is about 2 MB of base64, which leaves room for a large picture.
pub const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Where a picture is drawn, and what pays for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    /// The base URL, without `/images/generations`. A trailing `/` is allowed.
    pub base_url: String,
    /// The model the server is asked for.
    pub model: String,
    /// Sent as a bearer token. Empty sends no credential, for a server that
    /// needs none.
    pub credential: String,
}

/// One picture drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Drawn {
    /// The image file's bytes.
    pub bytes: Vec<u8>,
    /// What the server said it rewrote the prompt to, when it said.
    pub revised_prompt: Option<String>,
}

/// The picture was not drawn, in words worth posting into a thread.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ImageError(pub String);

/// Where the configured image model is drawn from, or why it cannot be.
///
/// The name is read as `!model` reads one: a short name from `aliases`, a
/// `provider/id`, or a bare id, with the effort to use as a level on the end.
/// The level stays on the model, because it is part of the name the images
/// endpoint is asked for, as `image:max`.
///
/// The provider is the one the catalog says serves the model, which is where
/// the request goes. A bare id is not read as the provider a session runs on:
/// that is a chat host, and sending an image request there reaches a route it
/// does not serve. Only a catalog that lists nothing leaves the name to the
/// configured provider, since then there is nothing better to go on.
///
/// The endpoint is the provider's own `baseUrl`. A provider whose key the
/// daemon does not hold cannot be reached at all, and that is said rather than
/// attempted with an empty one.
///
/// The effort is a level, and the images endpoint takes it as part of the
/// model id, as `image:max`. One asked for on the command replaces the level
/// the name has, since somebody typing one is being more specific than the
/// configuration was. None leaves the name as it is, which is how the
/// configuration's own level survives.
pub fn endpoint_with_effort(
    agent: &AgentConfig,
    available: &[AvailableModel],
    named: &str,
    aliases: &BTreeMap<String, String>,
    effort: Option<&str>,
) -> Result<Endpoint, ImageError> {
    let expanded = expand_alias(named, aliases);
    // The provider is where the request goes; the model is what the endpoint
    // is asked for, which is the id without the provider's name on it.
    let (provider, model) = match match_model(available, &expanded, &agent.provider) {
        Matched::One(found) => (found.provider, found.id),
        Matched::Several(options) => {
            return Err(ImageError(format!(
                "more than one provider serves `{expanded}`. Name one of these as `agent.imageGen`: {}",
                options.join(", ")
            )));
        }
        // Nothing lists it, so the name is all there is to go on: a
        // `provider/id` still names its provider, and a bare id falls back to
        // the configured one.
        Matched::None => {
            let chosen = resolve_model(&expanded, &known_providers(agent));
            (
                chosen.provider.unwrap_or_else(|| agent.provider.clone()),
                chosen.model,
            )
        }
    };
    let base_url = agent
        .providers
        .get(&provider)
        .and_then(|definition| definition.get("baseUrl"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| {
            ImageError(format!(
                "no endpoint is configured for `{provider}`; give it a `baseUrl`"
            ))
        })?;
    let credential = agent.credential_of(&provider).unwrap_or_default();
    // The level goes on the bare id, so an effort asked for on the command
    // replaces the one the name carried rather than stacking beside it.
    let model = match effort {
        Some(effort) => format!("{}:{effort}", split_level(&model).0),
        None => model,
    };
    Ok(Endpoint {
        base_url: base_url.to_owned(),
        model,
        credential: credential.to_owned(),
    })
}

/// The file extension of the image `bytes` declare themselves, for a type a
/// chat service shows as a picture: PNG, JPEG, GIF or WebP.
pub fn image_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// A drawn picture ready to post: its bytes, the name to give the file, and
/// the caption to post it with.
///
/// The size limit and the type check happen here rather than at each caller,
/// so a picture too large or of a type a channel cannot show is refused the
/// same way wherever it was drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    /// The bytes to post.
    pub bytes: Vec<u8>,
    /// The file name, with the extension the bytes declare.
    pub name: String,
    /// What the picture is posted with.
    pub caption: String,
}

impl Drawn {
    /// The picture to post, or why it cannot be.
    ///
    /// `limit` is the largest upload the surface takes, and the caption names
    /// the model that drew it and what the server said about the prompt when
    /// it said anything.
    pub fn into_picture(self, model: &str, limit: u64) -> Result<Picture, ImageError> {
        let size = u64::try_from(self.bytes.len()).unwrap_or(u64::MAX);
        if size > limit {
            return Err(ImageError(format!(
                "the picture is {size} bytes, over the {limit} byte upload limit, so it was not \
                 posted"
            )));
        }
        let Some(extension) = image_extension(&self.bytes) else {
            return Err(ImageError(
                "what came back is not a PNG, JPEG, GIF or WebP, so it was not posted".to_owned(),
            ));
        };
        Ok(Picture {
            caption: match self.revised_prompt.as_deref() {
                Some(revised) => format!("drawn on `{model}`: {revised}"),
                None => format!("drawn on `{model}`"),
            },
            name: format!("image.{extension}"),
            bytes: self.bytes,
        })
    }
}

/// What a draw is asked for.
#[derive(Debug, Clone)]
pub struct Request {
    /// Headers the request carries.
    pub headers: Vec<(String, String)>,
    /// The JSON body.
    pub body: String,
    /// How long the whole call may take.
    pub timeout_ms: u64,
}

/// What a draw answered with.
#[derive(Debug, Clone)]
pub struct Response {
    /// The HTTP status.
    pub status: u16,
    /// The parsed JSON body, or nothing when it was not JSON.
    pub body: Option<Value>,
}

/// Performs the request. Injected so tests need no network.
pub trait Post: Send + Sync {
    /// Performs the request, or says why it could not.
    fn post(
        &self,
        url: String,
        request: Request,
    ) -> impl Future<Output = Result<Response, String>> + Send;
}

/// Draws one picture, or fails with a reason worth showing.
///
/// The bytes are asked for as `b64_json`. `OpenAI`'s GPT image models always
/// answer base64 and refuse that parameter, so a refusal naming it is asked
/// again without it.
pub async fn draw(
    endpoint: &Endpoint,
    prompt: &str,
    post: &impl Post,
) -> Result<Drawn, ImageError> {
    let first = ask(endpoint, prompt, post, true).await;
    match &first {
        Err(ImageError(detail)) if detail.contains("response_format") => {
            ask(endpoint, prompt, post, false).await
        }
        _ => first,
    }
}

/// One request, asking for base64 when `ask_base64` is set.
async fn ask(
    endpoint: &Endpoint,
    prompt: &str,
    post: &impl Post,
    ask_base64: bool,
) -> Result<Drawn, ImageError> {
    let mut body = json!({
        "model": endpoint.model,
        "prompt": prompt,
        "n": 1,
    });
    if ask_base64 {
        body["response_format"] = Value::String("b64_json".to_owned());
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
            format!("{base}/images/generations"),
            Request {
                headers,
                body: body.to_string(),
                timeout_ms: DRAW_TIMEOUT_MS,
            },
        )
        .await
        .map_err(|error| ImageError(format!("it could not be reached: {error}")))?;

    if !(200..300).contains(&answer.status) {
        return Err(ImageError(format!(
            "{} refused to draw it: {}",
            endpoint.model,
            detail(answer.body.as_ref())
        )));
    }
    let body = answer
        .body
        .ok_or_else(|| ImageError(format!("{} answered no JSON", endpoint.model)))?;
    read(&body).ok_or_else(|| {
        ImageError(format!(
            "{} returned no image this reads; a link rather than bytes is not followed",
            endpoint.model
        ))
    })
}

/// How long one picture may take.
///
/// Longer than a question, because a server drawing one is doing more work
/// than a model answering in text.
pub const DRAW_TIMEOUT_MS: u64 = 180_000;

/// The first entry of `data` that carries bytes, with what the server said it
/// rewrote the prompt to.
fn read(body: &Value) -> Option<Drawn> {
    let entries = body.get("data")?.as_array()?;
    for entry in entries {
        let Some(encoded) = entry.get("b64_json").and_then(Value::as_str) else {
            continue;
        };
        let Some(bytes) = decode_base64(encoded) else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }
        return Some(Drawn {
            bytes,
            revised_prompt: entry
                .get("revised_prompt")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|revised| !revised.is_empty())
                .map(str::to_owned),
        });
    }
    None
}

/// The server's own message about a refusal, or a plain one when it said
/// nothing readable.
fn detail(body: Option<&Value>) -> String {
    let message = body
        .and_then(|body| body.get("error"))
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .or_else(|| {
            body.and_then(|body| body.get("message"))
                .and_then(Value::as_str)
        });
    message.map_or_else(|| "it did not say why".to_owned(), str::to_owned)
}

/// Decodes standard base64, padded or not. Nothing for any other character.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let digits = text.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(digits.len() / 4 * 3 + 2);
    for chunk in digits.chunks(4) {
        if chunk.len() == 1 {
            return None;
        }
        let mut combined = 0u32;
        for (at, &c) in chunk.iter().enumerate() {
            combined |= u32::from(value(c)?) << (18 - 6 * at);
        }
        let [_, a, b, c] = combined.to_be_bytes();
        out.extend_from_slice(&[a, b, c][..chunk.len() - 1]);
    }
    Some(out)
}

/// Performs an image request over HTTPS, as the daemon does.
#[derive(Clone, Copy, Default)]
pub struct HttpPost;

impl Post for HttpPost {
    async fn post(&self, url: String, request: Request) -> Result<Response, String> {
        let client = reqwest::Client::new();
        let mut sent = client.post(url);
        for (name, value) in &request.headers {
            sent = sent.header(name.as_str(), value.as_str());
        }
        let answer = sent
            .body(request.body)
            .timeout(std::time::Duration::from_millis(request.timeout_ms))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = answer.status().as_u16();
        // Capped rather than read whole, so a server answering a wall of
        // bytes cannot fill memory before it is refused.
        let bytes = answer.bytes().await.map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(format!(
                "it answered more than {MAX_RESPONSE_BYTES} bytes, which is more than an image"
            ));
        }
        let body = serde_json::from_slice::<Value>(&bytes).ok();
        Ok(Response { status, body })
    }
}

#[cfg(test)]
mod tests;
