//! Tests for paths into a JSON answer.

use serde_json::json;

use super::{at, check};

#[test]
fn keys_walk_objects_and_a_pick_finds_a_list_entry_by_its_field() {
    let body = json!({
        "limiting": { "resetsAt": "soon" },
        "windows": [
            { "id": "window-share:1w", "usedPercent": 87 },
            { "id": "window-share:300m", "usedPercent": 0.1, "resetsAt": "2026-09-24T17:40:04.732Z" },
        ],
        "data": [{ "id": 7, "name": "seven" }],
    });

    assert_eq!(at(&body, "limiting.resetsAt"), Some(&json!("soon")));
    assert_eq!(
        at(&body, "windows[id=window-share:300m].usedPercent"),
        Some(&json!(0.1))
    );
    assert_eq!(
        at(&body, "windows[id=window-share:300m].resetsAt"),
        Some(&json!("2026-09-24T17:40:04.732Z"))
    );
    assert_eq!(at(&body, "data[id=7].name"), Some(&json!("seven")));
    assert_eq!(at(&body, "windows[id=window-share:1d].usedPercent"), None);
    assert_eq!(at(&body, "limiting.missing"), None);
}

#[test]
fn a_path_that_cannot_be_read_is_refused_with_how_to_write_one() {
    for broken in ["", "a..b", "a[id]", "a[=x]", "a[id=x", "a]b", "a[b[c=d]]"] {
        assert!(check(broken).is_err(), "{broken} should be refused");
    }
    assert!(check("windows[id=window-share:300m].usedPercent").is_ok());
}
