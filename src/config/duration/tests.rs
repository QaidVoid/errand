use super::parse_duration;

const HOUR: i64 = 60 * 60 * 1_000;
const DAY: i64 = 24 * HOUR;

#[test]
fn lengths_are_read_as_written() {
    assert_eq!(parse_duration("30d"), Some(30 * DAY));
    assert_eq!(parse_duration("7d 12h"), Some(7 * DAY + 12 * HOUR));
    assert_eq!(parse_duration("7d12h"), Some(7 * DAY + 12 * HOUR));
    assert_eq!(parse_duration("30m"), Some(30 * 60 * 1_000));
    assert_eq!(parse_duration("2w"), Some(14 * DAY));
    assert_eq!(parse_duration(" 1mo "), Some(30 * DAY));
    assert_eq!(parse_duration("1y"), Some(365 * DAY));
}

/// What is not a positive length is refused, never read as something close.
#[test]
fn what_is_not_a_positive_length_is_refused() {
    for text in [
        "", "  ", "-7d", "0s", "0d 0h", "30", "d", "3 days", "1.5d", "7x",
    ] {
        assert_eq!(parse_duration(text), None, "{text:?}");
    }
}
