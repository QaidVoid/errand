//! Tests for vision routing, ported from `vision_test.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{Post, PostRequest, PostResponse, describe_images, described_block, image_describer};
use crate::agent::protocol::AgentImage;
use crate::config::schema::AgentConfig;
use crate::provider::models::{agent_directory, read_models, sees_images, vision_model};
use tempfile::TempDir;

fn image() -> AgentImage {
    AgentImage {
        r#type: "image".to_owned(),
        data: "aGVsbG8=".to_owned(),
        mime_type: "image/png".to_owned(),
    }
}

fn store() -> Value {
    json!({
        "zai-coding-cn": {
            "models": [
                { "id": "glm-5.3", "baseUrl": "https://api.example/v1",
                  "input": ["text"], "cost": { "input": 0.6 } },
                { "id": "glm-5.3-flash", "baseUrl": "https://api.example/v1",
                  "input": ["text", "image"], "cost": { "input": 0.1 } },
                { "id": "glm-5.3-vision-pro", "baseUrl": "https://api.example/v1",
                  "input": ["text", "image"], "cost": { "input": 2 } },
            ],
        },
    })
}

fn agent() -> AgentConfig {
    AgentConfig {
        provider: "zai-coding-cn".to_owned(),
        model: Some("glm-5.3".to_owned()),
        vision_model: None,
        credential_name: "ZAI_CODING_CN_API_KEY".to_owned(),
        credential: "secret-key".to_owned(),
        delegate: None,
        rules_path: None,
        providers: serde_json::Map::new(),
        aliases: BTreeMap::new(),
    }
}

fn agent_with_model(model: Option<&str>) -> AgentConfig {
    let mut config = agent();
    config.model = model.map(str::to_owned);
    config
}

fn with_store(contents: &Value) -> (TempDir, String) {
    let directory = TempDir::with_prefix("errand-models-").expect("a temporary directory");
    std::fs::write(
        directory.path().join("models-store.json"),
        contents.to_string(),
    )
    .expect("the store is written");
    let path = directory.path().to_string_lossy().into_owned();
    (directory, path)
}

/// Answers as the provider does, and records what it was asked.
#[derive(Clone)]
struct FakePost {
    answer: Result<PostResponse, String>,
    calls: Arc<Mutex<Vec<(String, Value)>>>,
}

