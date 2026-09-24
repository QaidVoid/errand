//! Tests for reading a provider's `usage`.

use serde_json::json;

use super::{MappedUsage, ResetFormat, UsageShape};

#[test]
fn a_named_shape_is_read_by_name_and_nothing_means_unmetered() {
    assert_eq!(
        UsageShape::of("p", &json!({ "usage": "zai" })),
        Ok(Some(UsageShape::Zai))
    );
    assert_eq!(
        UsageShape::of("p", &json!({ "usage": "gateway" })),
        Ok(Some(UsageShape::Gateway))
    );
    assert_eq!(UsageShape::of("p", &json!({})), Ok(None));
    assert!(UsageShape::of("p", &json!({ "usage": "openai" })).is_err());
}

#[test]
fn a_mapping_says_where_the_numbers_are() {
    let shape = UsageShape::of(
        "p",
        &json!({ "usage": {
            "path": "/v1/usage",
            "auth": "raw",
            "percent": "limiting.remainingPercent",
            "percentIs": "left",
            "resets": "limiting.resetsAt",
            "resetsAs": "iso",
        } }),
    );

    assert_eq!(
        shape,
        Ok(Some(UsageShape::Mapped(MappedUsage {
            path: "/v1/usage".to_owned(),
            bearer: false,
            percent: "limiting.remainingPercent".to_owned(),
            percent_is_left: true,
            resets: Some(("limiting.resetsAt".to_owned(), ResetFormat::Iso)),
        })))
    );
}

/// Used and left read the window in opposite directions, so neither is
/// assumed, and every problem is named for the provider.
#[test]
fn a_mapping_that_could_be_misread_is_refused() {
    let problems = UsageShape::of(
        "gateway",
        &json!({ "usage": {
            "percent": "left",
            "resets": "at",
            "auth": "basic",
            "window": "5h",
        } }),
    )
    .expect_err("refused");

    assert_eq!(problems.len(), 4, "{problems:?}");
    assert!(
        problems
            .iter()
            .all(|problem| problem.starts_with("agent.providers.gateway.usage"))
    );
}
