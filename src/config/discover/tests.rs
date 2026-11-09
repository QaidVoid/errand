//! Tests for reading a provider's `discover`.

use serde_json::json;

use super::Discovery;

#[test]
fn true_asks_the_openai_shape_and_false_or_nothing_asks_nothing() {
    let asked = Discovery::of("p", &json!({ "discover": true }))
        .expect("valid")
        .expect("asked");
    assert_eq!(asked.path, "/models");
    assert_eq!(asked.list, "data");
    assert_eq!(asked.fields["contextWindow"], "context_window");

    assert_eq!(Discovery::of("p", &json!({ "discover": false })), Ok(None));
    assert_eq!(Discovery::of("p", &json!({})), Ok(None));
}

#[test]
fn an_object_moves_the_list_and_renames_only_the_fields_it_names() {
    let asked = Discovery::of(
        "p",
        &json!({ "discover": {
            "path": "/api/models",
            "list": "result.models",
            "fields": { "contextWindow": "top_provider.context_length" },
            "defaults": { "reasoning": true },
        } }),
    )
    .expect("valid")
    .expect("asked");

    assert_eq!(asked.path, "/api/models");
    assert_eq!(asked.list, "result.models");
    assert_eq!(asked.fields["contextWindow"], "top_provider.context_length");
    assert_eq!(asked.fields["id"], "id");
    assert_eq!(asked.defaults["reasoning"], true);
}

#[test]
fn every_problem_is_named_for_the_provider() {
    let problems = Discovery::of(
        "gateway",
        &json!({ "discover": {
            "path": "models",
            "fields": { "cost": "price", "maxTokens": 7 },
            "limit": 3,
        } }),
    )
    .expect_err("refused");

    assert_eq!(problems.len(), 4, "{problems:?}");
    assert!(
        problems
            .iter()
            .all(|problem| problem.starts_with("agent.providers.gateway.discover"))
    );
    assert!(
        Discovery::of("p", &json!({ "discover": "yes" })).is_err(),
        "a word is not a setting"
    );
}

/// `everyMs` puts a provider on its own cadence; a shorter one than a minute
/// is refused, since a mistyped interval would hammer the endpoint forever.
#[test]
fn every_ms_names_the_cadence_and_a_minute_is_the_shortest() {
    let asked = Discovery::of("p", &json!({ "discover": { "everyMs": 3_600_000 } }))
        .expect("valid")
        .expect("asked");
    assert_eq!(asked.every_ms, Some(3_600_000));

    let plain = Discovery::of("p", &json!({ "discover": true }))
        .expect("valid")
        .expect("asked");
    assert_eq!(plain.every_ms, None, "the bare form carries no cadence");

    for every in [json!(59_999), json!(0), json!(-1), json!("hour")] {
        assert!(
            Discovery::of("p", &json!({ "discover": { "everyMs": every } })).is_err(),
            "{every} must be refused"
        );
    }
}