impl FakePost {
    fn described() -> Self {
        Self {
            answer: Ok(PostResponse {
                status: 200,
                body: json!({
                    "choices": [{ "message": { "content": "  a stack trace saying ENOSPC  " } }],
                }),
            }),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn refused() -> Self {
        Self {
            answer: Ok(PostResponse {
                status: 429,
                body: json!({ "error": { "message": "rate limited" } }),
            }),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn empty_answer() -> Self {
        Self {
            answer: Ok(PostResponse {
                status: 200,
                body: json!({ "choices": [{ "message": { "content": "   " } }] }),
            }),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Post for FakePost {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the trait is async; a stand-in that answers at once still has to match it"
    )]
    async fn post(&self, url: String, request: PostRequest) -> Result<PostResponse, String> {
        self.calls
            .lock()
            .unwrap()
            .push((url, serde_json::from_str(&request.body).expect("JSON body")));
        self.answer.clone()
    }
}

#[test]
fn the_store_says_which_models_can_be_shown_an_image() {
    let (_dir, path) = with_store(&store());
    let models = read_models(Some(&path), "zai-coding-cn");

    assert_eq!(models.len(), 3);
    assert!(!sees_images(models.first()));
    assert!(sees_images(models.get(1)));
}

#[test]
fn a_host_with_no_store_or_an_unreadable_one_lists_nothing() {
    let (_dir, path) = with_store(&store());
    assert!(read_models(Some(&path), "somebody-else").is_empty());
    assert!(read_models(Some("/nowhere/at/all"), "zai-coding-cn").is_empty());
    assert!(read_models(None, "zai-coding-cn").is_empty());
}

#[test]
fn a_store_that_is_not_json_is_treated_as_no_store_at_all() {
    let directory = TempDir::with_prefix("errand-models-").expect("a temporary directory");
    std::fs::write(directory.path().join("models-store.json"), "{ not json").expect("written");
    let path = directory.path().to_string_lossy().into_owned();

    assert!(read_models(Some(&path), "zai-coding-cn").is_empty());
}

/// Describing an image is a paragraph, and the work is done by another model.
#[test]
fn the_cheapest_model_that_can_see_is_the_one_chosen() {
    let (_dir, path) = with_store(&store());

    let chosen = vision_model(&read_models(Some(&path), "zai-coding-cn"), None);

    assert_eq!(
        chosen.map(|chosen| chosen.id),
        Some("glm-5.3-flash".to_owned())
    );
}

#[test]
fn a_model_named_in_the_configuration_wins_over_the_cheapest() {
    let (_dir, path) = with_store(&store());
    let models = read_models(Some(&path), "zai-coding-cn");

    assert_eq!(
        vision_model(&models, Some("glm-5.3-vision-pro")).map(|chosen| chosen.id),
        Some("glm-5.3-vision-pro".to_owned())
    );
    // Named but unable to see, or not in the store: no routing rather than a
    // silent fallback to something nobody asked for.
    assert!(vision_model(&models, Some("glm-5.3")).is_none());
    assert!(vision_model(&models, Some("not-a-model")).is_none());
}

#[test]
fn a_session_whose_model_can_already_see_routes_nothing() {
    let (_dir, path) = with_store(&store());
    let config = agent_with_model(Some("glm-5.3-flash"));

    assert!(image_describer(&config, Some(&path), FakePost::described()).is_none());
}

/// A pattern rather than an id is not in the store. Guessing it cannot see
/// would take images away from a model that can.
#[test]
fn a_model_the_store_does_not_list_is_left_alone() {
    let (_dir, path) = with_store(&store());

    assert!(
        image_describer(
            &agent_with_model(Some("glm-5.3-*")),
            Some(&path),
            FakePost::described()
        )
        .is_none()
    );
    assert!(image_describer(&agent_with_model(None), Some(&path), FakePost::described()).is_none());
}

#[test]
fn a_provider_with_nothing_that_can_see_routes_nothing() {
    let (_dir, path) = with_store(&json!({
        "zai-coding-cn": {
            "models": [{ "id": "glm-5.3", "baseUrl": "https://api.example/v1",
                         "input": ["text"] }],
        },
    }));

    assert!(image_describer(&agent(), Some(&path), FakePost::described()).is_none());
}

/// Knowing a model can see is no use without knowing where to reach it.
#[test]
fn a_model_with_nowhere_to_reach_it_is_not_chosen() {
    let (_dir, path) = with_store(&json!({
        "zai-coding-cn": {
            "models": [
                { "id": "glm-5.3", "baseUrl": "https://api.example/v1", "input": ["text"] },
                { "id": "glm-5.3-flash", "input": ["text", "image"] },
            ],
        },
    }));

    assert!(image_describer(&agent(), Some(&path), FakePost::described()).is_none());
}

#[tokio::test]
async fn a_text_only_model_gets_a_description_from_the_one_that_can_see() {
    let (_dir, path) = with_store(&store());
    let answering = FakePost::described();
    let describer = image_describer(&agent(), Some(&path), answering.clone()).expect("a describer");

    assert_eq!(describer.model, "glm-5.3-flash");
    let block = describer
        .describe(vec![image()], "what does this say?")
        .await
        .expect("a description");

    assert!(block.contains("cannot see images"));
    assert!(block.contains("glm-5.3-flash"));
    assert!(block.contains("a stack trace saying ENOSPC"));

    let calls = answering.calls();
    assert_eq!(
        calls[0].0,
        "https://api.example/v1/chat/completions".to_owned()
    );
    assert_eq!(
        calls[0].1.get("model").and_then(Value::as_str),
        Some("glm-5.3-flash")
    );
}

/// A description that catalogues the picture answers nobody's question.
#[tokio::test]
async fn what_was_asked_is_passed_along_with_the_image() {
    let answering = FakePost::described();

    describe_images(
        &super::Describer {
            base_url: "https://api.example/v1/".to_owned(),
            model: "seer".to_owned(),
            credential: "k".to_owned(),
        },
        &[image()],
        "  what is the error?  ",
        &answering,
    )
    .await
    .expect("a description");

    let content = answering.calls()[0]
        .1
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.first())
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(content[0].to_string().contains("what is the error?"));
    assert!(
        content[1]
            .to_string()
            .contains("data:image/png;base64,aGVsbG8=")
    );
}

#[tokio::test]
async fn a_trailing_slash_on_the_endpoint_does_not_double_up() {
    let answering = FakePost::described();

    describe_images(
        &super::Describer {
            base_url: "https://api.example/v1///".to_owned(),
            model: "seer".to_owned(),
            credential: "k".to_owned(),
        },
        &[image()],
        "",
        &answering,
    )
    .await
    .expect("a description");

    assert_eq!(
        answering.calls()[0].0,
        "https://api.example/v1/chat/completions".to_owned()
    );
}

#[tokio::test]
async fn a_provider_that_refuses_says_so_in_words_worth_posting() {
    let answering = FakePost::refused();

    let error = describe_images(
        &super::Describer {
            base_url: "https://api.example/v1".to_owned(),
            model: "seer".to_owned(),
            credential: "k".to_owned(),
        },
        &[image()],
        "",
        &answering,
    )
    .await
    .expect_err("a refusal");

    assert!(error.to_string().contains("rate limited"));
    assert!(error.to_string().contains("seer"));
}

#[tokio::test]
async fn an_answer_with_no_description_in_it_is_a_failure_not_an_empty_note() {
    let answering = FakePost::empty_answer();

    let error = describe_images(
        &super::Describer {
            base_url: "https://api.example/v1".to_owned(),
            model: "seer".to_owned(),
            credential: "k".to_owned(),
        },
        &[image()],
        "",
        &answering,
    )
    .await
    .expect_err("a failure");

    assert!(error.to_string().contains("returned no description"));
}

/// The agent is told it is reading a description, not looking at the image.
#[test]
fn the_note_says_plainly_what_the_agent_is_being_given() {
    let block = described_block("seer", "a terminal showing a failing test");

    assert!(block.contains("not the image"));
    assert!(block.contains("a terminal showing a failing test"));
}

#[test]
fn the_agent_directory_is_found_by_override_then_by_convention() {
    let (_dir, path) = with_store(&store());

    let env = [
        ("PI_CODING_AGENT_DIR".to_owned(), path.clone()),
        ("HOME".to_owned(), "/nowhere".to_owned()),
    ]
    .into_iter()
    .collect();
    assert_eq!(agent_directory(&env), Some(path));

    let empty_override = [
        ("PI_CODING_AGENT_DIR".to_owned(), " ".to_owned()),
        ("HOME".to_owned(), "/nowhere".to_owned()),
    ]
    .into_iter()
    .collect();
    assert_eq!(agent_directory(&empty_override), None);
}
