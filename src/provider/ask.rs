//! One question to one model, with no conversation and no tools.
//!
//! Deliberately not a client. There is no history, no system prompt describing
//! what the session is doing, and nothing the model can call: it is shown an
//! artefact and asked about it, and it answers in text.

use std::future::Future;

use serde_json::{Value, json};

/// Where a model is reached, and what pays for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    /// The endpoint the model is reached at.
    pub base_url: String,
    /// The model asked.
    pub model: String,
    /// The credential for the endpoint.
    pub credential: String,
}

/// What the model was asked, and what it was shown.
#[expect(
    clippy::struct_field_names,
    reason = "`question` is what the field is called wherever an ask travels"
)]
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    /// What is wanted to know.
    pub question: String,
    /// What the content is, so the answer can say what it describes.
    pub describes: String,
    /// The material the question is about.
    pub content: String,
}

/// What came back, and what it cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// What the model said.
    pub text: String,
    /// Tokens the provider charged, when it said.
    pub tokens: Option<i64>,
}

/// The model did not answer. Carries words worth showing in a thread.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct AskFailed(pub String);

/// What a send is asked for.
#[derive(Debug, Clone)]
pub struct SendRequest {
    /// Headers the request carries.
    pub headers: Vec<(String, String)>,
    /// The JSON body.
    pub body: String,
}

/// Performs the request. Injected so tests need no network.
///
/// Dropping the future this returns is how an in-flight question is
/// abandoned.
pub trait Sender: Send + Sync {
    /// Performs the request, or says why it could not.
    fn send(
        &self,
        url: String,
        request: SendRequest,
    ) -> impl Future<Output = Result<(u16, Option<Value>), String>> + Send;
}

const INSTRUCTION: &str = "Answer the question about the material below, using only what is in \
     it. Quote exactly when quoting: transcribe identifiers, paths, and messages character for \
     character. Say plainly when the material does not answer the question. Do not suggest what \
     to do about it.";

/// Asks the model, returning its answer or failing with a readable reason.
///
/// The caller owns the deadline through `cancel`, since abandoning a
/// delegation is the caller's decision rather than this function's.
pub async fn ask(
    endpoint: &Endpoint,
    asked: &Question,
    cancel: impl Future<Output = ()> + Send + Unpin,
    send: &impl Sender,
) -> Result<Answer, AskFailed> {
    let body = json!({
        "model": endpoint.model,
        "messages": [{
            "role": "user",
            "content": format!(
                "{INSTRUCTION}\n\nQuestion: {}\n\n{}:\n{}",
                asked.question, asked.describes, asked.content
            ),
        }],
    });

    // One or more trailing slashes come off, so the endpoint never doubles up.
    let base = endpoint.base_url.trim_end_matches('/');
    let outcome = tokio::select! {
        () = cancel => Err("it did not answer in time".to_owned()),
        answer = send.send(
            format!("{base}/chat/completions"),
            SendRequest {
                headers: vec![
                    (
                        "Authorization".to_owned(),
                        format!("Bearer {}", endpoint.credential),
                    ),
                    ("Content-Type".to_owned(), "application/json".to_owned()),
                ],
                body: body.to_string(),
            },
        ) => answer,
    };

    let (status, parsed) = match outcome {
        Err(error) => {
            return Err(AskFailed(format!("it could not be reached: {error}")));
        }
        Ok(answer) => answer,
    };

    if !(200..300).contains(&status) {
        return Err(AskFailed(format!(
            "{} refused the question: {status}",
            endpoint.model
        )));
    }

    let text = parsed.as_ref().and_then(content_of).unwrap_or_default();
    if text.trim().is_empty() {
        return Err(AskFailed(format!("{} returned no answer", endpoint.model)));
    }
    let tokens = parsed.as_ref().and_then(tokens_of);
    Ok(Answer {
        text: text.trim().to_owned(),
        tokens,
    })
}

fn content_of(body: &Value) -> Option<String> {
    body.get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?
        .as_str()
        .map(str::to_owned)
}

fn tokens_of(body: &Value) -> Option<i64> {
    body.get("usage")?
        .get("total_tokens")
        .and_then(Value::as_i64)
}

/// Performs a delegation request over HTTPS, as the agent's own calls do.
pub struct HttpSender;

impl Sender for HttpSender {
    async fn send(
        &self,
        url: String,
        request: SendRequest,
    ) -> Result<(u16, Option<Value>), String> {
        let client = reqwest::Client::new();
        let mut sent = client.post(url);
        for (name, value) in &request.headers {
            sent = sent.header(name.as_str(), value.as_str());
        }
        let answer = sent
            .header("Content-Type", "application/json")
            .body(request.body)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = answer.status().as_u16();
        let body = answer.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, Some(body)))
    }
}
