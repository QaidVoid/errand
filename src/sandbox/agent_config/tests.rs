//! Tests for what a sandboxed agent is told about providers and extensions.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{BrokeredProvider, copy_tree, provider_config, write_agent_config};
use crate::sandbox::backend::SandboxLaunch;

/// A provider the agent has no entry for is only reachable because the
/// operator defined it, so the broker's base URL must not replace that
/// definition.
#[test]
fn an_operators_provider_definition_survives_the_brokers_base_url() {
    let mut defined = serde_json::Map::new();
    defined.insert(
        "meta".to_owned(),
        json!({
            "baseUrl": "https://api.meta.example/v1",
            "api": "openai-completions",
            "credential": "the-real-meta-key",
            "usage": "gateway",
            "models": [{ "id": "muse-spark-1.3-contributor" }],
        }),
    );

    let mut brokered = BTreeMap::new();
    brokered.insert(
        "meta".to_owned(),
        BrokeredProvider {
            base_url: "http://169.254.169.1:8443/provider/meta".to_owned(),
            nonce: "n-meta".to_owned(),
        },
    );
    let merged = provider_config(&defined, &brokered, &BTreeMap::new(), false);
    let providers = merged
        .get("providers")
        .and_then(Value::as_object)
        .expect("the providers map");
    let meta = providers.get("meta").expect("the merged provider");

    // Only where it is reached changes; what it is stays.
    assert_eq!(
        meta.get("baseUrl").and_then(Value::as_str),
        Some("http://169.254.169.1:8443/provider/meta")
    );
    assert_eq!(
        meta.get("api").and_then(Value::as_str),
        Some("openai-completions")
    );
    assert_eq!(
        meta.get("models").and_then(Value::as_array).map(Vec::len),
        Some(1)
    );
    // The key the agent is given is the nonce, and the real one never appears.
    assert_eq!(meta.get("apiKey").and_then(Value::as_str), Some("n-meta"));
    assert_eq!(meta.get("credential"), None);
    // How the daemon asks about the window is not the agent's to read either.
    assert_eq!(meta.get("usage"), None);
    assert!(
        !Value::Object(merged.clone())
            .to_string()
            .contains("the-real-meta-key")
    );
}

#[test]
fn a_provider_the_operator_never_defined_still_gets_its_base_url() {
    let mut brokered = BTreeMap::new();
    brokered.insert(
        "zai-coding-cn".to_owned(),
        BrokeredProvider {
            base_url: "http://169.254.169.1:8443/provider/zai-coding-cn".to_owned(),
            nonce: "n".to_owned(),
        },
    );
    let merged = provider_config(&serde_json::Map::new(), &brokered, &BTreeMap::new(), false);
    let providers = merged
        .get("providers")
        .and_then(Value::as_object)
        .expect("the providers map");

    assert_eq!(
        providers.get("zai-coding-cn"),
        Some(&json!({
            "baseUrl": "http://169.254.169.1:8443/provider/zai-coding-cn",
            "apiKey": "n",
        }))
    );
}

/// Under a broker the key stays with the daemon, even for a provider the
/// broker has no route to.
#[test]
fn an_unbrokered_definition_passes_through_less_the_credential() {
    let mut defined = serde_json::Map::new();
    defined.insert(
        "meta".to_owned(),
        json!({ "baseUrl": "https://api.meta.example/v1", "credential": "k" }),
    );
    let merged = provider_config(&defined, &BTreeMap::new(), &BTreeMap::new(), false);
    let providers = merged
        .get("providers")
        .and_then(Value::as_object)
        .expect("the providers map");

    assert_eq!(
        providers.get("meta"),
        Some(&json!({ "baseUrl": "https://api.meta.example/v1" }))
    );
}

/// With no broker to put a key on, every provider carries its own, so the
/// agent can switch to any of them. A key written as `apiKey` is kept.
#[test]
fn without_a_broker_each_credential_becomes_the_providers_key() {
    let mut defined = serde_json::Map::new();
    defined.insert(
        "zai-coding-cn".to_owned(),
        json!({ "credential": "z-key", "models": [{ "id": "glm-5.3" }] }),
    );
    defined.insert(
        "meta".to_owned(),
        json!({ "baseUrl": "https://api.meta.example/v1", "apiKey": "own-key", "credential": "k" }),
    );
    let merged = provider_config(&defined, &BTreeMap::new(), &BTreeMap::new(), true);

    assert_eq!(
        merged["providers"],
        json!({
            "zai-coding-cn": { "apiKey": "z-key", "models": [{ "id": "glm-5.3" }] },
            "meta": { "baseUrl": "https://api.meta.example/v1", "apiKey": "own-key" },
        })
    );
}

