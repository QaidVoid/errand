//! Tests for reading kage's provider list.

use super::{KageProviders, as_installed};
use crate::sandbox::RunResult;

fn answered(code: i32, stdout: &str, stderr: &str) -> Result<KageProviders, String> {
    KageProviders::from_run(&RunResult {
        code,
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
    })
}

/// The list says, per id, which table errand writes it under.
#[test]
fn a_listed_id_says_which_table_it_takes() {
    let kage = as_installed();
    assert!(kage.overrides("openai"));
    assert!(kage.overrides("kenari"));
    assert!(!kage.reserves("openai"));
    assert!(kage.reserves("acp"));
    assert!(!kage.overrides("acp"));
    assert_eq!(
        kage.get("kenari-claude").map(|p| p.api.as_str()),
        Some("anthropic")
    );
}

/// An id kage does not list is neither overridden nor reserved, so it is
/// written as a custom provider. `openai-responses` is one kage dropped.
#[test]
fn an_unlisted_id_is_neither_overridden_nor_reserved() {
    let kage = as_installed();
    for id in ["openai-responses", "my-gateway"] {
        assert!(kage.get(id).is_none(), "{id}");
        assert!(!kage.overrides(id), "{id}");
        assert!(!kage.reserves(id), "{id}");
    }
}

/// A field kage adds later does not stop errand reading the list.
#[test]
fn a_field_errand_does_not_read_is_ignored() {
    let kage = answered(
        0,
        r#"{"providers":[{"id":"gw","api":"openai","override":true,"custom":true,"new":1}],"more":2}"#,
        "",
    )
    .expect("reads");
    assert!(kage.overrides("gw"));
}

/// A kage without the command says so on standard error and exits non-zero,
/// and the reason names the command and says to update.
#[test]
fn a_kage_without_the_command_is_refused_with_what_to_do() {
    let reason = answered(
        2,
        "",
        "error: unrecognized subcommand 'providers'\n\nFor more information, try '--help'.",
    )
    .expect_err("refused");
    assert!(
        reason.contains("`kage providers --json` exited 2"),
        "{reason}"
    );
    assert!(reason.contains("update kage"), "{reason}");
}

/// An answer that is not the list, or an empty one, is refused rather than
/// read as a kage that knows nothing.
#[test]
fn an_answer_that_is_not_a_list_is_refused() {
    for stdout in ["", "PROVIDER  API\nacp  acp", r#"{"providers":[]}"#] {
        let reason = answered(0, stdout, "").expect_err(stdout);
        assert!(
            reason.contains("`kage providers --json`"),
            "{stdout}: {reason}"
        );
    }
}

/// The operator's provider definitions, as validation hands them on.
fn defined(providers: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    providers.as_object().expect("an object").clone()
}

/// A provider named for an id kage reserves is refused, naming it.
#[test]
fn a_provider_named_for_a_reserved_id_is_refused() {
    let problems = as_installed().refuses(&defined(&serde_json::json!({
        "acp": { "credential": "k" },
        "gw": { "credential": "k" },
    })));
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("agent.providers.acp is named for a provider kage reserves"),
        "{problems:?}"
    );
}

/// The image tool cannot reach a kage provider that speaks the Anthropic API,
/// whatever it is called, including catalog ones a name list never caught.
#[test]
fn images_on_a_kage_provider_without_an_images_endpoint_are_refused() {
    for name in ["anthropic", "kenari-claude", "commandcode-claude"] {
        let problems = as_installed().refuses(&defined(&serde_json::json!({
            name: { "credential": "k", "images": { "model": "i" } },
        })));
        assert_eq!(problems.len(), 1, "{name}: {problems:?}");
        assert!(
            problems[0].contains(&format!(
                "agent.providers.{name}.images cannot use kage's {name} provider, which speaks \
                 the anthropic API"
            )),
            "{problems:?}"
        );
    }
}

/// The speech tool cannot reach a kage provider that speaks the Anthropic
/// API either, whatever it is called, including catalog ones a name list
/// never caught.
#[test]
fn speech_on_a_kage_provider_without_a_speech_endpoint_is_refused() {
    for name in ["anthropic", "kenari-claude", "commandcode-claude"] {
        let problems = as_installed().refuses(&defined(&serde_json::json!({
            name: { "credential": "k", "speech": { "model": "tts" } },
        })));
        assert_eq!(problems.len(), 1, "{name}: {problems:?}");
        assert!(
            problems[0].contains(&format!(
                "agent.providers.{name}.speech cannot use kage's {name} provider, which \
                 speaks the anthropic API"
            )),
            "{problems:?}"
        );
    }
}

/// `openai`, an OpenAI-compatible catalog provider, an id kage no longer
/// knows, and a gateway of the operator's own may all carry images, and a
/// provider a plugin registers is not held to kage's list.
#[test]
fn what_kage_takes_is_not_refused() {
    let problems = as_installed().refuses(&defined(&serde_json::json!({
        "openai": { "credential": "k", "images": { "model": "i" } },
        "kenari": { "credential": "k" },
        "openai-responses": { "credential": "k", "baseUrl": "https://o.example/v1" },
        "pictures": { "credential": "k", "baseUrl": "https://p.example/v1" },
        "acp": { "extension": true },
    })));
    assert!(problems.is_empty(), "{problems:?}");
}
