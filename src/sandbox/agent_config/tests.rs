//! Tests for the kage provider configuration a sandbox reads.

use std::collections::BTreeMap;

use serde_json::json;

use super::{BrokeredProvider, kage_config, write_agent_config};
use crate::sandbox::backend::SandboxLaunch;

/// A brokered provider kage knows is overridden in place: the broker's base
/// URL is what changes, and the key never enters the file.
#[test]
fn a_brokered_known_provider_is_overridden_with_the_brokers_url() {
    let mut brokered = BTreeMap::new();
    brokered.insert(
        "anthropic".to_owned(),
        BrokeredProvider {
            base_url: "http://169.254.169.1:8443/provider/anthropic".to_owned(),
        },
    );
    let names = BTreeMap::from([("anthropic".to_owned(), "ANTHROPIC_API_KEY".to_owned())]);
    let config = kage_config(&serde_json::Map::new(), &brokered, &BTreeMap::new(), &names);

    assert!(config.contains("[providers.anthropic]"), "{config}");
    assert!(
        config.contains(r#"base_url = "http://169.254.169.1:8443/provider/anthropic""#),
        "{config}"
    );
    assert!(!config.contains("api_key_env"), "{config}");
    assert!(!config.contains("custom"), "{config}");
}

/// A brokered provider kage never heard of is registered custom, with the
/// nonce's variable named and the store's models declared.
#[test]
fn a_brokered_unknown_provider_is_registered_custom_with_its_models() {
    let mut brokered = BTreeMap::new();
    brokered.insert(
        "meta".to_owned(),
        BrokeredProvider {
            base_url: "http://169.254.169.1:8443/provider/meta".to_owned(),
        },
    );
    let built_in = BTreeMap::from([(
        "meta".to_owned(),
        vec![json!({ "id": "muse-spark-1.3", "name": "Muse Spark" })],
    )]);
    let names = BTreeMap::from([("meta".to_owned(), "META_API_KEY".to_owned())]);
    let config = kage_config(&serde_json::Map::new(), &brokered, &built_in, &names);

    assert!(config.contains("[providers.custom.meta]"), "{config}");
    assert!(
        config.contains(r#"base_url = "http://169.254.169.1:8443/provider/meta""#),
        "{config}"
    );
    assert!(
        config.contains(r#"api_key_env = "META_API_KEY""#),
        "{config}"
    );
    assert!(
        config.contains("[[providers.custom.meta.models]]"),
        "{config}"
    );
    assert!(config.contains(r#"id = "muse-spark-1.3""#), "{config}");
}

/// An operator definition contributes its base URL and nothing secret.
#[test]
fn an_operator_base_url_passes_through_less_the_credential() {
    let mut defined = serde_json::Map::new();
    defined.insert(
        "meta".to_owned(),
        json!({ "baseUrl": "https://api.meta.example/v1", "credential": "real-meta-key-1" }),
    );
    let config = kage_config(
        &defined,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    );

    assert!(config.contains("[providers.custom.meta]"), "{config}");
    assert!(
        config.contains(r#"base_url = "https://api.meta.example/v1""#),
        "{config}"
    );
    assert!(!config.contains("real-meta-key-1"), "{config}");
}

/// A provider with neither an endpoint nor models to declare is left out:
/// its key still reaches the agent through the environment on its own.
#[test]
fn a_provider_with_nothing_to_say_is_left_out() {
    let mut defined = serde_json::Map::new();
    defined.insert("plain".to_owned(), json!({ "credential": "k" }));
    let config = kage_config(
        &defined,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    );

    assert!(!config.contains("plain"), "{config}");
}

/// An extension registers nothing kage reads, so naming one fails the launch
/// rather than starting a session without the tools it promises.
#[tokio::test]
async fn an_extension_fails_the_launch_loudly() {
    let launch = SandboxLaunch {
        state_dir: tempfile::tempdir()
            .expect("a state directory")
            .path()
            .display()
            .to_string(),
        extensions: vec!["free-models".to_owned()],
        ..SandboxLaunch::default()
    };
    let error = write_agent_config(
        &launch,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .await
    .expect_err("extensions have nowhere to go");
    assert!(error.0.contains("free-models"), "{error:?}");
}

/// The written file holds endpoints and model declarations, and no real key
/// anywhere in it.
#[tokio::test]
async fn the_written_file_holds_endpoints_and_no_keys() {
    let root = tempfile::tempdir().expect("a temp dir");
    let state = root.path().join("state");

    let mut providers = serde_json::Map::new();
    providers.insert(
        "gateway".to_owned(),
        json!({ "baseUrl": "https://gateway.example/v1", "credential": "g-key" }),
    );
    let mut brokered = BTreeMap::new();
    brokered.insert(
        "anthropic".to_owned(),
        BrokeredProvider {
            base_url: "http://169.254.169.1:8443/provider/anthropic".to_owned(),
        },
    );
    let names = BTreeMap::from([
        ("gateway".to_owned(), "GATEWAY_API_KEY".to_owned()),
        ("anthropic".to_owned(), "ANTHROPIC_API_KEY".to_owned()),
    ]);
    let launch = SandboxLaunch {
        state_dir: state.display().to_string(),
        providers,
        ..SandboxLaunch::default()
    };
    write_agent_config(&launch, &brokered, &BTreeMap::new(), &names)
        .await
        .expect("written");

    let body = std::fs::read_to_string(state.join("kage/kage/config.toml")).expect("the config");
    assert!(body.contains("[providers.anthropic]"), "{body}");
    assert!(body.contains("[providers.custom.gateway]"), "{body}");
    assert!(!body.contains("g-key"), "{body}");
}
