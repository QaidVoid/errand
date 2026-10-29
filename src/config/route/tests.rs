use serde_json::json;

use super::{Capabilities, Capability, Placement, RoutePolicy};

fn policy(definition: &serde_json::Value) -> RoutePolicy {
    RoutePolicy::of("p", definition).expect("valid")
}

fn refused(definition: &serde_json::Value) -> String {
    RoutePolicy::of("p", definition)
        .expect_err("refused")
        .join("\n")
}

fn capability(method: &str, path: &str) -> Capability {
    Capability {
        method: method.to_owned(),
        path: path.to_owned(),
    }
}

/// A declared placement wins; without one an explicit `kind` decides, and a
/// definition with neither, as every built-in is, keeps either header.
#[test]
fn placement_is_declared_or_derived_from_an_explicit_kind() {
    assert_eq!(policy(&json!({})).placement, Placement::Either);
    assert_eq!(
        policy(&json!({ "kind": "anthropic" })).placement,
        Placement::ApiKey
    );
    assert_eq!(
        policy(&json!({ "kind": "openai" })).placement,
        Placement::Bearer
    );
    assert_eq!(
        policy(&json!({ "kind": "openai", "placement": "x-api-key" })).placement,
        Placement::ApiKey
    );
    assert_eq!(
        policy(&json!({ "placement": "bearer" })).placement,
        Placement::Bearer
    );
    assert!(refused(&json!({ "placement": "cookie" })).contains("placement must be"));
}

/// Each state of the table: nothing, `"*"`, a list, and strict only over a
/// list. An empty list is refused rather than read as either extreme.
#[test]
fn capabilities_and_strictness_are_read_as_declared() {
    assert_eq!(policy(&json!({})).capabilities, Capabilities::Unset);
    assert_eq!(
        policy(&json!({ "capabilities": "*" })).capabilities,
        Capabilities::Any
    );
    let declared = policy(&json!({
        "capabilities": [{ "method": "POST", "path": "/v1/messages" }],
        "strictCapabilities": true,
    }));
    assert!(declared.strict);
    assert_eq!(
        declared.capabilities,
        Capabilities::Declared(vec![capability("POST", "/v1/messages")])
    );

    assert!(refused(&json!({ "capabilities": [] })).contains("is empty"));
    assert!(refused(&json!({ "strictCapabilities": true })).contains("needs a list"));
    assert!(
        refused(&json!({ "capabilities": "*", "strictCapabilities": true }))
            .contains("needs a list")
    );
    assert!(refused(&json!({ "strictCapabilities": "yes" })).contains("true or false"));
    assert!(refused(&json!({ "capabilities": "all" })).contains("must be \"*\" or a list"));
}

/// A capability's method and path are checked at load, so a typo is said
/// rather than matching nothing.
#[test]
fn a_malformed_capability_is_refused_at_load() {
    for (entry, says) in [
        (json!({ "method": "post", "path": "/v1" }), "upper-case"),
        (json!({ "method": "POST" }), "must be {"),
        (json!({ "method": "POST", "path": "v1" }), "start with /"),
        (
            json!({ "method": "POST", "path": "/v1//x" }),
            "empty segment",
        ),
        (json!({ "method": "POST", "path": "/v1/../x" }), ". or .."),
        (
            json!({ "method": "POST", "path": "/v1/mod*" }),
            "inside a segment",
        ),
        (json!({ "method": "POST", "path": "/v1?x=1" }), "query"),
        (json!({ "method": "POST", "path": "/v1/%2e" }), "escape"),
        (
            json!({ "method": "GET", "path": "/v1/models/{model}" }),
            "placeholder",
        ),
        (
            json!({ "method": "POST", "path": "/v1/**/x/**" }),
            "more than one **",
        ),
        (
            json!({ "method": "POST", "path": "/v1", "host": "x" }),
            "host is not something",
        ),
    ] {
        let problems = refused(&json!({ "capabilities": [entry] }));
        assert!(problems.contains(says), "{entry}: {problems}");
    }
}

/// `*` is exactly one segment and `**` any number, and the method is exact
/// unless it is `*`.
#[test]
fn a_glob_matches_by_whole_segments() {
    let one = capability("POST", "/v1/*/*/messages");
    assert!(one.matches("POST", "/v1/a/b/messages"));
    assert!(!one.matches("POST", "/v1/a/messages"));
    assert!(!one.matches("GET", "/v1/a/b/messages"));

    let many = capability("*", "/v1/**");
    assert!(many.matches("DELETE", "/v1"));
    assert!(many.matches("GET", "/v1/a/b/c"));
    assert!(!many.matches("GET", "/v2/a"));

    let between = capability("GET", "/v1/**/models");
    assert!(between.matches("GET", "/v1/models"));
    assert!(between.matches("GET", "/v1/x/y/models"));
    assert!(!between.matches("GET", "/v1/x/models/extra"));

    assert!(!capability("GET", "/v1/*").matches("GET", "/v1/"));
    assert!(!capability("POST", "/v1/messages").matches("POST", "/provider/v1/messages"));
    assert!(!capability("POST", "/v1/messages").matches("POST", "/v1/m%65ssages"));
}

/// Nothing declared, or `"*"`, allows every call; a list only what it names.
#[test]
fn only_a_declared_list_holds_a_call_back() {
    assert!(Capabilities::Unset.allow("DELETE", "/anything"));
    assert!(Capabilities::Any.allow("DELETE", "/anything"));
    let list = Capabilities::Declared(vec![
        capability("POST", "/v1/messages"),
        capability("GET", "/v1/models"),
    ]);
    assert!(list.allow("GET", "/v1/models"));
    assert!(!list.allow("DELETE", "/v1/models"));
}
