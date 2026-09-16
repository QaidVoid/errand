//! Tests for size parsing, ported from `size_test.ts`.

use super::parse_size;

#[test]
fn a_size_is_read_as_bytes_with_or_without_a_suffix() {
    assert_eq!(parse_size("1024"), Some(1024));
    assert_eq!(parse_size("512m"), Some(512 * 1024_u64.pow(2)));
    assert_eq!(parse_size("4g"), Some(4 * 1024_u64.pow(3)));
    assert_eq!(
        parse_size("1.5g"),
        Some((1.5 * 1024_u64.pow(3) as f64).floor() as u64)
    );
}

#[test]
fn the_spellings_people_actually_use_all_work() {
    assert_eq!(parse_size("4G"), parse_size("4g"));
    assert_eq!(parse_size("4gb"), parse_size("4g"));
    assert_eq!(parse_size(" 4 g "), parse_size("4g"));
}

/// A limit that validates and is then not applied is worse than a refusal.
#[test]
fn anything_that_is_not_a_size_is_refused_rather_than_guessed() {
    assert_eq!(parse_size("four gigs"), None);
    assert_eq!(parse_size("12x"), None);
    assert_eq!(parse_size(""), None);
    assert_eq!(parse_size("-3g"), None);
}
