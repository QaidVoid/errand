//! Tests for reading a usage window where a definition says it is.

use serde_json::{Value, json};

use super::read_mapped;
use crate::config::usage::{MappedUsage, MappedWindow, ResetFormat};
use crate::provider::usage::Quota;

const NOW: i64 = 1_790_259_877_013;

fn window(id: &str) -> MappedWindow {
    MappedWindow {
        percent: format!("windows[id={id}].usedPercent"),
        percent_is_left: false,
        resets: Some((format!("windows[id={id}].resetsAt"), ResetFormat::Iso)),
    }
}

/// The five hour window first, then the weekly, as the gateway lists them.
fn gateway() -> MappedUsage {
    MappedUsage {
        path: "/usage".to_owned(),
        bearer: true,
        windows: vec![window("window-share:300m"), window("window-share:1w")],
    }
}

fn answer(five_hours: &Value, weekly: &Value) -> Value {
    json!({ "windows": [
        { "id": "window-share:1w", "usedPercent": weekly, "resetsAt": "2026-09-28T00:00:00.000Z" },
        { "id": "window-share:300m", "usedPercent": five_hours, "resetsAt": "2026-09-24T18:09:25.000Z" },
    ] })
}

#[test]
fn the_first_window_is_shown_while_none_is_spent() {
    assert_eq!(
        read_mapped(&answer(&json!(12.0), &json!(87.0)), &gateway(), NOW),
        Some(Quota {
            percentage: 12.0,
            resets_at: Some(1_790_273_365_000),
        })
    );
}

/// A spent weekly allowance leaves nothing for the five hour window to give,
/// so the provider reads as spent until the week rolls over.
#[test]
fn a_spent_longer_window_outranks_a_fresh_short_one() {
    assert_eq!(
        read_mapped(&answer(&json!(0.0), &json!(100.0)), &gateway(), NOW),
        Some(Quota {
            percentage: 100.0,
            resets_at: Some(1_790_553_600_000),
        })
    );
}

/// A window missing from the answer, or already past its reset, is skipped
/// for the next one rather than shown stale.
#[test]
fn a_missing_or_stale_window_falls_through_to_the_next() {
    let weekly_only = json!({ "windows": [
        { "id": "window-share:1w", "usedPercent": 40.0, "resetsAt": "2026-09-28T00:00:00.000Z" },
    ] });
    assert_eq!(
        read_mapped(&weekly_only, &gateway(), NOW).map(|quota| quota.percentage),
        Some(40.0)
    );

    let long_after = 1_790_300_000_000;
    assert_eq!(
        read_mapped(&answer(&json!(100.0), &json!(40.0)), &gateway(), long_after)
            .map(|quota| quota.percentage),
        Some(40.0),
        "a five hour window that has rolled over no longer counts as spent"
    );
}

#[test]
fn what_is_left_is_turned_into_what_is_used_in_the_unit_it_is_written() {
    let body =
        json!({ "limiting": { "left": 74.0, "ms": 2_000_000_000_000_i64, "s": 2_000_000_000 } });
    let mapped = |resets: (&str, ResetFormat)| MappedUsage {
        path: "/usage".to_owned(),
        bearer: false,
        windows: vec![MappedWindow {
            percent: "limiting.left".to_owned(),
            percent_is_left: true,
            resets: Some((resets.0.to_owned(), resets.1)),
        }],
    };

    let millis = read_mapped(&body, &mapped(("limiting.ms", ResetFormat::Millis)), NOW);
    let seconds = read_mapped(&body, &mapped(("limiting.s", ResetFormat::Seconds)), NOW);
    assert_eq!(millis.as_ref().map(|quota| quota.percentage), Some(26.0));
    assert_eq!(
        millis.and_then(|quota| quota.resets_at),
        Some(2_000_000_000_000)
    );
    assert_eq!(
        seconds.and_then(|quota| quota.resets_at),
        Some(2_000_000_000_000)
    );
}

/// A percentage that is not where the mapping says is no answer, not an
/// empty or a spent window.
#[test]
fn nothing_readable_is_no_answer() {
    assert_eq!(
        read_mapped(&json!({ "windows": [] }), &gateway(), NOW),
        None
    );
    assert_eq!(
        read_mapped(&answer(&json!("most"), &json!(null)), &gateway(), NOW),
        None
    );
}
