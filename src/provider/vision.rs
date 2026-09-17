//! Describing an image with a model that can see, for one that cannot.
//!
//! A session's model is chosen for the work, and the good ones for code are
//! often text only. Refusing every screenshot on that basis loses the most
//! ordinary thing somebody does in a chat, so an image is shown to a model
//! from the same provider that accepts one, and the agent is given what it
//! said.
//!
//! The description is text, and text is all the agent gets. It is not the same
//! as having seen the image, and the note the agent receives says so rather
//! than pretending otherwise.

use serde_json::{Value, json};

use crate::agent::protocol::AgentImage;
use crate::config::schema::AgentConfig;
use crate::provider::models::{model_by_id, read_models, sees_images, vision_model};

/// How long a description may take before the turn goes on without it.
pub const TIMEOUT_MS: u64 = 60_000;

/// What the describing model is asked for.
const INSTRUCTION: &str = "Describe this image for another model that cannot see it, in a way \
     that lets it act. Transcribe any text, code, error message or stack trace exactly, \
     including punctuation and line breaks. Describe the layout only where it carries meaning. \
     Do not interpret, advise, or add anything that is not in the image.";

/// Where and how to reach the describing model.
#[derive(Debug, Clone, PartialEq)]
pub struct Describer {
    /// The endpoint the describing model is reached at.
    pub base_url: String,
    /// The describing model's id.
    pub model: String,
    /// The credential for the endpoint.
    pub credential: String,
}

/// What a POST answers with, reduced to what a description reads.
#[derive(Debug, Clone)]
pub struct PostResponse {
    /// The HTTP status.
    pub status: u16,
    /// The parsed JSON body, or an empty object when it was not JSON.
    pub body: Value,
}

/// What a POST is asked for.
#[derive(Debug, Clone)]
pub struct PostRequest {
    /// Headers the request carries.
    pub headers: Vec<(String, String)>,
    /// The JSON body.
    pub body: String,
    /// How long the answer may take before it is abandoned.
    pub timeout_ms: u64,
}

/// Sends the request. Injected so tests need no network.
pub trait Post: Send + Sync {
    /// Performs the request, or says why it could not.
    fn post(
        &self,
        url: String,
        request: PostRequest,
    ) -> impl std::future::Future<Output = Result<PostResponse, String>> + Send;
}

/// The description failed, in words worth posting into a thread.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct VisionError(pub String);

fn message_content(body: &Value) -> Option<String> {
    body.get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?
        .as_str()
        .map(str::to_owned)
}

fn detail(body: &Value) -> String {
    if let Some(message) = body
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        return message.to_owned();
    }
    body.get("message")
        .and_then(Value::as_str)
        .map_or_else(|| "it did not say why".to_owned(), str::to_owned)
}

/// Describes images, returning one block of text for the lot.
///
/// `question` is what the person said, passed as context so the description
/// answers what was asked rather than cataloguing the whole picture.
pub async fn describe_images(
    describer: &Describer,
    images: &[AgentImage],
    question: &str,
    post: &impl Post,
) -> Result<String, VisionError> {
    let asked = question.trim();
    let text = if asked.is_empty() {
        INSTRUCTION.to_owned()
    } else {
        format!("{INSTRUCTION}\n\nAsked: {asked}")
    };
    let mut content = vec![json!({ "type": "text", "text": text })];
    content.extend(images.iter().map(|image| {
        json!({
            "type": "image_url",
            "image_url": { "url": format!("data:{};base64,{}", image.mime_type, image.data) },
        })
    }));

    // One or more trailing slashes come off, so the endpoint never doubles up.
    let base = describer.base_url.trim_end_matches('/');
    let answer = post
        .post(
            format!("{base}/chat/completions"),
            PostRequest {
                headers: vec![
                    (
                        "Authorization".to_owned(),
                        format!("Bearer {}", describer.credential),
                    ),
                    ("Content-Type".to_owned(), "application/json".to_owned()),
                ],
                body: json!({
                    "model": describer.model,
                    "messages": [{ "role": "user", "content": content }],
                })
                .to_string(),
                timeout_ms: TIMEOUT_MS,
            },
        )
        .await
        .map_err(|error| VisionError(format!("it could not be reached: {error}")))?;

    if answer.status >= 400 {
        return Err(VisionError(format!(
            "{} refused to describe it: {}",
            describer.model,
            detail(&answer.body)
        )));
    }

    let text = message_content(&answer.body);
    match text {
        Some(text) if !text.trim().is_empty() => Ok(text.trim().to_owned()),
        _ => Err(VisionError(format!(
            "{} returned no description",
            describer.model
        ))),
    }
}

/// The note the agent is given in place of the images it cannot be shown.
pub fn described_block(model: &str, description: &str) -> String {
    [
        &format!("An image was attached. This session's model cannot see images, so {model}")[..],
        "was asked to describe it. What follows is that description, not the image:",
        "",
        description,
    ]
    .join("\n")
}

/// Which model was chosen, and how to ask it.
pub struct ImageDescriber<P: Post> {
    /// The model that does the describing.
    pub model: String,
    describer: Describer,
    post: P,
}

impl<P: Post> ImageDescriber<P> {
    /// Describes the images, wrapped in the note the agent reads.
    pub async fn describe(
        &self,
        images: Vec<AgentImage>,
        question: &str,
    ) -> Result<String, VisionError> {
        let described = describe_images(&self.describer, &images, question, &self.post).await?;
        Ok(described_block(&self.describer.model, &described))
    }
}

/// Decides how a session's images are handled, once, at startup.
///
/// Routing happens only when the configured model is known to be text only. A
/// model the store does not list, which is what a pattern rather than an id
/// produces, is left alone: guessing that it cannot see would take images away
/// from a model that can.
///
/// Returns nothing when there is nothing to do, whether because the model can
/// see, because the provider has no model that can, or because the host has no
/// agent installation to read the store from.
pub fn image_describer(
    agent: &AgentConfig,
    directory: Option<&str>,
    post: impl Post,
) -> Option<ImageDescriber<impl Post>> {
    let models = read_models(directory, &agent.provider);
    let own = model_by_id(&models, agent.model.as_deref());
    if own.is_none() || sees_images(own) {
        return None;
    }

    let chosen = vision_model(&models, agent.vision_model.as_deref())?;
    let describer = Describer {
        base_url: chosen.base_url.clone()?,
        model: chosen.id.clone(),
        credential: agent.credential.clone(),
    };
    Some(ImageDescriber {
        model: chosen.id,
        describer,
        post,
    })
}

/// Performs a vision request over HTTPS, as the production daemon does.
#[derive(Clone, Copy, Default)]
pub struct HttpPost;

impl Post for HttpPost {
    async fn post(&self, url: String, request: PostRequest) -> Result<PostResponse, String> {
        let client = reqwest::Client::new();
        let mut sent = client.post(url);
        for (name, value) in &request.headers {
            sent = sent.header(name.as_str(), value.as_str());
        }
        let answer = sent
            .header("Content-Type", "application/json")
            .body(request.body)
            .timeout(std::time::Duration::from_millis(request.timeout_ms))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = answer.status().as_u16();
        let body = answer.json::<serde_json::Value>().await.unwrap_or_default();
        Ok(PostResponse { status, body })
    }
}