/// An entry naming a model the agent already defines adjusts that model
/// rather than replacing it with one that has forgotten how to think. A model
/// the store does not know is left as written.
#[test]
fn an_entry_for_a_built_in_model_is_laid_over_its_definition() {
    let mut defined = serde_json::Map::new();
    defined.insert(
        "zai-coding-cn".to_owned(),
        json!({
            "credential": "k",
            "models": [
                { "id": "glm-5.3-flash", "contextWindow": 256_000 },
                { "id": "glm-6", "contextWindow": 128_000 },
            ],
        }),
    );
    let built_in = BTreeMap::from([(
        "zai-coding-cn".to_owned(),
        vec![json!({
            "id": "glm-5.3-flash",
            "provider": "zai-coding-cn",
            "api": "openai-completions",
            "baseUrl": "https://zai.example/v4",
            "reasoning": true,
            "thinkingLevelMap": { "max": "max" },
            "contextWindow": 1_000_000,
        })],
    )]);
    let merged = provider_config(&defined, &BTreeMap::new(), &built_in, false);

    assert_eq!(
        merged["providers"]["zai-coding-cn"]["models"],
        json!([
            {
                "id": "glm-5.3-flash",
                "api": "openai-completions",
                "reasoning": true,
                "thinkingLevelMap": { "max": "max" },
                "contextWindow": 256_000,
            },
            { "id": "glm-6", "contextWindow": 128_000 },
        ])
    );
}

/// An extension registers its own provider, so errand must not write a second
/// definition for it into the agent's configuration.
#[test]
fn an_extension_provider_is_left_out_of_what_is_written() {
    let mut defined = serde_json::Map::new();
    defined.insert(
        "free-models".to_owned(),
        json!({ "extension": true, "models": [{ "id": "free-fast" }] }),
    );
    defined.insert(
        "meta".to_owned(),
        json!({ "baseUrl": "https://api.meta.example/v1", "credential": "k" }),
    );
    let merged = provider_config(&defined, &BTreeMap::new(), &BTreeMap::new(), false);
    let providers = merged
        .get("providers")
        .and_then(Value::as_object)
        .expect("the providers map");

    assert!(
        !providers.contains_key("free-models"),
        "the extension owns it"
    );
    assert!(
        providers.contains_key("meta"),
        "an ordinary provider still passes"
    );
}

/// An extension is a directory the sandbox cannot see, so it is copied whole
/// into the session, subdirectories and all.
#[tokio::test]
async fn an_extension_directory_is_copied_whole() {
    let root = tempfile::tempdir().expect("a temp dir");
    let from = root.path().join("free-models");
    std::fs::create_dir_all(from.join("inner")).expect("made the source tree");
    std::fs::write(from.join("index.ts"), b"export default () => {};").expect("wrote entry");
    std::fs::write(from.join("inner/data.json"), b"{}").expect("wrote nested");

    let to = root.path().join("placed");
    copy_tree(&from, &to).await.expect("copied");

    assert_eq!(
        std::fs::read(to.join("index.ts")).expect("entry copied"),
        b"export default () => {};"
    );
    assert!(
        to.join("inner/data.json").exists(),
        "the nested file came too"
    );
}

/// With no broker, as under podman, the agent's home is given every
/// provider's key and every extension, where the agent reads them.
#[tokio::test]
async fn the_agent_home_holds_the_providers_and_extensions() {
    let root = tempfile::tempdir().expect("a temp dir");
    let extension = root.path().join("free-models");
    std::fs::create_dir_all(&extension).expect("made the extension");
    std::fs::write(extension.join("index.ts"), b"export default () => {};").expect("wrote it");
    let state = root.path().join("state");

    let mut providers = serde_json::Map::new();
    providers.insert(
        "gateway".to_owned(),
        json!({ "baseUrl": "https://gateway.example/v1", "credential": "g-key" }),
    );
    let launch = SandboxLaunch {
        state_dir: state.display().to_string(),
        providers,
        extensions: vec![extension.display().to_string()],
        ..SandboxLaunch::default()
    };
    write_agent_config(&launch, &BTreeMap::new(), &BTreeMap::new(), true)
        .await
        .expect("written");

    let agent = state.join("home/.pi/agent");
    let written: Value = serde_json::from_str(
        &std::fs::read_to_string(agent.join("models.json")).expect("the models file"),
    )
    .expect("JSON");
    assert_eq!(written["providers"]["gateway"]["apiKey"], "g-key");
    assert!(agent.join("extensions/free-models/index.ts").exists());
}
