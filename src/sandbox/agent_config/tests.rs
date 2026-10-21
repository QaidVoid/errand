//! Tests for the kage provider configuration a sandbox reads.

use std::collections::BTreeMap;

use serde_json::json;

use super::{BrokeredProvider, kage_config, kage_model, write_agent_config};
use crate::config::schema::PluginConfig;
use crate::sandbox::backend::{Denials, SandboxLaunch};

/// A brokered provider kage knows is overridden in place: the broker's base
/// URL is what changes, and the key never enters the file.
///
/// The variable is named anyway. Kage reads every key from the environment
/// and registers no provider whose variable is unset, so a known provider
/// whose variable went unnamed would be dropped with nothing to say why.
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
    let config = kage_config(
        &serde_json::Map::new(),
        &brokered,
        &BTreeMap::new(),
        &names,
        &[],
        &Denials::default(),
    );

    assert!(config.contains("[providers.anthropic]"), "{config}");
    assert!(
        config.contains(r#"base_url = "http://169.254.169.1:8443/provider/anthropic""#),
        "{config}"
    );
    assert!(
        config.contains(r#"api_key_env = "ANTHROPIC_API_KEY""#),
        "{config}"
    );
    assert!(!config.contains("custom"), "{config}");
}

/// A known provider with no endpoint of its own is still written, since the
/// variable is the only thing kage needs to register it.
#[test]
fn a_known_provider_with_a_key_and_no_endpoint_is_written() {
    let names = BTreeMap::from([("zai".to_owned(), "ERRAND_PROVIDER_ZAI_API_KEY".to_owned())]);
    let mut defined = serde_json::Map::new();
    defined.insert("zai".to_owned(), json!({ "credential": "k" }));
    let config = kage_config(
        &defined,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &names,
        &[],
        &Denials::default(),
    );

    assert!(config.contains("[providers.zai]"), "{config}");
    assert!(
        config.contains(r#"api_key_env = "ERRAND_PROVIDER_ZAI_API_KEY""#),
        "{config}"
    );
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
    let config = kage_config(
        &serde_json::Map::new(),
        &brokered,
        &built_in,
        &names,
        &[],
        &Denials::default(),
    );

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
        json!({
            "baseUrl": "https://api.meta.example/v1",
            "credential": "real-meta-key-1",
            "models": [{ "id": "muse-spark-1.3" }],
        }),
    );
    let config = kage_config(
        &defined,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &[],
        &Denials::default(),
    );

    assert!(config.contains("[providers.custom.meta]"), "{config}");
    assert!(
        config.contains(r#"base_url = "https://api.meta.example/v1""#),
        "{config}"
    );
    assert!(!config.contains("real-meta-key-1"), "{config}");
}

/// A model entry carries what kage needs beyond the id: the name, and, when
/// said, the context window, the output limit, the inputs, and the thinking
/// levels. Unknown inputs and levels are dropped one by one rather than
/// refusing the model.
#[test]
fn a_model_entry_carries_its_sizes_inputs_and_levels() {
    let mut defined = serde_json::Map::new();
    defined.insert(
        "meta".to_owned(),
        json!({
            "baseUrl": "https://api.meta.example/v1",
            "models": [{
                "id": "muse-spark-1.3",
                "name": "Muse Spark",
                "contextWindow": 256_000,
                "maxTokens": 128_000,
                "input": ["text", "image", "smell-o-vision"],
                "reasoning": true,
                "efforts": ["low", "max", "overdrive"],
            }],
        }),
    );
    let config = kage_config(
        &defined,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &[],
        &Denials::default(),
    );

    assert!(config.contains(r#"id = "muse-spark-1.3""#), "{config}");
    assert!(config.contains(r#"name = "Muse Spark""#), "{config}");
    assert!(config.contains("context = 256000"), "{config}");
    assert!(config.contains("max_output = 128000"), "{config}");
    assert!(config.contains(r#"input = ["text", "image"]"#), "{config}");
    assert!(config.contains("reasoning = true"), "{config}");
    assert!(config.contains(r#"efforts = ["low", "max"]"#), "{config}");
    assert!(!config.contains("smell-o-vision"), "{config}");
    assert!(!config.contains("overdrive"), "{config}");
}

/// A model entry naming no sizes, inputs, or levels is written with the id
/// and the name alone: kage then decides what the model can do.
/// A model that draws says so, so the agent hands its pictures over; only
/// the kinds kage knows are written.
#[test]
fn a_model_entry_says_what_it_produces() {
    let table = kage_model(&json!({
        "id": "image:max",
        "input": ["text"],
        "produces": ["text", "image", "hologram"],
    }))
    .unwrap();
    assert!(
        table.contains("\nproduces = [\"text\", \"image\"]"),
        "{table}"
    );
    assert!(
        !kage_model(&json!({ "id": "m" }))
            .unwrap()
            .contains("produces")
    );
}

#[test]
fn a_bare_model_entry_writes_no_sizes_inputs_or_levels() {
    let mut defined = serde_json::Map::new();
    defined.insert(
        "meta".to_owned(),
        json!({
            "baseUrl": "https://api.meta.example/v1",
            "models": [{ "id": "muse-spark-1.3" }],
        }),
    );
    let config = kage_config(
        &defined,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &[],
        &Denials::default(),
    );

    let entry = config
        .split("[[providers.custom.meta.models]]")
        .nth(1)
        .expect("the model is written");
    assert!(entry.contains(r#"id = "muse-spark-1.3""#), "{entry}");
    assert!(!entry.contains("context ="), "{entry}");
    assert!(!entry.contains("max_output ="), "{entry}");
    assert!(!entry.contains("input ="), "{entry}");
    assert!(!entry.contains("reasoning ="), "{entry}");
    assert!(!entry.contains("efforts ="), "{entry}");
}

/// A model entry whose `maxTokens` names nothing kage accepts as an output
/// limit leaves it out: zero, or more than a `u32` holds.
#[test]
fn an_unusable_output_limit_is_left_out() {
    for max in [json!(0), json!(9_999_999_999_u64)] {
        let mut defined = serde_json::Map::new();
        defined.insert(
            "meta".to_owned(),
            json!({
                "baseUrl": "https://api.meta.example/v1",
                "models": [{ "id": "muse-spark-1.3", "maxTokens": max }],
            }),
        );
        let config = kage_config(
            &defined,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &[],
            &Denials::default(),
        );

        assert!(!config.contains("max_output ="), "{max}: {config}");
    }
}

/// A custom provider speaks `openai` unless its definition names another
/// protocol, so existing configurations without a `kind` keep working.
#[test]
fn a_custom_provider_without_a_kind_speaks_openai() {
    let mut defined = serde_json::Map::new();
    defined.insert(
        "meta".to_owned(),
        json!({
            "baseUrl": "https://api.meta.example/v1",
            "models": [{ "id": "muse-spark-1.3" }],
        }),
    );
    let config = kage_config(
        &defined,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &[],
        &Denials::default(),
    );

    assert!(config.contains("[providers.custom.meta]"), "{config}");
    assert!(config.contains("kind = \"openai\""), "{config}");
}

/// A custom provider naming `anthropic` is written as one, so kage speaks its
/// protocol rather than `openai`'s.
#[test]
fn a_custom_provider_naming_anthropic_is_written_anthropic() {
    let mut defined = serde_json::Map::new();
    defined.insert(
        "meta".to_owned(),
        json!({
            "baseUrl": "https://api.meta.example/v1",
            "kind": "anthropic",
            "models": [{ "id": "muse-spark-1.3" }],
        }),
    );
    let config = kage_config(
        &defined,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &[],
        &Denials::default(),
    );

    assert!(config.contains("[providers.custom.meta]"), "{config}");
    assert!(config.contains("kind = \"anthropic\""), "{config}");
    assert!(!config.contains("kind = \"openai\""), "{config}");
}

/// A provider kage already knows speaks its own protocol under its own id,
/// so a `kind` on its definition has nothing to say and is left out.
#[test]
fn a_kind_on_a_known_provider_is_left_out() {
    let mut defined = serde_json::Map::new();
    defined.insert(
        "anthropic".to_owned(),
        json!({
            "baseUrl": "https://api.meta.example/v1",
            "kind": "anthropic",
        }),
    );
    let names = BTreeMap::from([("anthropic".to_owned(), "ANTHROPIC_API_KEY".to_owned())]);
    let config = kage_config(
        &defined,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &names,
        &[],
        &Denials::default(),
    );

    assert!(config.contains("[providers.anthropic]"), "{config}");
    assert!(!config.contains("kind ="), "{config}");
}

/// A provider with no key, no endpoint, and no models has nothing to say and
/// no variable to name, so it is left out.
#[test]
fn a_provider_with_nothing_to_say_is_left_out() {
    let mut defined = serde_json::Map::new();
    defined.insert("plain".to_owned(), json!({ "kind": "openai" }));
    let config = kage_config(
        &defined,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &[],
        &Denials::default(),
    );

    assert!(!config.contains("plain"), "{config}");
}

/// A custom provider kage could not load is left out rather than written.
/// Kage refuses a whole configuration file whose custom provider is missing
/// an endpoint or its models, which would cost every other provider its
/// session too.
#[test]
fn a_custom_provider_kage_could_not_load_is_left_out() {
    let names = BTreeMap::from([(
        "gateway".to_owned(),
        "ERRAND_PROVIDER_GATEWAY_API_KEY".to_owned(),
    )]);
    let no_models = serde_json::Map::from_iter([(
        "gateway".to_owned(),
        json!({ "baseUrl": "https://gateway.example/v1", "credential": "k" }),
    )]);
    let no_endpoint = serde_json::Map::from_iter([(
        "gateway".to_owned(),
        json!({ "credential": "k", "models": [{ "id": "fast" }] }),
    )]);

    for (defined, case) in [(no_models, "no models"), (no_endpoint, "no endpoint")] {
        let config = kage_config(
            &defined,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &names,
            &[],
            &Denials::default(),
        );
        assert!(!config.contains("gateway"), "{case}: {config}");
    }
}

/// A configured plugin is copied into the directory kage reads plugins from,
/// beside the session's configuration, and its grant is written into that
/// configuration file.
#[tokio::test]
async fn a_plugin_is_copied_and_granted() {
    let root = tempfile::tempdir().expect("a temp dir");
    let source = root.path().join("zhipuai-coding-plan.lua");
    std::fs::write(&source, "-- plugin").expect("the plugin");
    let state = root.path().join("state");

    let launch = SandboxLaunch {
        state_dir: state.display().to_string(),
        plugins: vec![PluginConfig {
            path: source.display().to_string(),
            capabilities: vec!["env".to_owned(), "net".to_owned(), "crypto".to_owned()],
        }],
        ..SandboxLaunch::default()
    };
    write_agent_config(
        &launch,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .await
    .expect("written");

    let copied = std::fs::read_to_string(state.join("kage/kage/plugins/zhipuai-coding-plan.lua"))
        .expect("the plugin copied");
    assert_eq!(copied, "-- plugin");
    let body = std::fs::read_to_string(state.join("kage/kage/config.toml")).expect("the config");
    assert!(
        body.contains(
            "[plugins.capabilities]\nzhipuai-coding-plan = [\"env\", \"net\", \"crypto\"]"
        ),
        "{body}"
    );
}

/// The plugins directory is reset on every launch, so a plugin the
/// configuration no longer names stops loading.
#[tokio::test]
async fn a_removed_plugin_stops_loading() {
    let root = tempfile::tempdir().expect("a temp dir");
    let state = root.path().join("state");
    let leftover = state.join("kage/kage/plugins");
    std::fs::create_dir_all(&leftover).expect("the plugins directory");
    std::fs::write(leftover.join("gone.lua"), "-- gone").expect("the stale plugin");

    let launch = SandboxLaunch {
        state_dir: state.display().to_string(),
        ..SandboxLaunch::default()
    };
    write_agent_config(
        &launch,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .await
    .expect("written");

    assert!(!leftover.join("gone.lua").exists());
}

/// Two plugins copying to one name would have the second silently replace
/// the first, so the launch refuses instead.
#[tokio::test]
async fn two_plugins_copying_to_one_name_are_refused() {
    let root = tempfile::tempdir().expect("a temp dir");
    let first = root.path().join("first/one.lua");
    let second = root.path().join("second/one.lua");
    std::fs::create_dir_all(root.path().join("first")).expect("the first directory");
    std::fs::create_dir_all(root.path().join("second")).expect("the second directory");
    std::fs::write(&first, "-- first").expect("the first plugin");
    std::fs::write(&second, "-- second").expect("the second plugin");

    let launch = SandboxLaunch {
        state_dir: root.path().join("state").display().to_string(),
        plugins: vec![
            PluginConfig {
                path: first.display().to_string(),
                capabilities: Vec::new(),
            },
            PluginConfig {
                path: second.display().to_string(),
                capabilities: Vec::new(),
            },
        ],
        ..SandboxLaunch::default()
    };
    let error = write_agent_config(
        &launch,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .await
    .expect_err("a name taken twice is refused");
    assert!(error.0.contains("one.lua"), "{error:?}");
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
        json!({
            "baseUrl": "https://gateway.example/v1",
            "credential": "g-key",
            "models": [{ "id": "gateway-fast" }],
        }),
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

/// Each tool is written once. A tool named twice is a TOML parse error, and
/// the agent refuses the whole configuration rather than the one table, so a
/// denied tool that was also allowed above ends every launch.
#[test]
fn no_tool_is_written_twice_whatever_the_operator_denies() {
    let denied = Denials {
        commands: vec!["rm -rf *".to_owned()],
        tools: vec!["web_search".to_owned(), "write".to_owned()],
    };
    let config = kage_config(
        &serde_json::Map::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &[],
        &denied,
    );

    let mut seen: Vec<&str> = Vec::new();
    for line in config.lines() {
        let Some(name) = line.strip_prefix("[permissions.tools.") else {
            continue;
        };
        let name = name.trim_end_matches(']');
        assert!(!seen.contains(&name), "{name} is written twice:\n{config}");
        seen.push(name);
    }
    // A denied tool keeps its own table, and shell carries the denied
    // commands rather than being denied outright.
    let shell = config
        .split("[permissions.tools.shell]")
        .nth(1)
        .expect("shell is written")
        .split("\n[")
        .next()
        .expect("its table");
    assert!(!shell.contains("default"), "{shell}");
    assert!(shell.contains(r#"deny = ["rm -rf *"]"#), "{shell}");
    for tool in ["web_search", "write"] {
        let denied_tool = config
            .split(&format!("[permissions.tools.{tool}]"))
            .nth(1)
            .expect("the tool is written")
            .split("\n[")
            .next()
            .expect("its table");
        assert!(
            denied_tool.contains(r#"default = "deny""#),
            "{tool}: {denied_tool}"
        );
    }
}

/// Denying a tool outright leaves its commands unreached, so no table is
/// written twice for it either.
#[test]
fn denying_the_shell_tool_takes_no_command_patterns_with_it() {
    let denied = Denials {
        commands: vec!["rm -rf *".to_owned()],
        tools: vec!["shell".to_owned()],
    };
    let config = kage_config(
        &serde_json::Map::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &[],
        &denied,
    );

    assert_eq!(
        config.matches("[permissions.tools.shell]").count(),
        1,
        "{config}"
    );
    assert!(
        config.contains("[permissions.tools.shell]\ndefault = \"deny\""),
        "{config}"
    );
}

/// Under `--yolo` nothing needs allowing, so without denials no permission
/// is written, and several denied commands share one list.
#[test]
fn only_denials_are_written() {
    let config = kage_config(
        &serde_json::Map::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &[],
        &Denials::default(),
    );
    assert!(!config.contains("[permissions"), "{config}");

    let denied = Denials {
        commands: vec!["rm -rf *".to_owned(), "git push*".to_owned()],
        tools: Vec::new(),
    };
    let config = kage_config(
        &serde_json::Map::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &[],
        &denied,
    );
    assert!(
        config.contains("[permissions.tools.shell]\ndeny = [\"rm -rf *\", \"git push*\"]\n"),
        "{config}"
    );
}

/// The broker's route for one provider, as the backend hands it over.
fn through_broker(name: &str) -> BTreeMap<String, BrokeredProvider> {
    BTreeMap::from([(
        name.to_owned(),
        BrokeredProvider {
            base_url: format!("http://169.254.169.1:8443/provider/{name}"),
        },
    )])
}

/// A provider that serves images and no chat model is not told to the agent
/// as a provider, which would refuse it for listing no models and offer its
/// image models for chat. The image tool is pointed at the broker with the
/// nonce's variable instead, so the real endpoint and key stay outside.
#[test]
fn an_images_only_provider_is_written_for_the_image_tool_alone() {
    let mut defined = serde_json::Map::new();
    defined.insert(
        "pictures".to_owned(),
        json!({
            "credential": "k",
            "baseUrl": "https://images.example/v1",
            "images": { "model": "image" },
        }),
    );
    let names = BTreeMap::from([(
        "pictures".to_owned(),
        "ERRAND_PROVIDER_PICTURES_API_KEY".to_owned(),
    )]);
    let config = kage_config(
        &defined,
        &through_broker("pictures"),
        &BTreeMap::new(),
        &names,
        &[],
        &Denials::default(),
    );

    assert!(!config.contains("[providers"), "{config}");
    assert!(
        config.contains(
            "\n[tools.generate_image]\n\
             base_url = \"http://169.254.169.1:8443/provider/pictures\"\n\
             api_key_env = \"ERRAND_PROVIDER_PICTURES_API_KEY\"\n\
             model = \"image\"\n"
        ),
        "{config}"
    );
    assert!(!config.contains("images.example"), "{config}");
}

/// Outside the broker the tool reaches the provider's own endpoint, with
/// its key in the variable the operator named.
#[test]
fn an_unbrokered_images_provider_points_the_tool_at_its_endpoint() {
    let mut defined = serde_json::Map::new();
    defined.insert(
        "pictures".to_owned(),
        json!({
            "credential": "k",
            "credentialName": "PICTURES_KEY",
            "baseUrl": "https://images.example/v1",
            "images": { "model": "image" },
        }),
    );
    let names = BTreeMap::from([("pictures".to_owned(), "PICTURES_KEY".to_owned())]);
    let config = kage_config(
        &defined,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &names,
        &[],
        &Denials::default(),
    );

    assert!(
        config.contains(
            "\n[tools.generate_image]\n\
             base_url = \"https://images.example/v1\"\n\
             api_key_env = \"PICTURES_KEY\"\n\
             model = \"image\"\n"
        ),
        "{config}"
    );
    assert!(!config.contains("models = ["), "{config}");
}

/// A provider the agent already reaches for chat lends the image tool its
/// endpoint and key, whether kage knows it or errand registers it custom.
#[test]
fn a_chat_provider_carrying_images_lends_the_tool_its_endpoint() {
    let mut defined = serde_json::Map::new();
    defined.insert(
        "gateway".to_owned(),
        json!({
            "credential": "k",
            "baseUrl": "https://gateway.example/v1",
            "models": [{ "id": "chat-model" }],
            "images": { "model": "image" },
        }),
    );
    let names = BTreeMap::from([(
        "gateway".to_owned(),
        "ERRAND_PROVIDER_GATEWAY_API_KEY".to_owned(),
    )]);
    let config = kage_config(
        &defined,
        &through_broker("gateway"),
        &BTreeMap::new(),
        &names,
        &[],
        &Denials::default(),
    );
    assert!(config.contains("[providers.custom.gateway]"), "{config}");
    assert!(
        config.contains(
            "\n[tools.generate_image]\n\
             provider = \"gateway\"\n\
             model = \"image\"\n"
        ),
        "{config}"
    );

    let mut known = serde_json::Map::new();
    known.insert(
        "openai".to_owned(),
        json!({ "credential": "k", "images": { "model": "gpt-image-1" } }),
    );
    let names = BTreeMap::from([("openai".to_owned(), "OPENAI_API_KEY".to_owned())]);
    let config = kage_config(
        &known,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &names,
        &[],
        &Denials::default(),
    );
    assert!(config.contains("[providers.openai]"), "{config}");
    assert!(
        config
            .contains("\n[tools.generate_image]\nprovider = \"openai\"\nmodel = \"gpt-image-1\"\n"),
        "{config}"
    );
}

/// No `images` writes no table, and neither does a provider the agent could
/// not be pointed at: nothing tells it where the endpoint is.
#[test]
fn no_image_tool_without_images_or_an_endpoint() {
    let mut defined = serde_json::Map::new();
    defined.insert(
        "chat".to_owned(),
        json!({ "credential": "k", "baseUrl": "https://chat.example/v1", "models": [{ "id": "m" }] }),
    );
    let names = BTreeMap::from([("chat".to_owned(), "CHAT_KEY".to_owned())]);
    let config = kage_config(
        &defined,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &names,
        &[],
        &Denials::default(),
    );
    assert!(!config.contains("[tools.generate_image]"), "{config}");

    defined.insert(
        "nowhere".to_owned(),
        json!({ "credential": "k", "images": { "model": "image" } }),
    );
    let config = kage_config(
        &defined,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &names,
        &[],
        &Denials::default(),
    );
    assert!(!config.contains("[tools.generate_image]"), "{config}");
}
