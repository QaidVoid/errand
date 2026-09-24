//! Tests for reading a provider's `usage`.

use serde_json::json;

use super::{MappedUsage, MappedWindow, ResetFormat, UsageShape};

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
            windows: vec![MappedWindow {
                percent: "limiting.remainingPercent".to_owned(),
                percent_is_left: true,
                resets: Some(("limiting.resetsAt".to_owned(), ResetFormat::Iso)),
            }],
        })))
    );
}

#[test]
fn several_windows_are_read_in_the_order_written() {
    let shape = UsageShape::of(
        "p",
        &json!({ "usage": { "windows": [
            { "percent": "windows[id=5h].used", "percentIs": "used" },
            { "percent": "windows[id=1w].used", "percentIs": "used",
              "resets": "windows[id=1w].at", "resetsAs": "ms" },
        ] } }),
    )
    .expect("valid");

    let Some(UsageShape::Mapped(mapped)) = shape else {
        panic!("a mapping");
    };
    assert_eq!(mapped.windows.len(), 2);
    assert_eq!(mapped.windows[0].percent, "windows[id=5h].used");
    assert_eq!(
        mapped.windows[1].resets,
        Some(("windows[id=1w].at".to_owned(), ResetFormat::Millis))
    );
}

/// A window's fields belong in the window, and a path that cannot be read is
/// refused where it is written.
#[test]
fn a_window_list_is_refused_when_written_ambiguously() {
    let problems = UsageShape::of(
        "p",
        &json!({ "usage": {
            "percent": "left",
            "windows": [{ "percent": "a[b", "percentIs": "used", "label": "5h" }],
        } }),
    )
    .expect_err("refused");

    assert!(
        problems
            .iter()
            .any(|problem| problem.contains("belong in each of them")),
        "{problems:?}"
    );
    assert!(
        problems
            .iter()
            .any(|problem| problem.contains("windows[0].percent")),
        "{problems:?}"
    );
    assert!(
        problems
            .iter()
            .any(|problem| problem.contains("windows[0].label")),
        "{problems:?}"
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
