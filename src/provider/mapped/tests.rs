//! Tests for reading a usage window where a definition says it is.

use serde_json::json;

use super::read_mapped;
use crate::config::usage::{MappedUsage, ResetFormat};
use crate::provider::usage::Quota;

fn mapping(percent_is_left: bool, resets: Option<(&str, ResetFormat)>) -> MappedUsage {
    MappedUsage {
        path: "/usage".to_owned(),
        bearer: true,
        percent: "limiting.percent".to_owned(),
        percent_is_left,
        resets: resets.map(|(at, format)| (at.to_owned(), format)),
    }
}

#[test]
fn what_is_left_is_turned_into_what_is_used() {
    let body = json!({ "limiting": { "percent": 74.0, "resetsAt": "2026-09-24T04:48:00Z" } });

    assert_eq!(
        read_mapped(
            &body,
            &mapping(true, Some(("limiting.resetsAt", ResetFormat::Iso)))
        ),
        Some(Quota {
            percentage: 26.0,
            resets_at: Some(1_790_225_280_000),
        })
    );
    assert_eq!(
        read_mapped(&body, &mapping(false, None)),
        Some(Quota {
            percentage: 74.0,
            resets_at: None,
        })
    );
}

#[test]
fn a_reset_is_read_in_the_unit_it_is_written_in() {
    let body = json!({ "limiting": { "percent": 10.0, "ms": 1_000, "s": 2 } });

    let millis = read_mapped(
        &body,
        &mapping(false, Some(("limiting.ms", ResetFormat::Millis))),
    );
    let seconds = read_mapped(
        &body,
        &mapping(false, Some(("limiting.s", ResetFormat::Seconds))),
    );
    assert_eq!(millis.and_then(|quota| quota.resets_at), Some(1_000));
    assert_eq!(seconds.and_then(|quota| quota.resets_at), Some(2_000));
}

/// A percentage that is not where the mapping says is no answer, not an
/// empty or a spent window.
#[test]
fn a_missing_percentage_is_no_answer() {
    assert_eq!(
        read_mapped(&json!({ "limiting": {} }), &mapping(false, None)),
        None
    );
    assert_eq!(
        read_mapped(
            &json!({ "limiting": { "percent": "most" } }),
            &mapping(false, None)
        ),
        None
    );
}
